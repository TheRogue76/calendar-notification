//! System-tray icon (StatusNotifierItem via `ksni` 0.3). Menu actions are sent
//! as [`Command`]s to the engine; the calendar submenu reflects live state,
//! which the engine refreshes through the returned [`ksni::Handle`].

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, Tray, TrayMethods};
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
        // Freedesktop themed calendar icon (present in Adwaita/Yaru).
        "x-office-calendar".into()
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
