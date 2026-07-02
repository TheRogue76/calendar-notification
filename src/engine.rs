//! Background engine: owns the Google client and config, runs the sync poll
//! loop and the reminder scheduler in a single `tokio::select!` loop, and acts
//! as the command hub between the tray, the UI, and Google.
//!
//! It runs on a dedicated background tokio runtime (see `main.rs`) so it never
//! contends with iced's own executor — this is the plan's isolation strategy
//! for making ksni + iced coexist cleanly.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Local, Utc};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration as TokioDuration, Instant};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::google::client::GoogleClient;
use crate::google::model::{Calendar, NewEvent, Occurrence};
use crate::notify;
use crate::tray::CalTray;

/// Reminders window: how far ahead we fetch occurrences for scheduling.
const REMINDER_WINDOW_HOURS: i64 = 48;

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
    /// Set a calendar's agenda visibility.
    SetVisible(String, bool),
    /// Set whether a calendar fires reminders.
    SetNotify(String, bool),
    /// Forwarded to the UI to open/close the widget.
    ToggleWidget,
    /// Shut everything down.
    Quit,
}

struct Engine {
    config: Config,
    client: GoogleClient,
    ui_tx: UnboundedSender<UiEvent>,
    /// Live tray handle, used to refresh the calendar submenu.
    tray: Option<ksni::Handle<CalTray>>,
    calendars: Vec<Calendar>,
    occurrences: Vec<Occurrence>,
    /// Dedup set of already-fired reminders (occurrence_key + minutes).
    fired: HashSet<String>,
}

impl Engine {
    fn emit(&self, ev: UiEvent) {
        // The UI may not be listening yet on early events; ignore send errors.
        let _ = self.ui_tx.send(ev);
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

    /// Fetch calendars + occurrences for the working window and republish.
    async fn resync(&mut self) {
        self.emit(UiEvent::Status("Syncing…".into()));

        match self.client.list_calendars().await {
            Ok(cals) => {
                for c in &cals {
                    self.config.ensure_calendar(&c.id, &c.color);
                }
                if let Err(e) = self.config.save() {
                    warn!("could not persist config: {e:#}");
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

        let mut all = Vec::new();
        for c in &self.calendars {
            let prefs = self.config.calendars.get(&c.id);
            let visible = prefs.map(|p| p.visible).unwrap_or(true);
            let notify = prefs.map(|p| p.notify).unwrap_or(true);
            if !visible && !notify {
                continue; // no reason to fetch this calendar
            }
            match self.client.list_events(&c.id, today_start, time_max).await {
                Ok(mut occs) => all.append(&mut occs),
                Err(e) => warn!("events fetch failed for {}: {e:#}", c.id),
            }
        }

        all.sort_by_key(|o| o.start);
        self.occurrences = all;
        self.prune_fired();
        self.emit(UiEvent::Occurrences(self.occurrences.clone()));
        self.emit(UiEvent::Status(format!(
            "Synced {}",
            Local::now().format("%H:%M")
        )));
        info!("resync complete: {} occurrences", self.occurrences.len());
    }

    /// Drop dedup entries for occurrences that are no longer in the window.
    fn prune_fired(&mut self) {
        let live: HashSet<String> = self
            .occurrences
            .iter()
            .map(|o| o.occurrence_key())
            .collect();
        self.fired
            .retain(|k| live.iter().any(|live_key| k.starts_with(live_key)));
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
    /// calendars. Returns (fire_time, occurrence index, minutes, dedup key).
    fn next_reminder(&self) -> Option<(DateTime<Utc>, usize, i64, String)> {
        let notify = self.notify_enabled();
        let now = Utc::now();
        let mut best: Option<(DateTime<Utc>, usize, i64, String)> = None;

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
                if best.as_ref().map(|(bf, ..)| fire < *bf).unwrap_or(true) {
                    best = Some((fire, idx, minutes, key));
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
                let _ = self.config.save();
                self.publish_calendars().await;
                self.resync().await;
            }
            Command::SetNotify(id, v) => {
                self.config.calendars.entry(id).or_default().notify = v;
                let _ = self.config.save();
                self.publish_calendars().await;
            }
            Command::InsertEvent(new) => {
                let result = self
                    .client
                    .insert_event(&new)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let ok = result.is_ok();
                self.emit(UiEvent::EventCreated(result));
                if ok {
                    self.resync().await;
                }
            }
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
pub async fn run(
    config: Config,
    client: GoogleClient,
    ui_tx: UnboundedSender<UiEvent>,
    mut cmd_rx: UnboundedReceiver<Command>,
    tray: Option<ksni::Handle<CalTray>>,
) {
    let poll_every =
        TokioDuration::from_secs(config.poll_interval_minutes.max(1).saturating_mul(60));
    let mut engine = Engine {
        config,
        client,
        ui_tx,
        tray,
        calendars: Vec::new(),
        occurrences: Vec::new(),
        fired: HashSet::new(),
    };

    engine.resync().await;

    let mut poll = tokio::time::interval(poll_every);
    poll.tick().await; // consume the immediate first tick (we just synced)

    loop {
        // Recompute the next reminder each iteration; state may have changed.
        let next = engine.next_reminder();
        // A single future for the reminder arm: it sleeps until the next fire
        // time, or — when nothing is scheduled — never resolves, so the arm
        // simply stays dormant. This avoids any unwrap/precondition coupling.
        let fire_at = next.as_ref().map(|(fire, ..)| deadline_for(*fire));
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
            _ = reminder_ready => {
                if let Some((_, idx, minutes, key)) = next {
                    engine.fire_reminder(idx, minutes, key).await;
                }
            }
        }
    }

    info!("engine loop exited");
}
