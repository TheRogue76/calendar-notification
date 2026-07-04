//! Background engine: owns the Google client and config, runs the sync poll
//! loop and the reminder scheduler in a single `tokio::select!` loop, and acts
//! as the command hub between the tray, the UI, and Google.
//!
//! It runs on a dedicated background tokio runtime (see `main.rs`) so it never
//! contends with iced's own executor — this is the plan's isolation strategy
//! for making ksni + iced coexist cleanly.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration as TokioDuration, Instant};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::google::model::{Calendar, EventDetails, NewEvent, Occurrence};
use crate::notify;
use crate::tray::{CalTray, NextEvent};

/// Reminders window: how far ahead we fetch occurrences for scheduling.
///
/// Known limitation: a reminder only fires if its *fire time*
/// (`event start − lead`) falls inside this window. A long lead on a distant
/// event (e.g. a "1 week before" reminder on an event 10 days out) is missed —
/// while the event is beyond the window it isn't fetched, and once it enters
/// the window the fire time is already in the past and gets skipped as stale.
/// Common minute/hour/1-day leads are unaffected. Widen this (at the cost of a
/// larger per-sync fetch) if longer leads need to be honoured.
const REMINDER_WINDOW_HOURS: i64 = 48;

/// Result message for a UI action that arrives before setup is complete.
const NOT_CONFIGURED: &str = "Not connected to Google yet — finish setup first.";

/// The calendar data source the engine talks to. Implemented by the real
/// `GoogleClient`; a fake implementation drives the engine in tests.
#[allow(async_fn_in_trait)]
pub trait CalendarSource {
    async fn list_calendars(&self) -> Result<Vec<Calendar>>;
    async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<Occurrence>>;
    async fn insert_event(&self, new: &NewEvent) -> Result<String>;
    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<EventDetails>;
    async fn update_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        ev: &NewEvent,
    ) -> Result<String>;
    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()>;
}

/// Builds a [`CalendarSource`] from the OAuth credentials in [`Config`]. This is
/// deferred (rather than done once at startup) so the app can run tray-only,
/// windowless, with *no* credentials yet and build the client on demand once the
/// user completes the in-app setup. Implemented by `GoogleAuthorizer`; a fake
/// implementation drives the setup flow in tests.
#[allow(async_fn_in_trait)]
pub trait Authorizer {
    type Source: CalendarSource;
    async fn authorize(&self, cfg: &Config) -> Result<Self::Source>;
}

/// Messages flowing from the engine to the UI (bridged into an iced subscription).
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// The set of calendars (with prefs applied) changed.
    Calendars(Vec<CalendarView>),
    /// The full occurrence set within the working window changed.
    Occurrences(Vec<Occurrence>),
    /// Toggle the agenda widget window open/closed (from the tray).
    ToggleWidget,
    /// Result of an add-event request: Ok(event_id) or Err(message).
    EventCreated(std::result::Result<String, String>),
    /// Full details of a selected event: Ok(details) or Err(message).
    /// Carries the requested `event_id` so the UI can drop a slow response
    /// that arrives after the user selected a different event.
    EventLoaded {
        event_id: String,
        result: std::result::Result<EventDetails, String>,
    },
    /// Result of an edit request: Ok(event_id) or Err(message).
    EventUpdated(std::result::Result<String, String>),
    /// Result of a delete request: Ok(()) or Err(message).
    EventDeleted(std::result::Result<(), String>),
    /// Open the credential-setup screen, prefilled with the current (possibly
    /// empty) client id/secret so the user can enter or fix them.
    OpenSetup {
        client_id: String,
        client_secret: String,
    },
    /// Result of a [`Command::SaveCredentials`] attempt: Ok(()) once the new
    /// credentials authorize and reach Google, or Err(message) to show inline.
    SetupResult(std::result::Result<(), String>),
    /// Human-readable status line (sync in progress, offline, etc.).
    Status(String),
    /// The user chose Quit.
    Quit,
}

/// A calendar plus the user's current prefs, sent to the UI.
#[derive(Debug, Clone)]
pub struct CalendarView {
    pub id: String,
    pub summary: String,
    pub color: String,
    pub primary: bool,
    pub visible: bool,
    pub notify: bool,
}

/// Commands flowing into the engine from the tray and the UI.
#[derive(Debug)]
pub enum Command {
    /// Force an immediate resync.
    SyncNow,
    /// Create a new event, then resync.
    InsertEvent(NewEvent),
    /// Fetch full details of an event for the detail pane / edit form.
    LoadEvent {
        calendar_id: String,
        event_id: String,
    },
    /// Patch an existing event (whole series for recurring), then resync.
    UpdateEvent {
        calendar_id: String,
        event_id: String,
        event: NewEvent,
    },
    /// Delete an event — a single instance or the whole series, depending on
    /// which `event_id` the caller resolved — then resync.
    DeleteEvent {
        calendar_id: String,
        event_id: String,
    },
    /// Set a calendar's agenda visibility.
    SetVisible(String, bool),
    /// Set whether a calendar fires reminders.
    SetNotify(String, bool),
    /// Forwarded to the UI to open/close the widget.
    ToggleWidget,
    /// Open the credential-setup screen (from the tray). The engine replies with
    /// a [`UiEvent::OpenSetup`] carrying the current credentials to prefill.
    Configure,
    /// Save freshly entered OAuth credentials, authorize, and (on success) start
    /// syncing. Replies with a [`UiEvent::SetupResult`].
    SaveCredentials {
        client_id: String,
        client_secret: String,
    },
    /// Shut everything down.
    Quit,
}

/// The next reminder due to fire, as chosen by [`Engine::next_reminder`].
#[derive(Debug, Clone)]
struct ScheduledReminder {
    /// When the reminder should fire (UTC).
    fire: DateTime<Utc>,
    /// Index into [`Engine::occurrences`] of the occurrence it belongs to.
    idx: usize,
    /// Lead time in minutes (for phrasing the notification).
    minutes: i64,
    /// Dedup key (`occurrence_key::minutes`) recorded once fired.
    key: String,
}

