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
    EventLoaded(std::result::Result<EventDetails, String>),
    /// Result of an edit request: Ok(event_id) or Err(message).
    EventUpdated(std::result::Result<String, String>),
    /// Result of a delete request: Ok(()) or Err(message).
    EventDeleted(std::result::Result<(), String>),
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

struct Engine<C: CalendarSource> {
    config: Config,
    client: C,
    ui_tx: UnboundedSender<UiEvent>,
    /// Live tray handle, used to refresh the calendar submenu.
    tray: Option<ksni::Handle<CalTray>>,
    /// Where to persist config. `None` = the default XDG path; tests inject a
    /// temp path so they never touch the user's real config.
    config_path: Option<PathBuf>,
    calendars: Vec<Calendar>,
    occurrences: Vec<Occurrence>,
    /// Dedup set of already-fired reminders (occurrence_key + minutes).
    fired: HashSet<String>,
}

impl<C: CalendarSource> Engine<C> {
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
        let client = &self.client;
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
        for (id, res) in results {
            match res {
                Ok(mut occs) => all.append(&mut occs),
                Err(e) => warn!("events fetch failed for {id}: {e:#}"),
            }
        }

        all.sort_by_key(|o| o.start);
        self.occurrences = all;
        self.prune_fired();
        self.emit(UiEvent::Occurrences(self.occurrences.clone()));
        self.publish_next_event().await;
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
            Command::LoadEvent {
                calendar_id,
                event_id,
            } => {
                let result = self
                    .client
                    .get_event(&calendar_id, &event_id)
                    .await
                    .map_err(|e| format!("{e:#}"));
                self.emit(UiEvent::EventLoaded(result));
            }
            Command::UpdateEvent {
                calendar_id,
                event_id,
                event,
            } => {
                let result = self
                    .client
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
                let result = self
                    .client
                    .delete_event(&calendar_id, &event_id)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let ok = result.is_ok();
                self.emit(UiEvent::EventDeleted(result));
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
pub async fn run<C: CalendarSource>(
    config: Config,
    client: C,
    ui_tx: UnboundedSender<UiEvent>,
    cmd_rx: UnboundedReceiver<Command>,
    tray: Option<ksni::Handle<CalTray>>,
) {
    let engine = Engine {
        config,
        client,
        ui_tx,
        tray,
        config_path: None,
        calendars: Vec::new(),
        occurrences: Vec::new(),
        fired: HashSet::new(),
    };
    run_loop(engine, cmd_rx).await;
}

/// The engine's event loop, split out so tests can drive a hand-built engine
/// (with a fake source and an injected config path).
async fn run_loop<C: CalendarSource>(
    mut engine: Engine<C>,
    mut cmd_rx: UnboundedReceiver<Command>,
) {
    let poll_every = TokioDuration::from_secs(
        engine
            .config
            .poll_interval_minutes
            .max(1)
            .saturating_mul(60),
    );

    engine.resync().await;

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

    fn engine_with(
        client: FakeSource,
        config: Config,
    ) -> (
        Engine<FakeSource>,
        UnboundedReceiver<UiEvent>,
        tempfile::TempDir,
    ) {
        let (ui_tx, ui_rx) = unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine {
            config,
            client,
            ui_tx,
            tray: None,
            config_path: Some(dir.path().join("config.toml")),
            calendars: Vec::new(),
            occurrences: Vec::new(),
            fired: HashSet::new(),
        };
        (engine, ui_rx, dir)
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
        assert_eq!(e.client.inserted.lock().unwrap().len(), 1);
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
            UiEvent::EventLoaded(Ok(d)) if d.event_id == "evt" && d.calendar_id == "p"
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
        let recorded = e.client.updated.lock().unwrap();
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
        let recorded = e.client.deleted.lock().unwrap();
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
}
