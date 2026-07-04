//! System-tray icon (StatusNotifierItem via `ksni` 0.3). Menu actions are sent
//! as [`Command`]s to the engine; the calendar submenu reflects live state,
//! which the engine refreshes through the returned [`ksni::Handle`].

use chrono::{DateTime, Local};
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, Tray, TrayMethods};
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

use crate::engine::{CalendarView, Command};

/// The soonest upcoming event, shown as a label at the top of the tray menu.
/// The engine picks it; the tray formats the relative time at render.
#[derive(Debug, Clone)]
pub struct NextEvent {
    pub title: String,
    pub start: DateTime<Local>,
}

pub struct CalTray {
    pub tx: UnboundedSender<Command>,
    pub calendars: Vec<CalendarView>,
    pub next_event: Option<NextEvent>,
    /// Whether OAuth credentials are configured. While `false` the menu reduces
    /// to just *Configure…* + *Quit*; the engine flips it once setup succeeds.
    pub configured: bool,
}

impl CalTray {
    pub fn new(tx: UnboundedSender<Command>) -> Self {
        Self {
            tx,
            calendars: Vec::new(),
            next_event: None,
            configured: false,
        }
    }

    /// Spawn the tray on the current tokio runtime, returning a live handle.
    pub async fn spawn_tray(self) -> Option<ksni::Handle<CalTray>> {
        match self.spawn().await {
            Ok(handle) => Some(handle),
            Err(e) => {
                warn!("could not register system-tray icon: {e}");
                None
            }
        }
    }

    fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Tray for CalTray {
    fn id(&self) -> String {
        "com.calendar-notification.tray".into()
    }

    fn title(&self) -> String {
        "Calendar".into()
    }

    fn icon_name(&self) -> String {
        // Fallback themed name; most panels prefer the pixmap below.
        "x-office-calendar".into()
    }

    /// A hand-drawn colored calendar glyph, so the tray shows a recognizable
    /// icon instead of falling back to a generic themed gear. Provided at a few
    /// sizes so the panel can pick the closest fit.
    fn icon_pixmap(&self) -> Vec<Icon> {
        [22, 24, 32, 48].into_iter().map(calendar_icon).collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Calendar Notification".into(),
            description: "Upcoming Google Calendar events".into(),
            icon_name: "x-office-calendar".into(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // Before setup is complete there's nothing to sync or filter, so the
        // menu is just a way in to the credential screen (plus Quit).
        if !self.configured {
            return vec![
                StandardItem {
                    label: "Configure…".into(),
                    icon_name: "preferences-system".into(),
                    activate: Box::new(|t: &mut Self| t.send(Command::Configure)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    icon_name: "application-exit".into(),
                    activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                    ..Default::default()
                }
                .into(),
            ];
        }

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // "Up next" label at the top: a disabled item so it reads as text, not a
        // clickable action. Formatted here so the relative time is fresh each
        // time the menu opens.
        if let Some(ev) = &self.next_event {
            items.push(
                StandardItem {
                    label: format!(
                        "Next: {} — {}",
                        ev.title,
                        format_relative(ev.start, Local::now())
                    ),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }

        items.extend([
            StandardItem {
                label: "Show / hide widget".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleWidget)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Sync now".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::SyncNow)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ]);

        // Per-calendar submenu with Visible / Notify checkboxes.
        if !self.calendars.is_empty() {
            let mut cal_items: Vec<MenuItem<Self>> = Vec::new();
            for cal in &self.calendars {
                let id_vis = cal.id.clone();
                let id_not = cal.id.clone();
                cal_items.push(
                    SubMenu {
                        label: cal.summary.clone(),
                        submenu: vec![
                            CheckmarkItem {
                                label: "Visible in agenda".into(),
                                checked: cal.visible,
                                activate: Box::new(move |t: &mut Self| {
                                    if let Some(c) = t.calendars.iter_mut().find(|c| c.id == id_vis)
                                    {
                                        c.visible = !c.visible;
                                        let v = c.visible;
                                        t.send(Command::SetVisible(id_vis.clone(), v));
                                    }
                                }),
                                ..Default::default()
                            }
                            .into(),
                            CheckmarkItem {
                                label: "Notify".into(),
                                checked: cal.notify,
                                activate: Box::new(move |t: &mut Self| {
                                    if let Some(c) = t.calendars.iter_mut().find(|c| c.id == id_not)
                                    {
                                        c.notify = !c.notify;
                                        let v = c.notify;
                                        t.send(Command::SetNotify(id_not.clone(), v));
                                    }
                                }),
                                ..Default::default()
                            }
                            .into(),
                        ],
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(
                SubMenu {
                    label: "Calendars".into(),
                    submenu: cal_items,
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }

        items.push(
            StandardItem {
                label: "Settings".into(),
                icon_name: "preferences-system".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Configure)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}

/// Human-friendly lead time until `start`, phrased for the tray label:
/// `"now"` once it has started, else `"in 5 min"` / `"in 2 h 10 min"` /
/// `"in 3 days"`.
fn format_relative(start: DateTime<Local>, now: DateTime<Local>) -> String {
    let delta = start - now;
    let mins = delta.num_minutes();
    if mins <= 0 {
        return "now".into();
    }
    if mins < 60 {
        return format!("in {mins} min");
    }
    let hours = delta.num_hours();
    if hours < 24 {
        let rem = mins - hours * 60;
        if rem == 0 {
            return format!("in {hours} h");
        }
        return format!("in {hours} h {rem} min");
    }
    let days = delta.num_days();
    if days == 1 {
        "in 1 day".into()
    } else {
        format!("in {days} days")
    }
}

/// The colored calendar glyph (see [`crate::icon`]) repacked into a ksni
/// [`Icon`]: ARGB32 in network byte order (A, R, G, B), vs. the shared drawing's
/// RGBA. Same glyph as the widget's window icon, so tray and dock stay in sync.
fn calendar_icon(size: u32) -> Icon {
    let mut data = crate::icon::calendar_rgba(size);
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1); // RGBA -> ARGB (A, R, G, B)
    }
    Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn view(id: &str) -> CalendarView {
        CalendarView {
            id: id.into(),
            summary: id.to_uppercase(),
            color: "#123456".into(),
            primary: false,
            visible: true,
            notify: true,
        }
    }

    fn has_submenu(items: &[MenuItem<CalTray>], label: &str) -> bool {
        items
            .iter()
            .any(|i| matches!(i, MenuItem::SubMenu(sm) if sm.label == label))
    }

    fn invoke_all(items: Vec<MenuItem<CalTray>>, tray: &mut CalTray) {
        for it in items {
            match it {
                MenuItem::Standard(s) => (s.activate)(tray),
                MenuItem::Checkmark(c) => (c.activate)(tray),
                MenuItem::SubMenu(sm) => invoke_all(sm.submenu, tray),
                _ => {}
            }
        }
    }

    fn drain(rx: &mut UnboundedReceiver<Command>) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(c);
        }
        out
    }

    #[test]
    fn metadata_is_populated() {
        let (tx, _rx) = unbounded_channel();
        let t = CalTray::new(tx);
        assert!(!t.id().is_empty());
        assert_eq!(t.title(), "Calendar");
        assert!(!t.icon_name().is_empty());
        assert_eq!(t.tool_tip().title, "Calendar Notification");
    }

    #[test]
    fn calendar_icon_has_correct_argb_buffer() {
        for size in [22u32, 24, 32, 48] {
            let icon = calendar_icon(size);
            assert_eq!(icon.width, size as i32);
            assert_eq!(icon.height, size as i32);
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
            // At least some pixels are painted opaque (alpha byte set).
            assert!(icon.data.chunks_exact(4).any(|px| px[0] == 255));
        }
    }

    #[test]
    fn icon_pixmap_offers_multiple_sizes() {
        let (tx, _rx) = unbounded_channel();
        let t = CalTray::new(tx);
        let pixmaps = t.icon_pixmap();
        assert_eq!(pixmaps.len(), 4);
        assert!(pixmaps.iter().all(|i| !i.data.is_empty()));
    }

    fn first_label(items: &[MenuItem<CalTray>]) -> Option<String> {
        items.iter().find_map(|i| match i {
            MenuItem::Standard(s) => Some(s.label.clone()),
            _ => None,
        })
    }

    #[test]
    fn format_relative_covers_ranges() {
        let now = Local.with_ymd_and_hms(2026, 7, 2, 12, 0, 0).unwrap();
        assert_eq!(
            format_relative(now - chrono::Duration::minutes(1), now),
            "now"
        );
        assert_eq!(format_relative(now, now), "now");
        assert_eq!(
            format_relative(now + chrono::Duration::minutes(5), now),
            "in 5 min"
        );
        assert_eq!(
            format_relative(now + chrono::Duration::hours(2), now),
            "in 2 h"
        );
        assert_eq!(
            format_relative(now + chrono::Duration::minutes(130), now),
            "in 2 h 10 min"
        );
        assert_eq!(
            format_relative(now + chrono::Duration::hours(24), now),
            "in 1 day"
        );
        assert_eq!(
            format_relative(now + chrono::Duration::days(3), now),
            "in 3 days"
        );
    }

    #[test]
    fn menu_shows_next_event_when_set() {
        let (tx, _rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.configured = true;
        assert!(t.next_event.is_none());
        // Without a next event, the first item is a regular action, not "Next:".
        assert!(!first_label(&t.menu()).unwrap().starts_with("Next:"));

        t.next_event = Some(NextEvent {
            title: "Standup".into(),
            start: Local::now() + chrono::Duration::minutes(30),
        });
        let label = first_label(&t.menu()).unwrap();
        assert!(label.starts_with("Next: Standup — in"), "got {label:?}");
    }

    #[test]
    fn menu_without_calendars_has_no_submenu() {
        let (tx, _rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.configured = true;
        let items = t.menu();
        assert!(!has_submenu(&items, "Calendars"));
    }

    #[test]
    fn menu_with_calendars_has_submenu() {
        let (tx, _rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.configured = true;
        t.calendars = vec![view("p"), view("w")];
        assert!(has_submenu(&t.menu(), "Calendars"));
    }

    #[test]
    fn unconfigured_menu_offers_only_configure_and_quit() {
        let (tx, mut rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        assert!(!t.configured, "default is unconfigured");
        let items = t.menu();
        // No agenda controls or calendar submenu before setup.
        assert!(!has_submenu(&items, "Calendars"));
        invoke_all(items, &mut t);
        let cmds = drain(&mut rx);
        assert!(cmds.iter().any(|c| matches!(c, Command::Configure)));
        assert!(cmds.iter().any(|c| matches!(c, Command::Quit)));
        assert!(
            !cmds.iter().any(|c| matches!(c, Command::SyncNow)),
            "no Sync now while unconfigured"
        );
    }

    #[test]
    fn configured_menu_includes_settings_entry() {
        let (tx, mut rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.configured = true;
        invoke_all(t.menu(), &mut t);
        let cmds = drain(&mut rx);
        // The Settings item reuses Command::Configure to reopen the setup screen.
        assert!(cmds.iter().any(|c| matches!(c, Command::Configure)));
    }

    #[test]
    fn activating_items_sends_commands() {
        let (tx, mut rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.configured = true;
        t.calendars = vec![view("p")];
        let items = t.menu();
        invoke_all(items, &mut t);
        let cmds = drain(&mut rx);
        assert!(cmds.iter().any(|c| matches!(c, Command::ToggleWidget)));
        assert!(cmds.iter().any(|c| matches!(c, Command::SyncNow)));
        assert!(cmds.iter().any(|c| matches!(c, Command::Quit)));
        // The per-calendar checkmarks flip local state and emit set-commands.
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::SetVisible(id, _) if id == "p")));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::SetNotify(id, _) if id == "p")));
    }
}
