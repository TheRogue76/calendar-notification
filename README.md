# calendar-notification

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

Closing the widget window keeps the daemon (tray + reminders) running.

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

## Reliability

The app is **fail-fast**: any unexpected panic (on any thread) logs and then
exits the whole process, rather than leaving a half-dead state (e.g. a live tray
with a silently-stopped sync/reminder engine). When run under the systemd user
service above, `Restart=on-failure` restarts it automatically, so fail-fast
becomes self-healing. If you run it in a terminal instead, it will exit on a
panic and you'll see the backtrace (set `RUST_BACKTRACE=1` for detail).

## Configuration

`~/.config/calendar-notification/config.toml`:

| Key | Meaning |
|---|---|
| `client_id` / `client_secret` | OAuth Desktop-app credentials |
| `poll_interval_minutes` | How often to resync (default 5) |
| `[calendars.<id>]` | Per-calendar `visible` / `notify` / `color`, managed via the tray |

## How reminders work

On each sync the app fetches occurrences for the next 48 hours (recurring events
expanded server-side via Google's `singleEvents`, so timezone/DST/EXDATE edge
cases are handled by Google). For every occurrence on a **notify-enabled**
calendar it schedules that occurrence's `popup` reminders (event overrides, or
the calendar's default reminders) and fires a desktop notification at each lead
time. Already-fired reminders are de-duplicated so restarts don't re-notify.

## Notes / limitations

- Reminders use each event's **popup** reminders; `email` reminders are left to
  Google to deliver.
- New events are created with the calendar's default reminders (`useDefault`).
- "Always on top" for the widget is honoured by GNOME for normal windows; if a
  compositor ignores the hint it's cosmetic only.
