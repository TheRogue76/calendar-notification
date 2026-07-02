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
| `main.rs` | Panic hook, rustls provider, config load, channel + thread setup, launches daemon |
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
cargo test             # recurrence + client encoding + parse_hex tests
cargo clippy           # keep at zero warnings
cargo fmt              # run before finishing
./target/release/calendar-notification
```

Config: `~/.config/calendar-notification/config.toml`. OAuth token cache:
`~/.local/share/calendar-notification/token.json`.

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
