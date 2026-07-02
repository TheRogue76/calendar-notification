# CLAUDE.md

Guidance for working in this repo. Read this before making changes.

## What this is

A single-binary Rust desktop app for **Ubuntu/GNOME (Wayland)**: a system-tray
Google Calendar reminder daemon plus an on-demand agenda / add-event widget.
User-facing docs live in `README.md` (build, OAuth setup, autostart). This file
is the engineering map.

## Architecture

**Two execution contexts in one process:**

1. **Main thread — iced daemon** (`app.rs`, launched from `main.rs`). Owns the
   winit event loop and the widget window. Runs in *daemon* mode, so it starts
   with **no window** and lives happily windowless (the tray opens the window on
   demand). `iced::daemon(boot, update, view).title().subscription().theme().run()`.

2. **Background thread — the engine** (`engine.rs`, spawned in `main.rs` on a
   dedicated multi-thread tokio runtime). Owns OAuth, the Google client, the
   sync poll loop, and the reminder scheduler — all merged into one
   `tokio::select!` loop. Also owns the tray (`tray.rs`, via `ksni`).

**They communicate over two channels only** (set up in `main.rs`):

- **engine → UI:** `tokio::mpsc::UnboundedReceiver<UiEvent>`, bridged into an
  iced `Subscription` via a `static OnceLock` in `app.rs` (see gotcha below).
- **UI → engine:** `tokio::mpsc::UnboundedSender<Command>` held in `App`.

This split is deliberate: it keeps `ksni` + OAuth + HTTP entirely off iced's
executor so they never contend. Don't call blocking or Google code from the UI
thread; send a `Command` instead. Don't touch iced state from the engine; send
a `UiEvent`.

```
main.rs ── spawns ──▶ engine thread (tokio) ── owns ──▶ GoogleClient, scheduler, ksni tray
   │                        ▲   │
   │   Command (mpsc)  ─────┘   └───── UiEvent (mpsc) ─────┐
   ▼                                                        ▼
iced daemon (main thread) ◀── Subscription bridge (static UI_RX) ──
```

## Module map

| File | Responsibility |
|---|---|
| `lib.rs` | `run()`: panic hook, rustls provider, config load, channel + thread setup, launches daemon. Exposes all modules so tests can reach them |
| `main.rs` | Thin wrapper: `fn main() { calendar_notification::run() }` |
| `config.rs` | TOML config at `~/.config/calendar-notification/config.toml`; XDG paths |
| `engine.rs` | `UiEvent`/`Command`/`CalendarView` types; sync + scheduler + command hub loop |
| `notify.rs` | `notify-rust` reminder wrapper (async) |
| `tray.rs` | `ksni` tray: menu, per-calendar submenu, sends `Command`s |
| `app.rs` | iced daemon: `App` state, `Message`, update/view/subscription, window toggle |
| `ui/agenda.rs` | Today's agenda list + calendar filter chips |
| `ui/add_event.rs` | Add-event form state, view, and `NewEvent` assembly |
| `ui/recurrence.rs` | Recurrence presets ↔ RRULE strings (unit-tested) |
| `google/auth.rs` | `yup-oauth2` InstalledFlow authenticator |
| `google/client.rs` | `CalendarHub` wrapper: list calendars/events, insert; domain conversion |
| `google/model.rs` | Domain types (`Calendar`, `Occurrence`, `NewEvent`, `ReminderRule`) — decoupled from generated API structs |

## Hard-won gotchas (don't regress these)

- **rustls needs exactly one crypto provider.** `main.rs` calls
  `rustls::crypto::ring::default_provider().install_default()`. `hyper-rustls`
  is pinned with `default-features = false` so it can't drag in `aws-lc-rs`
  alongside `ring` (two providers → panic on first TLS). Keep it to `ring` only.

