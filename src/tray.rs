//! System-tray icon (StatusNotifierItem via `ksni` 0.3). Menu actions are sent
//! as [`Command`]s to the engine; the calendar submenu reflects live state,
//! which the engine refreshes through the returned [`ksni::Handle`].

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, Tray, TrayMethods};
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

use crate::engine::{CalendarView, Command};

pub struct CalTray {
    pub tx: UnboundedSender<Command>,
    pub calendars: Vec<CalendarView>,
}

impl CalTray {
    pub fn new(tx: UnboundedSender<Command>) -> Self {
        Self {
            tx,
            calendars: Vec::new(),
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
        let mut items: Vec<MenuItem<Self>> = vec![
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
        ];

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

// -- icon drawing ----------------------------------------------------------

type Rgb = (u8, u8, u8);

const WHITE: Rgb = (255, 255, 255);
const BLUE: Rgb = (66, 133, 244); // calendar-blue header
const DARK: Rgb = (60, 64, 67); // binding tabs
const RED: Rgb = (234, 67, 53); // "today" accent dot
const GREY: Rgb = (154, 160, 166); // other day dots

/// Set one opaque pixel (ARGB32, network byte order: A, R, G, B).
fn set_px(data: &mut [u8], s: i32, x: i32, y: i32, (r, g, b): Rgb) {
    if x < 0 || y < 0 || x >= s || y >= s {
        return;
    }
    let i = ((y * s + x) as usize) * 4;
    data[i] = 255;
    data[i + 1] = r;
    data[i + 2] = g;
    data[i + 3] = b;
}

/// Fill a rectangle whose corners can be individually rounded
/// (`corners` = [top-left, top-right, bottom-left, bottom-right]).
#[allow(clippy::too_many_arguments)]
fn fill_rounded(
    data: &mut [u8],
    s: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    r: i32,
    corners: [bool; 4],
    color: Rgb,
) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            // Skip a pixel only if it's outside the rounding circle of a corner
            // that is meant to be rounded.
            let outside = |cx: i32, cy: i32| {
                let (dx, dy) = ((x - cx) as f32, (y - cy) as f32);
                dx * dx + dy * dy > (r * r) as f32
            };
            let skip = (corners[0] && x < x0 + r && y < y0 + r && outside(x0 + r, y0 + r))
                || (corners[1] && x > x1 - r && y < y0 + r && outside(x1 - r, y0 + r))
                || (corners[2] && x < x0 + r && y > y1 - r && outside(x0 + r, y1 - r))
                || (corners[3] && x > x1 - r && y > y1 - r && outside(x1 - r, y1 - r));
            if !skip {
                set_px(data, s, x, y, color);
            }
        }
    }
}

/// Fill a solid disc centered at (cx, cy).
fn fill_disc(data: &mut [u8], s: i32, cx: i32, cy: i32, r: i32, color: Rgb) {
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let (dx, dy) = ((x - cx) as f32, (y - cy) as f32);
            if dx * dx + dy * dy <= (r * r) as f32 {
                set_px(data, s, x, y, color);
            }
        }
    }
}

/// Draw the colored calendar glyph at `size`×`size` as an ARGB32 pixmap: a
/// rounded white card with a blue header band, two dark binding tabs, and a grid
/// of day dots (the first one red as a "today" accent).
fn calendar_icon(size: u32) -> Icon {
    let s = size as i32;
    let sf = size as f32;
    let mut data = vec![0u8; (size * size * 4) as usize];

    let m = (sf * 0.09).round() as i32;
    let card_left = m;
    let card_right = s - m;
    let card_top = (sf * 0.22).round() as i32;
    let card_bottom = s - m;
    let radius = ((sf * 0.12).round() as i32).max(1);
    let header_bottom = card_top + ((card_bottom - card_top) as f32 * 0.30) as i32;

    // Binding tabs peeking above the card.
    let tab_w = ((sf * 0.09).round() as i32).max(1);
    let tab_top = (sf * 0.06).round() as i32;
    for tab_x in [
        card_left + (sf * 0.16) as i32,
        card_right - (sf * 0.16) as i32 - tab_w,
    ] {
        fill_rounded(
            &mut data,
            s,
            tab_x,
            tab_top,
            tab_x + tab_w,
            header_bottom,
            (tab_w / 2).max(1),
            [true, true, false, false],
            DARK,
        );
    }

    // Blue card (rounded), then the white body below the header (bottom corners
    // rounded, top square so it meets the header on a straight line).
    fill_rounded(
        &mut data,
        s,
        card_left,
        card_top,
        card_right,
        card_bottom,
        radius,
        [true, true, true, true],
        BLUE,
    );
    fill_rounded(
        &mut data,
        s,
        card_left,
        header_bottom,
        card_right,
        card_bottom,
        radius,
        [false, false, true, true],
        WHITE,
    );

    // Day dots: 3 columns × 2 rows across the white body; first one red.
    let dot_r = ((sf * 0.055).round() as i32).max(1);
    let body_left = card_left + (sf * 0.13) as i32;
    let body_right = card_right - (sf * 0.13) as i32;
    let body_top = header_bottom + (sf * 0.12) as i32;
    let col_gap = (body_right - body_left) / 2;
    let row_gap = (card_bottom - (sf * 0.10) as i32 - body_top).max(1);
    for (idx, (col, row)) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]
        .into_iter()
        .enumerate()
    {
        let cx = body_left + col * col_gap;
        let cy = body_top + row * row_gap;
        let color = if idx == 0 { RED } else { GREY };
        fill_disc(&mut data, s, cx, cy, dot_r, color);
    }

    Icon {
        width: s,
        height: s,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn menu_without_calendars_has_no_submenu() {
        let (tx, _rx) = unbounded_channel();
        let t = CalTray::new(tx);
        let items = t.menu();
        assert!(!has_submenu(&items, "Calendars"));
    }

    #[test]
    fn menu_with_calendars_has_submenu() {
        let (tx, _rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
        t.calendars = vec![view("p"), view("w")];
        assert!(has_submenu(&t.menu(), "Calendars"));
    }

    #[test]
    fn activating_items_sends_commands() {
        let (tx, mut rx) = unbounded_channel();
        let mut t = CalTray::new(tx);
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