struct Engine<A: Authorizer> {
    config: Config,
    /// The live calendar source, built lazily once credentials are configured.
    /// `None` means "not configured yet" — the app runs tray-only until the user
    /// completes setup.
    client: Option<A::Source>,
    /// Builds `client` from the config's credentials on demand.
    authorizer: A,
    ui_tx: UnboundedSender<UiEvent>,
    /// Live tray handle, used to refresh the calendar submenu.
    tray: Option<ksni::Handle<CalTray>>,
    /// Where to persist config. `None` = the default XDG path; tests inject a
    /// temp path so they never touch the user's real config.
    config_path: Option<PathBuf>,
    /// Where the OAuth token cache lives (deleted when credentials change so a
    /// fresh consent runs). `None` = the default XDG path; tests inject a temp
    /// path so they never touch the user's real token cache.
    token_path: Option<PathBuf>,
    calendars: Vec<Calendar>,
    occurrences: Vec<Occurrence>,
    /// Dedup set of already-fired reminders (occurrence_key + minutes).
    fired: HashSet<String>,
}

impl<A: Authorizer> Engine<A> {
    /// Persist config to the injected path (tests) or the default XDG location.
    fn save_config(&self) {
        let result = match &self.config_path {
            Some(p) => self.config.save_to(p),
            None => self.config.save(),
        };
        if let Err(e) = result {
            warn!("could not persist config: {e:#}");
        }
    }

    fn emit(&self, ev: UiEvent) {
        // The UI may not be listening yet on early events; ignore send errors.
        let _ = self.ui_tx.send(ev);
    }

    /// Reflect configured/unconfigured state in the tray so it can switch
    /// between the reduced (Configure + Quit) and full menus.
    async fn set_tray_configured(&self, configured: bool) {
        if let Some(handle) = &self.tray {
            handle
                .update(move |t: &mut CalTray| t.configured = configured)
                .await;
        }
    }