- **notify-rust must use `show_async().await` in the engine loop.** The sync
  `.show()` spins up a blocking zbus runtime and panics ("Cannot start a runtime
  from within a runtime") inside our tokio context. `notify::show_reminder` is
  `async` for this reason.

- **Calendar ids must be percent-encoded before API calls.** `google-calendar3`
  substitutes ids into the URL path *without* encoding, so a `#` (every holiday
  calendar id has one) is parsed as a URL fragment and truncates the request.
  `google/client.rs::encode_calendar_id` handles `#`/`?`/`%`; use it for any new
  call that puts a calendar id in the path.

- **Stale/unreadable calendars.** `list_calendars` filters out `deleted` entries
  and non-`reader/writer/owner` access roles; `list_events` treats a per-calendar
  `404`/`notFound` as "skip quietly" (returns empty) rather than erroring, so a
  lingering subscription can't spam warnings every sync.

- **Recurrence expansion is server-side.** We call `single_events(true)`; Google
  expands recurring events (handling EXDATE/DST/per-instance edits). We do *not*
  expand RRULE client-side. The add-event form still *emits* RRULE strings.

- **iced 0.14 subscription bridge is a bare `fn`.** `Subscription::run` takes a
  `fn` pointer that can't capture, so the engine→UI receiver is handed over
  through a `static OnceLock<Mutex<Option<Receiver>>>` (`app.rs::UI_RX`) and
  drained inside `iced::stream::channel`. If you add another external event
  source, follow the same pattern.

- **Window ✕ hides, doesn't quit.** The widget window uses
  `exit_on_close_request: true`; in daemon mode that only closes the *window*
  (the "exit on last window" path is gated by `!is_daemon`). `close_events()`
  clears `App::widget` so the tray can reopen it.

- **Fail-fast panic policy.** A panic hook in `main.rs` logs then
  `process::exit(101)` so any panic on any thread takes the whole process down
  (systemd `Restart=on-failure` restarts it) rather than leaving a zombie with a
  live tray but a dead engine. Prefer converting fallible operations to
  `Result`/`Option`; don't `catch_unwind` for control flow.

## Build / test / run

```bash
cargo build            # or --release
cargo test             # unit (in-module) + integration (tests/integration.rs)
cargo clippy --tests   # keep at zero warnings (tests included)
cargo fmt              # run before finishing
./target/release/calendar-notification
```

Config: `~/.config/calendar-notification/config.toml`. OAuth token cache:
`~/.local/share/calendar-notification/token.json`.

## Testing & coverage

**Policy (important — hold yourself to this):** this project has a **≥80% line
coverage goal**, and it is currently met (~84%). Always strive to keep it there
or higher. **Any code you add should come with tests** — treat a change as
incomplete until its new logic is exercised. Before finishing a change:
run `cargo llvm-cov --summary-only`, and if your edit dropped coverage or added
untested logic, add tests until you're back at/above the goal. If something is
genuinely not unit-testable (needs live GUI/network/D-Bus/OAuth — see the
"Intentionally uncovered" list below), say so explicitly rather than skipping
silently; don't pad coverage with brittle mocks of those layers.

- **Unit tests** live in each module under `#[cfg(test)] mod tests` (they can
  reach private fns — that's where most coverage comes from). **Integration
  tests** in `tests/integration.rs` exercise the public API as an external
  consumer.
- **Coverage** (~84% line, run it before/after test changes):
  ```bash
  cargo llvm-cov --summary-only       # table
  cargo llvm-cov --open               # HTML report
  ```
- **Testability patterns already in place — reuse them:**
  - The engine is generic over the `CalendarSource` trait (`engine.rs`); tests
    inject a `FakeSource` to drive `resync`/`handle_command`/`run_loop` without
    network. `run()` builds the real `GoogleClient` (which impls the trait).
  - `Engine.config_path` is injectable so tests persist config to a `tempfile`
    tempdir instead of the real `~/.config`. Never let a test write there.
  - `Config::load_or_create_at` / `save_to` take an explicit path for the same
    reason — prefer them in tests over the XDG-resolving `load_or_create`/`save`.
  - iced `view()`/`update()` are plain functions — call them directly in tests
    (building the widget tree needs no GPU/event loop). Assert `update` side
    effects by draining the `Command`/`UiEvent` channels.
  - The tray's `menu()` and its `activate` closures are testable by building a
    `CalTray`, calling `menu()`, and invoking the boxed closures.
- **Intentionally uncovered** (needs live GUI / network / D-Bus / OAuth, not
  unit-testable): `lib.rs::run`, `google/auth.rs`, the `GoogleClient` network
  method bodies, `notify::show_reminder`, `main.rs`. Don't chase these with
  brittle mocks — verify them by running the app.

## Verifying changes

Much of the runtime path (tray, live calendar, notifications) needs real Google
OAuth credentials and an interactive browser consent, so it can't be fully
exercised headlessly. What you *can* verify without credentials:

- `cargo test` / `cargo clippy` / `cargo build`.
- First-run flow: with no `client_id`/`client_secret`, the binary prints the
  setup instructions and exits (doesn't launch the GUI).

For anything touching the live flow, describe what you changed and ask the user
to run it (they have credentials configured), or reason precisely from the code.
When unsure about an exact API of `iced` 0.14 / `ksni` 0.3 / `google-calendar3`
v7 / `yup-oauth2` 12, **check the installed source** under
`~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/` rather than
guessing — these crates drifted from their older documented APIs.

## Conventions

- Keep `clippy` clean and run `cargo fmt`.
- Domain types in `google/model.rs` stay decoupled from generated API structs;
  do conversions in `google/client.rs`.
- New UI interactions: add a `Message` variant in `app.rs`; if it's an action
  the engine performs, add a `Command` and handle it in `engine.rs`.
