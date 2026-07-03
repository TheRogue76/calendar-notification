# calendar-notification

[![CI](https://github.com/TheRogue76/calendar-notification/actions/workflows/ci.yml/badge.svg)](https://github.com/TheRogue76/calendar-notification/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/TheRogue76/calendar-notification/branch/main/graph/badge.svg)](https://codecov.io/gh/TheRogue76/calendar-notification)

A lightweight **Google Calendar** companion for Ubuntu / GNOME (Wayland):

- **System-tray reminder daemon** — lives in the tray and pops a desktop
  notification before each event, honouring the event's own Google reminder
  settings (falling back to the calendar's defaults).
- **On-screen agenda widget** — toggled from the tray; shows today's events
  colour-coded per calendar, with a full add-event form (all-day, start/end,
  location, description, guests, recurrence presets, calendar picker).

Single Rust binary, single process. The UI runs on [`iced`](https://iced.rs)
in daemon mode; the tray uses [`ksni`](https://crates.io/crates/ksni) (pure
D-Bus StatusNotifierItem — no GTK), notifications use
[`notify-rust`](https://crates.io/crates/notify-rust), and calendar access uses
`google-calendar3` + `yup-oauth2` over rustls (no OpenSSL).

## Requirements

- Ubuntu 24.04+ / GNOME with the **`ubuntu-appindicators`** extension enabled
  (needed for the tray icon on GNOME Shell).
- A Rust toolchain (`cargo`) to build.

## Build & install

```bash
cargo build --release
install -Dm755 target/release/calendar-notification ~/.local/bin/calendar-notification
```

Make sure `~/.local/bin` is on your `PATH`.

## Google Cloud OAuth setup (one time)

The app talks to your calendar with your own OAuth **Desktop-app** client. You
create it once in the Google Cloud Console:

1. Go to <https://console.cloud.google.com/> and **create a project**.
2. **APIs & Services → Library →** enable the **Google Calendar API**.
3. **APIs & Services → OAuth consent screen →** choose **External**, fill in the
   minimal app details, and under **Test users** add your own Google account.
   (Staying in "Testing" mode avoids Google's app-verification requirement.)
4. **APIs & Services → Credentials → Create credentials → OAuth client ID →**
   choose **Desktop app**. Copy the **Client ID** and **Client secret**.
5. Run the app once to generate the config file, then paste the two values in:

   ```bash
   calendar-notification          # prints the config path and exits on first run
   ```

   Edit `~/.config/calendar-notification/config.toml`:

   ```toml
   client_id = "xxxxxxxx.apps.googleusercontent.com"
   client_secret = "xxxxxxxxxxxxxxxxx"
   poll_interval_minutes = 5
   ```

6. Run it again. A browser window opens for consent; approve it. The refresh
   token is cached at `~/.local/share/calendar-notification/token.json` and
   reused automatically on later runs.

## Run

```bash
calendar-notification
```

A calendar icon appears in the tray. Its menu:

- **Show / hide widget** — toggle the agenda window.
- **Sync now** — force a refresh.
- **Calendars** — per-calendar **Visible in agenda** / **Notify** toggles.
- **Quit**.

Closing the widget with its titlebar **✕** just hides it — the daemon (tray +
reminders) keeps running, and **Show / hide widget** reopens it.

## Autostart on login (systemd user service)

```bash
install -Dm644 systemd/calendar-notification.service \
    ~/.config/systemd/user/calendar-notification.service
systemctl --user daemon-reload
systemctl --user enable --now calendar-notification
```

Check status / logs:

```bash
systemctl --user status calendar-notification
journalctl --user -u calendar-notification -f
```

## App icon (dock / launcher)

The tray indicator draws its own colored calendar icon, but the **widget
window's** icon in the GNOME dock/launcher comes from a `.desktop` file: on
Wayland the compositor matches the window (via its `application_id`) to an
installed desktop entry and uses that entry's `Icon=`. Without it you get a
generic fallback. Install the icon + desktop entry so the dock shows the
calendar:

```bash
install -Dm644 assets/calendar-notification.svg \
    ~/.local/share/icons/hicolor/scalable/apps/calendar-notification.svg
install -Dm644 assets/calendar-notification.desktop \
    ~/.local/share/applications/calendar-notification.desktop
update-icon-caches ~/.local/share/icons/hicolor 2>/dev/null || true
```

Log out/in (or restart GNOME Shell) if the dock doesn't pick it up immediately.
The desktop entry's `StartupWMClass` matches the window `application_id`
(`calendar-notification`), which is how the match is made.

## Continuous integration

Every pull request runs [`.github/workflows/ci.yml`](.github/workflows/ci.yml):

- **Test & lint** — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
- **Coverage** — `cargo llvm-cov` uploads to [Codecov](https://about.codecov.io/).
  Codecov posts a coverage comment on the PR and a **patch** status check that
  fails when the PR's *new/changed lines* fall below **80%** coverage — so
  inadequately-tested new code is flagged right on the PR. Thresholds live in
  [`codecov.yml`](codecov.yml).

**One-time setup:** enable the repo on codecov.io and add its upload token as a
repository secret named `CODECOV_TOKEN` (Settings → Secrets and variables →
Actions). To make the checks block merges, add "Test & lint" and `codecov/patch`
as required status checks in branch protection.

## Reliability

The app is **fail-fast**: any unexpected panic (on any thread) logs and then
exits the whole process, rather than leaving a half-dead state (e.g. a live tray
with a silently-stopped sync/reminder engine). When run under the systemd user
service above, `Restart=on-failure` restarts it automatically, so fail-fast
becomes self-healing. If you run it in a terminal instead, it will exit on a
panic and you'll see the backtrace (set `RUST_BACKTRACE=1` for detail).

## Performance

The daemon is designed to be near-free while idle: both tokio runtimes (the
engine's and the UI's) are capped at 2 worker threads each, release builds use
thin LTO + stripped symbols, calendar syncs request Google *partial responses*
(only the fields the app reads) to keep poll traffic small, and the widget
renders with tiny-skia software rendering — a GPU stack (wgpu/Vulkan) costs
~130 MB of mapped driver libraries for a small always-on tray app and buys
nothing for this UI.

To check the footprint at any time:

```bash
make perf         # release binary size + running daemon CPU/RSS/threads
```

For an allocation-level deep dive, install [heaptrack](https://github.com/KDE/heaptrack)
(`sudo apt install heaptrack`), stop the user service so you don't get a second
tray instance, and run `make heaptrack`.

## Configuration

`~/.config/calendar-notification/config.toml`:

| Key | Meaning |
|---|---|
| `client_id` / `client_secret` | OAuth Desktop-app credentials |
| `poll_interval_minutes` | How often to resync (default 5) |
| `[calendars.<id>]` | Per-calendar `visible` / `notify` / `color`, managed via the tray |

## How reminders work

On each sync the app fetches occurrences from the start of today through the
next 48 hours (recurring events expanded server-side via Google's
`singleEvents`, so timezone/DST/EXDATE edge cases are handled by Google). The
agenda shows today's events; reminders can fire for anything in the window. For
every occurrence on a **notify-enabled** calendar it schedules that occurrence's
`popup` reminders (event overrides, or the calendar's default reminders) and
fires a desktop notification at each lead time.

Already-fired reminders are de-duplicated **within a run** (an in-memory set
keyed by occurrence + lead time). This set is not persisted, so it's empty after
a restart; what prevents a flood of stale notifications on startup is a separate
guard that skips any reminder whose fire time is already more than a few minutes
in the past. (A reminder that fired moments before a restart could therefore
fire once more.)

## Notes / limitations

- Reminders use each event's **popup** reminders; `email` reminders are left to
  Google to deliver.
- New events are created with the calendar's default reminders (`useDefault`).
- "Always on top" for the widget is honoured by GNOME for normal windows; if a
  compositor ignores the hint it's cosmetic only.