    /// Best-effort delete of the OAuth token cache, so a credential change
    /// forces a fresh browser consent instead of reusing a stale refresh token.
    fn clear_token_cache(&self) {
        let path = match &self.token_path {
            Some(p) => p.clone(),
            None => match crate::config::token_path() {
                Ok(p) => p,
                Err(e) => {
                    warn!("could not resolve token path: {e:#}");
                    return;
                }
            },
        };
        // Just try the remove; a missing file is the expected case (fresh
        // install / already cleared), so don't warn on NotFound and don't bother
        // with a racy exists() pre-check.
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("could not clear token cache {}: {e:#}", path.display());
            }
        }
    }

    /// Persist freshly entered credentials, authorize, and — once we've actually
    /// reached Google (which triggers the first-run browser consent) — start
    /// syncing. On any failure the credentials stay saved and the error is sent
    /// back for the setup screen to show, without disturbing an existing client.
    async fn save_credentials(&mut self, client_id: String, client_secret: String) {
        let changed =
            client_id != self.config.client_id || client_secret != self.config.client_secret;
        self.config.client_id = client_id;
        self.config.client_secret = client_secret;
        self.save_config();
        if changed {
            self.clear_token_cache();
        }

        self.emit(UiEvent::Status("Connecting to Google…".into()));
        let client = match self.authorizer.authorize(&self.config).await {
            Ok(client) => client,
            Err(e) => return self.fail_setup(&e),
        };
        // Validating by actually listing calendars is what drives the OAuth
        // consent flow on first run; a failure here means bad credentials, a
        // denied consent, or no network.
        if let Err(e) = client.list_calendars().await {
            return self.fail_setup(&e);
        }

        self.client = Some(client);
        self.set_tray_configured(true).await;
        self.emit(UiEvent::SetupResult(Ok(())));
        self.resync().await;
    }

    /// Report a failed setup attempt: send the error to the setup screen and
    /// clear the transient "Connecting…" status, so the agenda doesn't stay
    /// stuck on it if the screen is closed (the setup screen swallows
    /// `SetupResult` once dismissed, but `Status` always applies).
    fn fail_setup(&self, err: &anyhow::Error) {
        self.emit(UiEvent::Status("Setup failed — not connected".into()));
        self.emit(UiEvent::SetupResult(Err(format!("{err:#}"))));
    }

    /// Publish the current calendar list (with prefs applied) to the UI and
    /// refresh the tray submenu.
    async fn publish_calendars(&self) {
        let views: Vec<CalendarView> = self
            .calendars
            .iter()
            .map(|c| {
                let prefs = self.config.calendars.get(&c.id);
                CalendarView {
                    id: c.id.clone(),
                    summary: c.summary.clone(),
                    color: prefs
                        .map(|p| p.color.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| c.color.clone()),
                    primary: c.primary,
                    visible: prefs.map(|p| p.visible).unwrap_or(true),
                    notify: prefs.map(|p| p.notify).unwrap_or(true),
                }
            })
            .collect();

        if let Some(handle) = &self.tray {
            let for_tray = views.clone();
            handle
                .update(move |t: &mut CalTray| t.calendars = for_tray)
                .await;
        }
        self.emit(UiEvent::Calendars(views));
    }

    /// Fetch calendars + occurrences for the working window and republish. A
    /// no-op until credentials are configured (no `client` yet).
    async fn resync(&mut self) {
        // Scope the client borrow to just the list call so the Ok arm below can
        // freely mutate `self` (config, calendars). No client -> not configured.
        let list = match self.client.as_ref() {
            Some(client) => {
                self.emit(UiEvent::Status("Syncing…".into()));
                client.list_calendars().await
            }
            None => return,
        };

        match list {
            Ok(cals) => {
                // Only touch disk when a genuinely new calendar appeared;
                // resync runs every poll and shouldn't rewrite config each time.
                let mut changed = false;
                for c in &cals {
                    changed |= self.config.ensure_calendar(&c.id, &c.color);
                }
                if changed {
                    self.save_config();
                }
                self.calendars = cals;
            }
            Err(e) => {
                warn!("calendar list failed (offline?): {e:#}");
                self.emit(UiEvent::Status("Offline — showing last sync".into()));
                return;
            }
        }
        self.publish_calendars().await;

        let now = Utc::now();
        let today_start = Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|dt| dt.and_local_timezone(Local).single())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let time_max = now + Duration::hours(REMINDER_WINDOW_HOURS);

        // Fetch every relevant calendar concurrently rather than serializing a
        // network round-trip per calendar (holiday/shared calendars add up).
        // Re-borrow the client (the borrow above couldn't be held across the
        // config/calendar mutations); degrade to a no-op rather than panic if it
        // somehow went away, matching the guard at the top of resync.
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let fetches = self.calendars.iter().filter_map(|c| {
            let prefs = self.config.calendars.get(&c.id);
            let visible = prefs.map(|p| p.visible).unwrap_or(true);
            let notify = prefs.map(|p| p.notify).unwrap_or(true);
            if !visible && !notify {
                return None; // no reason to fetch this calendar
            }
            Some(async move {
                let res = client.list_events(&c.id, today_start, time_max).await;
                (c.id.as_str(), res)
            })
        });
        let results = futures::future::join_all(fetches).await;

        let mut all = Vec::new();
        let mut failed = 0usize;
        for (id, res) in results {
            match res {
                Ok(mut occs) => all.append(&mut occs),
                Err(e) => {
                    // Keep this calendar's previous window: dropping it would
                    // wipe its agenda entries, scheduled reminders, and fired
                    // dedup keys over a transient per-calendar failure.
                    warn!("events fetch failed for {id}: {e:#}");
                    failed += 1;
                    all.extend(
                        self.occurrences
                            .iter()
                            .filter(|o| o.calendar_id == id)
                            .cloned(),
                    );
                }
            }
        }

        all.sort_by_key(|o| o.start);
        self.occurrences = all;
        self.prune_fired();
        self.emit(UiEvent::Occurrences(self.occurrences.clone()));
        self.publish_next_event().await;
        let stamp = Local::now().format("%H:%M");
        let status = if failed == 0 {
            format!("Synced {stamp}")
        } else {
            format!("Synced {stamp} — {failed} calendar(s) failed")
        };
        self.emit(UiEvent::Status(status));
        info!(
            "resync complete: {} occurrences ({failed} calendars failed)",
            self.occurrences.len()
        );
    }

    /// Drop dedup entries for occurrences that are no longer in the window.
    fn prune_fired(&mut self) {
        let live: HashSet<String> = self
            .occurrences
            .iter()
            .map(|o| o.occurrence_key())
            .collect();
        // A fired key is `{occurrence_key}::{minutes}`; strip the trailing
        // `::minutes` (the last separator) to recover the occurrence key and
        // test it against the live set directly — an O(1) lookup per key.
        self.fired.retain(|k| {
            k.rsplit_once("::")
                .is_some_and(|(occ_key, _minutes)| live.contains(occ_key))
        });
    }

    /// Which calendars are currently visible in the agenda.
    fn visible_calendars(&self) -> HashMap<String, bool> {
        self.calendars
            .iter()
            .map(|c| {
                let on = self
                    .config
                    .calendars
                    .get(&c.id)
                    .map(|p| p.visible)
                    .unwrap_or(true);
                (c.id.clone(), on)
            })
            .collect()
    }

    /// The soonest occurrence that hasn't ended yet, on a visible calendar —
    /// what the tray surfaces as "up next". `occurrences` is sorted by start, so
    /// the first still-live match is the nearest.
    fn next_upcoming(&self) -> Option<&Occurrence> {
        let now = Utc::now();
        let visible = self.visible_calendars();
        self.occurrences.iter().find(|o| {
            o.end.with_timezone(&Utc) > now && visible.get(&o.calendar_id).copied().unwrap_or(true)
        })
    }

    /// Push the "up next" event to the tray. The tray formats the relative time
    /// at render, so we only send the title + start.
    async fn publish_next_event(&self) {
        if let Some(handle) = &self.tray {
            let next = self.next_upcoming().map(|o| NextEvent {
                title: o.title.clone(),
                start: o.start,
            });
            handle
                .update(move |t: &mut CalTray| t.next_event = next)
                .await;
        }
    }

    /// Which calendars currently have notifications enabled.
    fn notify_enabled(&self) -> HashMap<String, bool> {
        self.calendars
            .iter()
            .map(|c| {
                let on = self
                    .config
                    .calendars
                    .get(&c.id)
                    .map(|p| p.notify)
                    .unwrap_or(true);
                (c.id.clone(), on)
            })
            .collect()
    }

    /// Find the earliest not-yet-fired reminder across all notify-enabled
    /// calendars.
    fn next_reminder(&self) -> Option<ScheduledReminder> {
        let notify = self.notify_enabled();
        let now = Utc::now();
        let mut best: Option<ScheduledReminder> = None;

        for (idx, occ) in self.occurrences.iter().enumerate() {
            if !notify.get(&occ.calendar_id).copied().unwrap_or(true) {
                continue;
            }
            for (fire, minutes) in occ.reminder_fire_times() {
                let key = format!("{}::{}", occ.occurrence_key(), minutes);
                if self.fired.contains(&key) {
                    continue;
                }
                // Skip reminders whose fire time is already well in the past
                // (more than 5 min stale) so a fresh start doesn't spam old ones.
                if fire < now - Duration::minutes(5) {
                    continue;
                }
                if best.as_ref().map(|b| fire < b.fire).unwrap_or(true) {
                    best = Some(ScheduledReminder {
                        fire,
                        idx,
                        minutes,
                        key,
                    });
                }
            }
        }
        best
    }

    async fn fire_reminder(&mut self, idx: usize, minutes: i64, key: String) {
        if let Some(occ) = self.occurrences.get(idx) {
            info!(
                "firing reminder for '{}' ({} min before)",
                occ.title, minutes
            );
            if let Err(e) = notify::show_reminder(occ, minutes).await {
                error!("notification failed: {e:#}");
            }
        }
        self.fired.insert(key);
    }

    async fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::SyncNow => self.resync().await,
            Command::ToggleWidget => self.emit(UiEvent::ToggleWidget),
            Command::SetVisible(id, v) => {
                self.config.calendars.entry(id).or_default().visible = v;
                self.save_config();
                self.publish_calendars().await;
                self.resync().await;
            }
            Command::SetNotify(id, v) => {
                self.config.calendars.entry(id).or_default().notify = v;
                self.save_config();
                self.publish_calendars().await;
            }
            Command::InsertEvent(new) => {
                let Some(client) = self.client.as_ref() else {
                    self.emit(UiEvent::EventCreated(Err(NOT_CONFIGURED.into())));
                    return true;
                };
                let result = client
                    .insert_event(&new)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let ok = result.is_ok();
                self.emit(UiEvent::EventCreated(result));
                if ok {
                    self.resync().await;
                }
            }
            Command::LoadEvent {
                calendar_id,
                event_id,
            } => {
                let Some(client) = self.client.as_ref() else {
                    self.emit(UiEvent::EventLoaded {
                        event_id,
                        result: Err(NOT_CONFIGURED.into()),
                    });
                    return true;
                };
                let result = client
                    .get_event(&calendar_id, &event_id)
                    .await
                    .map_err(|e| format!("{e:#}"));
                self.emit(UiEvent::EventLoaded { event_id, result });
            }
            Command::UpdateEvent {
                calendar_id,
                event_id,
                event,
            } => {
                let Some(client) = self.client.as_ref() else {
                    self.emit(UiEvent::EventUpdated(Err(NOT_CONFIGURED.into())));
                    return true;
                };
                let result = client
                    .update_event(&calendar_id, &event_id, &event)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let ok = result.is_ok();
                self.emit(UiEvent::EventUpdated(result));
                if ok {
                    self.resync().await;
                }
            }
            Command::DeleteEvent {
                calendar_id,
                event_id,
            } => {
                let Some(client) = self.client.as_ref() else {
                    self.emit(UiEvent::EventDeleted(Err(NOT_CONFIGURED.into())));
                    return true;
                };
                let result = client
                    .delete_event(&calendar_id, &event_id)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let ok = result.is_ok();
                self.emit(UiEvent::EventDeleted(result));
                if ok {
                    self.resync().await;
                }
            }
            Command::Configure => {
                self.emit(UiEvent::OpenSetup {
                    client_id: self.config.client_id.clone(),
                    client_secret: self.config.client_secret.clone(),
                });
            }
            Command::SaveCredentials {
                client_id,
                client_secret,
            } => self.save_credentials(client_id, client_secret).await,
            Command::Quit => {
                self.emit(UiEvent::Quit);
                return false;
            }
        }
        true
    }
}

