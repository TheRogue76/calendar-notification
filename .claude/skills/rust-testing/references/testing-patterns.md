# Testing patterns — copyable snippets

All drawn from the current test suites. Match the nearest example when adding tests.

## Inline unit-test module skeleton
```rust
#[cfg(test)]
mod tests {
    use super::*;           // reach private items

    // -- section name ------------------------------------------------------
    #[test]
    fn describes_the_behavior() {
        // arrange / act / assert, with a message on the key assertion
        assert!(cond, "why this must hold");
    }
}
```
See `notify.rs:72`, `model.rs:56`, `config.rs:144`, `engine.rs:394`.

## Builder helpers (define once, reuse per test)
From `engine.rs`:
```rust
fn cal(id: &str, primary: bool) -> Calendar {
    Calendar { id: id.into(), summary: id.to_uppercase(), color: "#112233".into(), primary }
}

fn occ(cal_id: &str, start: DateTime<Local>, reminders: Vec<i64>) -> Occurrence {
    Occurrence {
        event_id: format!("evt-{cal_id}"), calendar_id: cal_id.into(),
        title: "T".into(), location: None, start, end: start, all_day: false,
        reminders: reminders.into_iter().map(|m| ReminderRule { minutes: m }).collect(),
    }
}

fn drain(rx: &mut UnboundedReceiver<UiEvent>) -> Vec<UiEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() { out.push(ev); }
    out
}
```

## Filesystem isolation with tempfile + injectable core
```rust
#[test]
fn load_or_create_creates_then_reads_roundtrip() {
    let dir = tempfile::tempdir().unwrap();          // auto-cleaned on drop
    let path = dir.path().join("nested/config.toml");
    let created = Config::load_or_create_at(&path).unwrap();   // core, not the XDG wrapper
    assert!(path.exists(), "default file should be created");
    // …mutate, save_to(&path), reload, assert round-trip…
}
```
Never call `Config::save()` / `load_or_create()` (the XDG-resolving wrappers) in a
test — always the `_at`/`_to` core with a temp path (`config.rs:204`).

## Mocking the network with a fake `CalendarSource`
```rust
struct FakeSource {
    calendars: Vec<Calendar>,
    events: HashMap<String, Vec<Occurrence>>,
    inserted: Mutex<Vec<NewEvent>>,
    fail_calendars: bool,
}

impl CalendarSource for FakeSource {
    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        if self.fail_calendars { anyhow::bail!("offline"); }   // simulate offline
        Ok(self.calendars.clone())
    }
    async fn list_events(&self, id: &str, _min: DateTime<Utc>, _max: DateTime<Utc>)
        -> Result<Vec<Occurrence>> {
        Ok(self.events.get(id).cloned().unwrap_or_default())
    }
    async fn insert_event(&self, new: &NewEvent) -> Result<String> {
        self.inserted.lock().unwrap().push(new.clone());       // record for asserting
        Ok("new-id".into())
    }
}
```
`engine.rs:405`. `tests/integration.rs` defines a *separate* external `FakeSource`
to prove the trait is publicly implementable by a downstream consumer.

## Building an `Engine` directly (bypass `run()`)
```rust
fn engine_with(client: FakeSource, config: Config)
    -> (Engine<FakeSource>, UnboundedReceiver<UiEvent>, tempfile::TempDir) {
    let (ui_tx, ui_rx) = unbounded_channel();
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine {
        config, client, ui_tx, tray: None,
        config_path: Some(dir.path().join("config.toml")),   // injected — no real config
        calendars: Vec::new(), occurrences: Vec::new(), fired: HashSet::new(),
    };
    (engine, ui_rx, dir)   // keep `dir` alive for the test's lifetime
}
```
Return the `TempDir` so it isn't dropped (and deleted) mid-test.

## Async tests + asserting on channel output
```rust
#[tokio::test]
async fn resync_populates_and_persists() {
    let mut fake = FakeSource::new(vec![cal("p", true)]);
    fake.events.insert("p".into(), vec![occ("p", Local::now() + Duration::hours(3), vec![10])]);
    let (mut e, mut rx, dir) = engine_with(fake, Config::default());
    e.resync().await;

    assert_eq!(e.occurrences.len(), 1);
    assert!(dir.path().join("config.toml").exists(), "config persisted to temp path");
    let evs = drain(&mut rx);
    assert!(evs.iter().any(|ev| matches!(ev, UiEvent::Occurrences(_))));
}
```

## Driving the whole loop to completion
Queue commands (ending in `Quit`) before awaiting `run_loop`, so it processes them
and exits deterministically without a real timer/notification firing
(`engine.rs:693`):
```rust
let (cmd_tx, cmd_rx) = unbounded_channel();
cmd_tx.send(Command::SyncNow).unwrap();
cmd_tx.send(Command::Quit).unwrap();
run_loop(e, cmd_rx).await;
```

## iced view/update are plain functions — test them directly
`app.rs`'s `update`/`view` take no GPU/event loop; call them and assert side effects
by draining the `Command`/`UiEvent` channels (see the pattern note in `CLAUDE.md`).

## Integration test shape (`tests/integration.rs`)
Import through the public crate path and test cross-module contracts:
```rust
use calendar_notification::config::Config;
use calendar_notification::engine::CalendarSource;
use calendar_notification::google::model::{Calendar, NewEvent, Occurrence, ReminderRule};
```
Guard the *public surface*; leave fine-grained logic to the in-module unit tests.