/// Convert a UTC fire time to a tokio sleep deadline (immediate if in the past).
fn deadline_for(fire: DateTime<Utc>) -> Instant {
    let now = Utc::now();
    let delta = fire - now;
    let millis = delta.num_milliseconds().max(0) as u64;
    Instant::now() + TokioDuration::from_millis(millis)
}

/// Run the engine loop until a Quit command. Consumes the command receiver.
/// Starts with no client; if credentials are already configured it builds one,
/// otherwise it waits tray-only for the user to complete setup.
pub async fn run<A: Authorizer>(
    config: Config,
    authorizer: A,
    ui_tx: UnboundedSender<UiEvent>,
    cmd_rx: UnboundedReceiver<Command>,
    tray: Option<ksni::Handle<CalTray>>,
) {
    let engine = Engine {
        config,
        client: None,
        authorizer,
        ui_tx,
        tray,
        config_path: None,
        token_path: None,
        calendars: Vec::new(),
        occurrences: Vec::new(),
        fired: HashSet::new(),
    };
    run_loop(engine, cmd_rx).await;
}

/// The engine's event loop, split out so tests can drive a hand-built engine
/// (with a fake source and an injected config path).
async fn run_loop<A: Authorizer>(mut engine: Engine<A>, mut cmd_rx: UnboundedReceiver<Command>) {
    let poll_every = TokioDuration::from_secs(
        engine
            .config
            .poll_interval_minutes
            .max(1)
            .saturating_mul(60),
    );

    // Returning user: credentials are already saved, so build the client (its
    // token is cached, so no browser opens) and mark the tray configured before
    // the first sync. A fresh install has no credentials and stays tray-only —
    // `Command::Configure`/`SaveCredentials` drive setup from here. On the rare
    // startup authorize failure we stay unconfigured so the tray still offers a
    // way back into setup.
    if engine.config.has_credentials() {
        match engine.authorizer.authorize(&engine.config).await {
            Ok(client) => {
                engine.client = Some(client);
                engine.set_tray_configured(true).await;
                engine.resync().await;
            }
            Err(e) => warn!("startup authorization failed; awaiting reconfigure: {e:#}"),
        }
    }

    let mut poll = tokio::time::interval(poll_every);
    poll.tick().await; // consume the immediate first tick (we just synced)

    // Refresh the tray's "up next" label between syncs so relative times stay
    // fresh and events that have passed roll off without waiting for a poll.
    let mut tick = tokio::time::interval(TokioDuration::from_secs(60));
    tick.tick().await; // consume the immediate first tick

    loop {
        // Recompute the next reminder each iteration; state may have changed.
        let next = engine.next_reminder();
        // A single future for the reminder arm: it sleeps until the next fire
        // time, or — when nothing is scheduled — never resolves, so the arm
        // simply stays dormant. This avoids any unwrap/precondition coupling.
        let fire_at = next.as_ref().map(|r| deadline_for(r.fire));
        let reminder_ready = async move {
            match fire_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        if !engine.handle_command(cmd).await {
                            break;
                        }
                    }
                    None => break, // all senders dropped
                }
            }
            _ = poll.tick() => {
                engine.resync().await;
            }
            _ = tick.tick() => {
                engine.publish_next_event().await;
            }
            _ = reminder_ready => {
                if let Some(r) = next {
                    engine.fire_reminder(r.idx, r.minutes, r.key).await;
                }
            }
        }
    }

    info!("engine loop exited");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use crate::config::CalendarPrefs;
    use crate::google::model::ReminderRule;
    use chrono::TimeZone;
    use std::sync::Mutex;
    use tokio::sync::mpsc::unbounded_channel;

    // -- fake source ---------------------------------------------------------

    struct FakeSource {
        calendars: Vec<Calendar>,
        events: std::collections::HashMap<String, Vec<Occurrence>>,
        inserted: Mutex<Vec<NewEvent>>,
        updated: Mutex<Vec<(String, String, NewEvent)>>,
        deleted: Mutex<Vec<(String, String)>>,
        fail_calendars: bool,
        /// Calendar ids whose `list_events` call fails (per-calendar outage).
        fail_events: HashSet<String>,
    }

    impl FakeSource {
        fn new(calendars: Vec<Calendar>) -> Self {
            Self {
                calendars,
                events: std::collections::HashMap::new(),
                inserted: Mutex::new(Vec::new()),
                updated: Mutex::new(Vec::new()),
                deleted: Mutex::new(Vec::new()),
                fail_calendars: false,
                fail_events: HashSet::new(),
            }
        }
    }

    impl CalendarSource for FakeSource {
        async fn list_calendars(&self) -> Result<Vec<Calendar>> {
            if self.fail_calendars {
                anyhow::bail!("offline");
            }
            Ok(self.calendars.clone())
        }
        async fn list_events(
            &self,
            calendar_id: &str,
            _min: DateTime<Utc>,
            _max: DateTime<Utc>,
        ) -> Result<Vec<Occurrence>> {
            if self.fail_events.contains(calendar_id) {
                anyhow::bail!("events unavailable");
            }
            Ok(self.events.get(calendar_id).cloned().unwrap_or_default())
        }
        async fn insert_event(&self, new: &NewEvent) -> Result<String> {
            self.inserted.lock().unwrap().push(new.clone());
            Ok("new-id".into())
        }
        async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<EventDetails> {
            Ok(EventDetails {
                calendar_id: calendar_id.into(),
                event_id: event_id.into(),
                title: "Fetched".into(),
                location: None,
                description: None,
                all_day: false,
                start: Local::now(),
                end: Local::now(),
                attendees: vec![],
                recurrence: vec![],
            })
        }
        async fn update_event(
            &self,
            calendar_id: &str,
            event_id: &str,
            ev: &NewEvent,
        ) -> Result<String> {
            self.updated.lock().unwrap().push((
                calendar_id.to_string(),
                event_id.to_string(),
                ev.clone(),
            ));
            Ok(event_id.into())
        }
        async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()> {
            self.deleted
                .lock()
                .unwrap()
                .push((calendar_id.to_string(), event_id.to_string()));
            Ok(())
        }
    }

    // -- fake authorizer -----------------------------------------------------

    /// Stands in for `GoogleAuthorizer` in the setup flow: hands back a
    /// `FakeSource` (optionally one whose `list_calendars` fails, to exercise the
    /// validation-failure path) or fails outright.
    struct FakeAuthorizer {
        calendars: Vec<Calendar>,
        /// `authorize()` itself fails (e.g. building the authenticator).
        fail: bool,
        /// The produced source's `list_calendars` fails (validation failure).
        source_fail: bool,
    }

    impl FakeAuthorizer {
        fn ok(calendars: Vec<Calendar>) -> Self {
            Self {
                calendars,
                fail: false,
                source_fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                calendars: Vec::new(),
                fail: true,
                source_fail: false,
            }
        }
        fn source_failing() -> Self {
            Self {
                calendars: Vec::new(),
                fail: false,
                source_fail: true,
            }
        }
    }

    impl Authorizer for FakeAuthorizer {
        type Source = FakeSource;
        async fn authorize(&self, _cfg: &Config) -> Result<FakeSource> {
            if self.fail {
                anyhow::bail!("authorize failed");
            }
            let mut src = FakeSource::new(self.calendars.clone());
            src.fail_calendars = self.source_fail;
            Ok(src)
        }
    }

    // -- builders ------------------------------------------------------------

    fn cal(id: &str, primary: bool) -> Calendar {
        Calendar {
            id: id.into(),
            summary: id.to_uppercase(),
            color: "#112233".into(),
            primary,
        }
    }

    fn occ(cal_id: &str, start: DateTime<Local>, reminders: Vec<i64>) -> Occurrence {
        Occurrence {
            event_id: format!("evt-{cal_id}"),
            recurring_event_id: None,
            calendar_id: cal_id.into(),
            title: "T".into(),
            location: None,
            start,
            end: start,
            all_day: false,
            reminders: reminders
                .into_iter()
                .map(|m| ReminderRule { minutes: m })
                .collect(),
        }
    }

    #[allow(clippy::type_complexity)]
    fn build_engine(
        client: Option<FakeSource>,
        authorizer: FakeAuthorizer,
        config: Config,
    ) -> (
        Engine<FakeAuthorizer>,
        UnboundedReceiver<UiEvent>,
        tempfile::TempDir,
    ) {
        let (ui_tx, ui_rx) = unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine {
            config,
            client,
            authorizer,
            ui_tx,
            tray: None,
            config_path: Some(dir.path().join("config.toml")),
            token_path: Some(dir.path().join("token.json")),
            calendars: Vec::new(),
            occurrences: Vec::new(),
            fired: HashSet::new(),
        };
        (engine, ui_rx, dir)
    }

    /// A configured engine: the client is already present (the authorizer is
    /// unused). For the existing sync/command tests.
    fn engine_with(
        client: FakeSource,
        config: Config,
    ) -> (
        Engine<FakeAuthorizer>,
        UnboundedReceiver<UiEvent>,
        tempfile::TempDir,
    ) {
        build_engine(Some(client), FakeAuthorizer::ok(Vec::new()), config)
    }

    /// An unconfigured engine (no client yet) with a controllable authorizer,
    /// for the setup / `SaveCredentials` flow.
    fn setup_engine(
        authorizer: FakeAuthorizer,
        config: Config,
    ) -> (
        Engine<FakeAuthorizer>,
        UnboundedReceiver<UiEvent>,
        tempfile::TempDir,
    ) {
        build_engine(None, authorizer, config)
    }

    fn drain(rx: &mut UnboundedReceiver<UiEvent>) -> Vec<UiEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    // -- deadline_for --------------------------------------------------------

    #[test]
    fn deadline_for_past_is_immediate() {
        let past = Utc::now() - Duration::hours(1);
        let d = deadline_for(past);
        assert!(d <= Instant::now() + TokioDuration::from_millis(5));
    }

    #[test]
    fn deadline_for_future_is_later() {
        let future = Utc::now() + Duration::seconds(60);
        assert!(deadline_for(future) > Instant::now() + TokioDuration::from_secs(30));
    }

    // -- next_reminder / notify_enabled / prune_fired ------------------------

    #[test]
    fn next_reminder_none_when_empty() {
        let (e, _rx, _d) = engine_with(FakeSource::new(vec![]), Config::default());
        assert!(e.next_reminder().is_none());
    }

    #[test]
    fn next_reminder_picks_earliest_future_and_skips_stale_and_fired() {
        let (mut e, _rx, _d) = engine_with(FakeSource::new(vec![]), Config::default());
        e.calendars = vec![cal("p", true)];
        let now = Local::now();
        e.occurrences = vec![
            // stale: fired 10 min ago (start 9 min ago, 1 min lead) -> skipped
            occ("p", now - Duration::minutes(9), vec![1]),
            // future in 60 min (start +120, lead 60)
            occ("p", now + Duration::minutes(120), vec![60]),
            // sooner: future in 30 min (start +40, lead 10)
            occ("p", now + Duration::minutes(40), vec![10]),
        ];
        let r = e.next_reminder().expect("a reminder");
        assert_eq!(r.minutes, 10, "closest reminder wins");
        assert_eq!(r.idx, 2);

        // Once that key is fired, the next call returns the later one.
        e.fired.insert(r.key);
        assert_eq!(e.next_reminder().unwrap().minutes, 60);
    }

    #[test]
    fn next_reminder_skips_notify_disabled_calendar() {
        let mut config = Config::default();
        config.calendars.insert(
            "p".into(),
            CalendarPrefs {
                visible: true,
                notify: false,
                color: String::new(),
            },
        );
        let (mut e, _rx, _d) = engine_with(FakeSource::new(vec![]), config);
        e.calendars = vec![cal("p", true)];
        e.occurrences = vec![occ("p", Local::now() + Duration::hours(1), vec![10])];
        assert!(e.next_reminder().is_none());
    }

    #[test]
    fn prune_fired_drops_dead_keys() {
        let (mut e, _rx, _d) = engine_with(FakeSource::new(vec![]), Config::default());
        let live = occ("p", Local::now() + Duration::hours(1), vec![10]);
        let live_key = format!("{}::10", live.occurrence_key());
        e.occurrences = vec![live];
        e.fired.insert(live_key.clone());
        e.fired
            .insert("evt-gone::2026-01-01T00:00:00+00:00::5".into());
        e.prune_fired();
        assert!(e.fired.contains(&live_key));
        assert_eq!(e.fired.len(), 1);
    }

    // -- publish_calendars ---------------------------------------------------

    #[tokio::test]
    async fn publish_calendars_applies_prefs() {
        let mut config = Config::default();
        config.calendars.insert(
            "p".into(),
            CalendarPrefs {
                visible: false,
                notify: true,
                color: "#abcdef".into(),
            },
        );
        let (mut e, mut rx, _d) = engine_with(FakeSource::new(vec![]), config);
        e.calendars = vec![cal("p", true)];
        e.publish_calendars().await;
        let events = drain(&mut rx);
        match &events[0] {
            UiEvent::Calendars(v) => {
                assert_eq!(v[0].color, "#abcdef"); // config color overrides
                assert!(!v[0].visible);
            }
            other => panic!("expected Calendars, got {other:?}"),
        }
    }

    // -- resync --------------------------------------------------------------

    #[tokio::test]
    async fn resync_populates_and_persists() {
        let mut fake = FakeSource::new(vec![cal("p", true)]);
        fake.events.insert(
            "p".into(),
            vec![occ("p", Local::now() + Duration::hours(3), vec![10])],
        );
        let (mut e, mut rx, dir) = engine_with(fake, Config::default());
        e.resync().await;

        assert_eq!(e.calendars.len(), 1);
        assert_eq!(e.occurrences.len(), 1);
        assert!(
            dir.path().join("config.toml").exists(),
            "config persisted to temp path"
        );
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Calendars(_))));
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Occurrences(_))));
    }

    #[tokio::test]
    async fn resync_offline_emits_status_and_keeps_state() {
        let mut fake = FakeSource::new(vec![]);
        fake.fail_calendars = true;
        let (mut e, mut rx, _d) = engine_with(fake, Config::default());
        e.resync().await;
        assert!(e.calendars.is_empty());
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::Status(s) if s.contains("Offline"))));
    }

    #[tokio::test]
    async fn resync_partial_failure_keeps_previous_window_and_flags_status() {
        let mut fake = FakeSource::new(vec![cal("a", true), cal("b", false)]);
        let occ_a = occ("a", Local::now() + Duration::hours(1), vec![10]);
        let occ_b = occ("b", Local::now() + Duration::hours(2), vec![10]);
        fake.events.insert("a".into(), vec![occ_a]);
        fake.events.insert("b".into(), vec![occ_b.clone()]);
        let (mut e, mut rx, _d) = engine_with(fake, Config::default());

        // Healthy sync: both calendars present; mark b's reminder as fired.
        e.resync().await;
        assert_eq!(e.occurrences.len(), 2);
        let fired_b = format!("{}::10", occ_b.occurrence_key());
        e.fired.insert(fired_b.clone());
        drain(&mut rx);

        // b's events fetch now fails: its previous occurrences (and the fired
        // dedup key) must survive, and the status must say the sync was partial.
        e.client.as_mut().unwrap().fail_events.insert("b".into());
        e.resync().await;
        assert_eq!(e.occurrences.len(), 2, "b's window is retained");
        assert!(e
            .occurrences
            .iter()
            .any(|o| o.calendar_id == "b" && o.start == occ_b.start));
        assert!(e.fired.contains(&fired_b), "dedup key not pruned");
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::Status(s) if s.contains("1 calendar(s) failed"))));
    }

    // -- handle_command ------------------------------------------------------

    #[tokio::test]
    async fn handle_toggle_and_quit() {
        let (mut e, mut rx, _d) = engine_with(FakeSource::new(vec![]), Config::default());
        assert!(e.handle_command(Command::ToggleWidget).await);
        assert!(
            !e.handle_command(Command::Quit).await,
            "Quit stops the loop"
        );
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::ToggleWidget)));
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Quit)));
    }

    #[tokio::test]
    async fn handle_set_visible_updates_config() {
        let (mut e, _rx, _d) =
            engine_with(FakeSource::new(vec![cal("p", true)]), Config::default());
        e.handle_command(Command::SetVisible("p".into(), false))
            .await;
        assert!(!e.config.calendars["p"].visible);
        e.handle_command(Command::SetNotify("p".into(), false))
            .await;
        assert!(!e.config.calendars["p"].notify);
    }

    #[tokio::test]
    async fn handle_insert_event_records_and_reports() {
        let (mut e, mut rx, _d) =
            engine_with(FakeSource::new(vec![cal("p", true)]), Config::default());
        let new = NewEvent {
            calendar_id: "p".into(),
            title: "New".into(),
            location: None,
            description: None,
            all_day: false,
            start: Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap(),
            end: Local.with_ymd_and_hms(2026, 7, 2, 10, 0, 0).unwrap(),
            attendees: vec![],
            recurrence: vec![],
        };
        e.handle_command(Command::InsertEvent(new)).await;
        assert_eq!(e.client.as_ref().unwrap().inserted.lock().unwrap().len(), 1);
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventCreated(Ok(_)))));
    }

    #[tokio::test]
    async fn handle_load_event_emits_details() {
        let (mut e, mut rx, _d) =
            engine_with(FakeSource::new(vec![cal("p", true)]), Config::default());
        e.handle_command(Command::LoadEvent {
            calendar_id: "p".into(),
            event_id: "evt".into(),
        })
        .await;
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|ev| matches!(
            ev,
            UiEvent::EventLoaded { event_id, result: Ok(d) }
                if event_id == "evt" && d.event_id == "evt" && d.calendar_id == "p"
        )));
    }

    #[tokio::test]
    async fn handle_update_event_patches_and_resyncs() {
        let (mut e, mut rx, _d) =
            engine_with(FakeSource::new(vec![cal("p", true)]), Config::default());
        let event = NewEvent {
            calendar_id: "p".into(),
            title: "Edited".into(),
            location: None,
            description: None,
            all_day: false,
            start: Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap(),
            end: Local.with_ymd_and_hms(2026, 7, 2, 10, 0, 0).unwrap(),
            attendees: vec![],
            recurrence: vec![],
        };
        e.handle_command(Command::UpdateEvent {
            calendar_id: "p".into(),
            event_id: "master".into(),
            event,
        })
        .await;
        let client = e.client.as_ref().unwrap();
        let recorded = client.updated.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "p");
        assert_eq!(recorded[0].1, "master");
        drop(recorded);
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventUpdated(Ok(_)))));
        // Ok -> a resync follows, republishing occurrences.
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Occurrences(_))));
    }

    #[tokio::test]
    async fn handle_delete_event_deletes_and_resyncs() {
        let (mut e, mut rx, _d) =
            engine_with(FakeSource::new(vec![cal("p", true)]), Config::default());
        e.handle_command(Command::DeleteEvent {
            calendar_id: "p".into(),
            event_id: "instance-1".into(),
        })
        .await;
        let client = e.client.as_ref().unwrap();
        let recorded = client.deleted.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], ("p".to_string(), "instance-1".to_string()));
        drop(recorded);
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventDeleted(Ok(())))));
        // Ok -> a resync follows, republishing occurrences.
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Occurrences(_))));
    }

    // -- next_upcoming -------------------------------------------------------

    #[tokio::test]
    async fn next_upcoming_picks_first_live_visible_event() {
        let mut fake = FakeSource::new(vec![cal("p", true)]);
        let past = occ("p", Local::now() - Duration::hours(2), vec![]);
        let soon = occ("p", Local::now() + Duration::minutes(30), vec![]);
        let later = occ("p", Local::now() + Duration::hours(3), vec![]);
        // Insert out of order; resync sorts by start.
        fake.events
            .insert("p".into(), vec![later, past, soon.clone()]);
        let (mut e, _rx, _d) = engine_with(fake, Config::default());
        e.resync().await;
        let next = e.next_upcoming().expect("a live event");
        assert_eq!(next.start, soon.start);
    }

    #[tokio::test]
    async fn next_upcoming_skips_hidden_calendars() {
        let mut config = Config::default();
        config.calendars.entry("p".into()).or_default().visible = false;
        let mut fake = FakeSource::new(vec![cal("p", true)]);
        fake.events.insert(
            "p".into(),
            vec![occ("p", Local::now() + Duration::minutes(30), vec![])],
        );
        let (mut e, _rx, _d) = engine_with(fake, config);
        e.resync().await;
        assert!(e.next_upcoming().is_none());
    }

    // -- run_loop end-to-end -------------------------------------------------

    #[tokio::test]
    async fn run_loop_processes_commands_then_quits() {
        let mut fake = FakeSource::new(vec![cal("p", true)]);
        // Future reminder so the scheduler arm never fires (no real notification).
        fake.events.insert(
            "p".into(),
            vec![occ("p", Local::now() + Duration::hours(5), vec![10])],
        );
        let (e, mut ui_rx, _d) = engine_with(fake, Config::default());
        let (cmd_tx, cmd_rx) = unbounded_channel();
        cmd_tx.send(Command::SyncNow).unwrap();
        cmd_tx.send(Command::Quit).unwrap();
        run_loop(e, cmd_rx).await;
        let evs = drain(&mut ui_rx);
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Occurrences(_))));
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Quit)));
    }

    // -- setup / credentials flow --------------------------------------------

    fn sample_new_event() -> NewEvent {
        NewEvent {
            calendar_id: "p".into(),
            title: "New".into(),
            location: None,
            description: None,
            all_day: false,
            start: Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap(),
            end: Local.with_ymd_and_hms(2026, 7, 2, 10, 0, 0).unwrap(),
            attendees: vec![],
            recurrence: vec![],
        }
    }

    #[tokio::test]
    async fn resync_without_client_is_noop() {
        let (mut e, mut rx, _d) = setup_engine(FakeAuthorizer::ok(vec![]), Config::default());
        e.resync().await;
        assert!(e.calendars.is_empty());
        assert!(
            drain(&mut rx).is_empty(),
            "no UI events while unconfigured (not even a Syncing status)"
        );
    }

    #[tokio::test]
    async fn commands_without_client_report_not_configured() {
        let (mut e, mut rx, _d) = setup_engine(FakeAuthorizer::ok(vec![]), Config::default());
        e.handle_command(Command::InsertEvent(sample_new_event()))
            .await;
        e.handle_command(Command::LoadEvent {
            calendar_id: "p".into(),
            event_id: "x".into(),
        })
        .await;
        e.handle_command(Command::UpdateEvent {
            calendar_id: "p".into(),
            event_id: "x".into(),
            event: sample_new_event(),
        })
        .await;
        e.handle_command(Command::DeleteEvent {
            calendar_id: "p".into(),
            event_id: "x".into(),
        })
        .await;
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventCreated(Err(_)))));
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventLoaded { result: Err(_), .. })));
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventUpdated(Err(_)))));
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::EventDeleted(Err(_)))));
    }

    #[tokio::test]
    async fn configure_emits_open_setup_with_current_credentials() {
        let mut config = Config::default();
        config.client_id = "id".into();
        config.client_secret = "sec".into();
        let (mut e, mut rx, _d) = setup_engine(FakeAuthorizer::ok(vec![]), config);
        e.handle_command(Command::Configure).await;
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|ev| matches!(
            ev,
            UiEvent::OpenSetup { client_id, client_secret }
                if client_id == "id" && client_secret == "sec"
        )));
    }

    #[tokio::test]
    async fn save_credentials_authorizes_persists_and_syncs() {
        let (mut e, mut rx, dir) =
            setup_engine(FakeAuthorizer::ok(vec![cal("p", true)]), Config::default());
        assert!(e.client.is_none());
        e.handle_command(Command::SaveCredentials {
            client_id: "id".into(),
            client_secret: "secret".into(),
        })
        .await;

        assert!(e.client.is_some(), "client built after successful setup");
        assert!(e.config.has_credentials());
        assert!(
            dir.path().join("config.toml").exists(),
            "credentials persisted to the temp config path"
        );
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::SetupResult(Ok(())))));
        // A resync follows so the agenda is populated.
        assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Calendars(_))));
    }

    #[tokio::test]
    async fn save_credentials_authorize_failure_reports_and_stays_unconfigured() {
        let (mut e, mut rx, _d) = setup_engine(FakeAuthorizer::failing(), Config::default());
        e.handle_command(Command::SaveCredentials {
            client_id: "id".into(),
            client_secret: "secret".into(),
        })
        .await;
        assert!(e.client.is_none(), "no client when authorize fails");
        // Credentials are still saved so the user can retry from the screen.
        assert!(e.config.has_credentials());
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::SetupResult(Err(_)))));
        // The status must move off "Connecting…" so a closed screen doesn't
        // leave the agenda stuck showing it.
        assert!(matches!(evs.last(), Some(UiEvent::SetupResult(Err(_)))));
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::Status(s) if !s.contains("Connecting"))));
    }

    #[tokio::test]
    async fn save_credentials_validation_failure_reports_error() {
        let (mut e, mut rx, _d) = setup_engine(FakeAuthorizer::source_failing(), Config::default());
        e.handle_command(Command::SaveCredentials {
            client_id: "id".into(),
            client_secret: "secret".into(),
        })
        .await;
        assert!(
            e.client.is_none(),
            "credentials that can't reach Google don't become the live client"
        );
        let evs = drain(&mut rx);
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::SetupResult(Err(_)))));
        // Status is cleared off "Connecting…" on failure.
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, UiEvent::Status(s) if s.contains("failed"))));
    }
}
