//! The iced daemon: application state, the update loop, the subscription that
//! bridges engine events + window-close events into messages, and the widget
//! window lifecycle.
//!
//! Runs on the main thread with iced's own executor; all Google/tray/scheduler
//! work happens on a separate background runtime (see `main.rs`) and reaches us
//! only through `UI_RX` (engine → UI) and `cmd_tx` (UI → engine).

use std::sync::{Mutex, OnceLock};

use iced::futures::{SinkExt, Stream};
use iced::{window, Element, Size, Subscription, Task, Theme};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::engine::{CalendarView, Command, UiEvent};
use crate::google::model::{EventDetails, Occurrence};
use crate::ui::add_event::{self, FormMsg, FormState};
use crate::ui::{agenda, detail};

/// Engine → UI channel, installed by `main` before the daemon starts. The
/// subscription builder must be a bare `fn` (iced 0.14), so it can't capture the
/// receiver — we hand it over through this static instead.
static UI_RX: OnceLock<Mutex<Option<UnboundedReceiver<UiEvent>>>> = OnceLock::new();

pub fn install_ui_receiver(rx: UnboundedReceiver<UiEvent>) {
    let _ = UI_RX.set(Mutex::new(Some(rx)));
}

#[derive(Debug, Clone)]
pub enum Message {
    Engine(UiEvent),
    WindowClosed(window::Id),
    SyncNow,
    OpenAddForm,
    CloseAddForm,
    SubmitForm,
    Form(FormMsg),
    SetCalendarVisible(String, bool),
    /// An agenda row was clicked: open the detail pane and fetch full details.
    SelectEvent {
        calendar_id: String,
        event_id: String,
        title: String,
    },
    /// Leave the detail pane, back to the agenda.
    CloseDetail,
    /// Edit the currently-loaded event: pre-fill and open the form.
    EditSelected,
}

/// State of the detail pane, which takes over the window when an event is
/// selected (mirrors how the add-event form takes over).
#[derive(Debug, Clone)]
pub enum DetailState {
    /// Details are being fetched; show the title we already have.
    Loading { title: String },
    /// Fully-fetched event to display.
    Loaded(EventDetails),
    /// The fetch failed.
    Failed { title: String, message: String },
}

pub struct App {
    cmd_tx: UnboundedSender<Command>,
    pub calendars: Vec<CalendarView>,
    pub occurrences: Vec<Occurrence>,
    pub status: String,
    pub widget: Option<window::Id>,
    pub form: FormState,
    /// When `Some`, the detail pane is shown (unless the form is also open).
    pub detail: Option<DetailState>,
}

impl App {
    pub fn new(cmd_tx: UnboundedSender<Command>) -> Self {
        Self {
            cmd_tx,
            calendars: Vec::new(),
            occurrences: Vec::new(),
            status: "Starting…".to_string(),
            widget: None,
            form: FormState::default(),
            detail: None,
        }
    }

    fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn toggle_widget(&mut self) -> Task<Message> {
        if let Some(id) = self.widget.take() {
            window::close(id)
        } else {
            let settings = window::Settings {
                size: Size::new(380.0, 520.0),
                resizable: true,
                decorations: true,
                level: window::Level::AlwaysOnTop,
                position: window::Position::Centered,
                // The colored calendar glyph as the window/titlebar/taskbar icon.
                // Honoured on X11; on GNOME/Wayland the dock icon comes from the
                // `.desktop` file matched via `application_id` below instead.
                icon: window_icon(),
                platform_specific: window::settings::PlatformSpecific {
                    // Must match the installed `calendar-notification.desktop`
                    // basename / its `StartupWMClass` so GNOME shows our icon in
                    // the dock rather than a generic fallback.
                    application_id: "calendar-notification".to_string(),
                    ..window::settings::PlatformSpecific::default()
                },
                // Let the titlebar ✕ close (hide) the widget window. In daemon
                // mode this only closes the window — the process, tray, and
                // reminders keep running — and `close_events()` clears
                // `self.widget` so the tray toggle reopens it.
                exit_on_close_request: true,
                ..window::Settings::default()
            };
            let (id, open) = window::open(settings);
            self.widget = Some(id);
            open.discard()
        }
    }
}

/// The widget window icon: the shared colored calendar glyph at 64×64.
fn window_icon() -> Option<window::Icon> {
    const SIZE: u32 = 64;
    window::icon::from_rgba(crate::icon::calendar_rgba(SIZE), SIZE, SIZE).ok()
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Engine(ev) => handle_engine_event(app, ev),
        Message::WindowClosed(id) => {
            if app.widget == Some(id) {
                app.widget = None;
            }
            Task::none()
        }
        Message::SyncNow => {
            app.send(Command::SyncNow);
            Task::none()
        }
        Message::OpenAddForm => {
            // If a previous edit was cancelled, its edit state can linger on the
            // form; reset so "+ Add event" always opens a clean create form.
            if app.form.editing.is_some() {
                app.form.reset();
            }
            app.form.open = true;
            app.form.error = None;
            Task::none()
        }
        Message::CloseAddForm => {
            app.form.open = false;
            Task::none()
        }
        Message::Form(fm) => {
            app.form.update(fm);
            Task::none()
        }
        Message::SubmitForm => {
            match app.form.build(&app.calendars) {
                Ok(new) => {
                    app.form.submitting = true;
                    app.form.error = None;
                    match &app.form.editing {
                        Some(target) => app.send(Command::UpdateEvent {
                            calendar_id: target.calendar_id.clone(),
                            event_id: target.event_id.clone(),
                            event: new,
                        }),
                        None => app.send(Command::InsertEvent(new)),
                    }
                }
                Err(e) => app.form.error = Some(e),
            }
            Task::none()
        }
        Message::SetCalendarVisible(id, v) => {
            app.send(Command::SetVisible(id, v));
            Task::none()
        }
        Message::SelectEvent {
            calendar_id,
            event_id,
            title,
        } => {
            app.detail = Some(DetailState::Loading {
                title: title.clone(),
            });
            app.send(Command::LoadEvent {
                calendar_id,
                event_id,
            });
            Task::none()
        }
        Message::CloseDetail => {
            app.detail = None;
            Task::none()
        }
        Message::EditSelected => {
            if let Some(DetailState::Loaded(details)) = &app.detail {
                app.form = FormState::prefill(details);
                app.form.open = true;
            }
            Task::none()
        }
    }
}

fn handle_engine_event(app: &mut App, ev: UiEvent) -> Task<Message> {
    match ev {
        UiEvent::Calendars(cals) => {
            // Default the form's calendar to the primary once we know it.
            if app.form.calendar_id.is_none() {
                app.form.calendar_id = cals
                    .iter()
                    .find(|c| c.primary)
                    .or_else(|| cals.first())
                    .map(|c| c.id.clone());
            }
            app.calendars = cals;
            Task::none()
        }
        UiEvent::Occurrences(occ) => {
            app.occurrences = occ;
            Task::none()
        }
        UiEvent::ToggleWidget => app.toggle_widget(),
        UiEvent::EventCreated(Ok(_)) => {
            app.form.submitting = false;
            app.form.reset();
            app.form.open = false;
            app.status = "Event created".to_string();
            Task::none()
        }
        UiEvent::EventCreated(Err(e)) => {
            app.form.submitting = false;
            app.form.error = Some(e);
            Task::none()
        }
        UiEvent::EventLoaded(Ok(details)) => {
            // Ignore a stale load if the user already left the detail pane.
            if app.detail.is_some() {
                app.detail = Some(DetailState::Loaded(details));
            }
            Task::none()
        }
        UiEvent::EventLoaded(Err(message)) => {
            if let Some(DetailState::Loading { title }) = &app.detail {
                app.detail = Some(DetailState::Failed {
                    title: title.clone(),
                    message,
                });
            }
            Task::none()
        }
        UiEvent::EventUpdated(Ok(_)) => {
            app.form.submitting = false;
            app.form.reset();
            app.form.open = false;
            app.detail = None;
            app.status = "Event updated".to_string();
            Task::none()
        }
        UiEvent::EventUpdated(Err(e)) => {
            app.form.submitting = false;
            app.form.error = Some(e);
            Task::none()
        }
        UiEvent::Status(s) => {
            app.status = s;
            Task::none()
        }
        UiEvent::Quit => iced::exit(),
    }
}

pub fn view(app: &App, _window: window::Id) -> Element<'_, Message> {
    if app.form.open {
        add_event::view(&app.form, &app.calendars)
    } else if let Some(detail) = &app.detail {
        detail::view(detail, &app.calendars)
    } else {
        agenda::view(app)
    }
}

pub fn title(_app: &App, _window: window::Id) -> String {
    "Calendar".to_string()
}

pub fn theme(_app: &App, _window: window::Id) -> Theme {
    Theme::Dark
}

pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([
        Subscription::run(engine_events),
        window::close_events().map(Message::WindowClosed),
    ])
}

/// Bare `fn` (no captures) that drains the engine→UI receiver into messages.
fn engine_events() -> impl Stream<Item = Message> {
    iced::stream::channel(
        256,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let taken = UI_RX
                .get()
                .and_then(|m| m.lock().ok().and_then(|mut guard| guard.take()));
            let Some(mut rx) = taken else {
                // Receiver already taken (or not installed) — idle forever.
                std::future::pending::<()>().await;
                return;
            };
            while let Some(ev) = rx.recv().await {
                let _ = output.send(Message::Engine(ev)).await;
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::add_event::FormMsg;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn app() -> (App, UnboundedReceiver<Command>) {
        let (tx, rx) = unbounded_channel();
        (App::new(tx), rx)
    }

    fn cal(id: &str, primary: bool) -> CalendarView {
        CalendarView {
            id: id.into(),
            summary: id.into(),
            color: String::new(),
            primary,
            visible: true,
            notify: true,
        }
    }

    #[test]
    fn calendars_event_sets_state_and_defaults_form_calendar() {
        let (mut a, _rx) = app();
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::Calendars(vec![cal("a", false), cal("p", true)])),
        );
        assert_eq!(a.calendars.len(), 2);
        assert_eq!(a.form.calendar_id.as_deref(), Some("p")); // primary chosen
    }

    #[test]
    fn occurrences_event_sets_state() {
        let (mut a, _rx) = app();
        let _ = update(&mut a, Message::Engine(UiEvent::Occurrences(vec![])));
        assert!(a.occurrences.is_empty());
        let _ = update(&mut a, Message::Engine(UiEvent::Status("Synced".into())));
        assert_eq!(a.status, "Synced");
    }

    #[test]
    fn event_created_ok_resets_and_closes_form() {
        let (mut a, _rx) = app();
        a.form.open = true;
        a.form.submitting = true;
        a.form.title = "x".into();
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventCreated(Ok("id".into()))),
        );
        assert!(!a.form.open);
        assert!(!a.form.submitting);
        assert!(a.form.title.is_empty());
        assert_eq!(a.status, "Event created");
    }

    #[test]
    fn event_created_err_surfaces_error() {
        let (mut a, _rx) = app();
        a.form.submitting = true;
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventCreated(Err("bad".into()))),
        );
        assert!(!a.form.submitting);
        assert_eq!(a.form.error.as_deref(), Some("bad"));
    }

    #[test]
    fn toggle_widget_opens_then_closes() {
        let (mut a, _rx) = app();
        assert!(a.widget.is_none());
        let _ = update(&mut a, Message::Engine(UiEvent::ToggleWidget));
        let id = a.widget.expect("widget should be open");
        // Closing again clears it.
        let _ = update(&mut a, Message::Engine(UiEvent::ToggleWidget));
        assert!(a.widget.is_none());
        // A WindowClosed for the (now-stale) id is a no-op.
        let _ = update(&mut a, Message::WindowClosed(id));
        assert!(a.widget.is_none());
    }

    #[test]
    fn window_closed_clears_matching_widget() {
        let (mut a, _rx) = app();
        let _ = update(&mut a, Message::Engine(UiEvent::ToggleWidget));
        let id = a.widget.unwrap();
        let _ = update(&mut a, Message::WindowClosed(id));
        assert!(a.widget.is_none());
    }

    #[test]
    fn sync_now_sends_command() {
        let (mut a, mut rx) = app();
        let _ = update(&mut a, Message::SyncNow);
        assert!(matches!(rx.try_recv(), Ok(Command::SyncNow)));
    }

    #[test]
    fn open_and_close_add_form() {
        let (mut a, _rx) = app();
        a.form.error = Some("old".into());
        let _ = update(&mut a, Message::OpenAddForm);
        assert!(a.form.open);
        assert!(a.form.error.is_none());
        let _ = update(&mut a, Message::CloseAddForm);
        assert!(!a.form.open);
    }

    #[test]
    fn form_message_is_delegated() {
        let (mut a, _rx) = app();
        let _ = update(&mut a, Message::Form(FormMsg::Title("Hello".into())));
        assert_eq!(a.form.title, "Hello");
    }

    #[test]
    fn submit_valid_form_sends_insert_and_marks_submitting() {
        let (mut a, mut rx) = app();
        a.calendars = vec![cal("p", true)];
        a.form.calendar_id = Some("p".into());
        a.form.title = "Standup".into();
        a.form.start_date = "2026-07-02".into();
        a.form.start_time = "09:00".into();
        a.form.end_date = "2026-07-02".into();
        a.form.end_time = "09:30".into();
        let _ = update(&mut a, Message::SubmitForm);
        assert!(a.form.submitting);
        assert!(matches!(rx.try_recv(), Ok(Command::InsertEvent(_))));
    }

    #[test]
    fn submit_invalid_form_sets_error_no_command() {
        let (mut a, mut rx) = app();
        // no title -> build fails
        let _ = update(&mut a, Message::SubmitForm);
        assert!(!a.form.submitting);
        assert!(a.form.error.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn set_calendar_visible_sends_command() {
        let (mut a, mut rx) = app();
        let _ = update(&mut a, Message::SetCalendarVisible("c1".into(), false));
        match rx.try_recv() {
            Ok(Command::SetVisible(id, v)) => {
                assert_eq!(id, "c1");
                assert!(!v);
            }
            other => panic!("expected SetVisible, got {other:?}"),
        }
    }

    fn details() -> EventDetails {
        let start = chrono::Local::now();
        EventDetails {
            calendar_id: "p".into(),
            event_id: "master".into(),
            title: "Standup".into(),
            location: Some("Room".into()),
            description: Some("notes".into()),
            all_day: false,
            start,
            end: start + chrono::Duration::hours(1),
            attendees: vec!["a@x.com".into()],
            recurrence: vec![],
        }
    }

    #[test]
    fn select_event_sets_loading_and_sends_load_command() {
        let (mut a, mut rx) = app();
        let _ = update(
            &mut a,
            Message::SelectEvent {
                calendar_id: "p".into(),
                event_id: "master".into(),
                title: "Standup".into(),
            },
        );
        assert!(matches!(a.detail, Some(DetailState::Loading { .. })));
        match rx.try_recv() {
            Ok(Command::LoadEvent {
                calendar_id,
                event_id,
            }) => {
                assert_eq!(calendar_id, "p");
                assert_eq!(event_id, "master");
            }
            other => panic!("expected LoadEvent, got {other:?}"),
        }
    }

    #[test]
    fn event_loaded_transitions_loading_to_loaded_and_ignores_stale() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loading { title: "T".into() });
        let _ = update(&mut a, Message::Engine(UiEvent::EventLoaded(Ok(details()))));
        assert!(matches!(a.detail, Some(DetailState::Loaded(_))));

        // A load that arrives after the pane was closed is ignored.
        a.detail = None;
        let _ = update(&mut a, Message::Engine(UiEvent::EventLoaded(Ok(details()))));
        assert!(a.detail.is_none());
    }

    #[test]
    fn event_loaded_error_becomes_failed() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loading { title: "T".into() });
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventLoaded(Err("boom".into()))),
        );
        assert!(matches!(a.detail, Some(DetailState::Failed { .. })));
    }

    #[test]
    fn edit_selected_prefills_and_opens_form() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        let _ = update(&mut a, Message::EditSelected);
        assert!(a.form.open);
        assert_eq!(a.form.title, "Standup");
        assert!(a.form.editing.is_some());
    }

    #[test]
    fn submit_in_edit_mode_sends_update_event() {
        let (mut a, mut rx) = app();
        a.calendars = vec![cal("p", true)];
        a.form = crate::ui::add_event::FormState::prefill(&details());
        a.form.open = true;
        let _ = update(&mut a, Message::SubmitForm);
        assert!(a.form.submitting);
        match rx.try_recv() {
            Ok(Command::UpdateEvent {
                calendar_id,
                event_id,
                ..
            }) => {
                assert_eq!(calendar_id, "p");
                assert_eq!(event_id, "master");
            }
            other => panic!("expected UpdateEvent, got {other:?}"),
        }
    }

    #[test]
    fn open_add_form_resets_lingering_edit_state() {
        let (mut a, _rx) = app();
        // Simulate a cancelled edit: the form still carries edit state.
        a.form = crate::ui::add_event::FormState::prefill(&details());
        a.form.open = false;
        let _ = update(&mut a, Message::OpenAddForm);
        assert!(a.form.open);
        assert!(
            a.form.editing.is_none(),
            "add form must not be in edit mode"
        );
        assert!(a.form.title.is_empty(), "prefilled title must be cleared");
    }

    #[test]
    fn window_icon_builds_from_rgba() {
        assert!(window_icon().is_some(), "64x64 RGBA must be a valid icon");
    }

    #[test]
    fn close_detail_clears_pane() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        let _ = update(&mut a, Message::CloseDetail);
        assert!(a.detail.is_none());
    }

    #[test]
    fn event_updated_ok_closes_form_and_clears_detail() {
        let (mut a, _rx) = app();
        a.form = crate::ui::add_event::FormState::prefill(&details());
        a.form.open = true;
        a.form.submitting = true;
        a.detail = Some(DetailState::Loaded(details()));
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventUpdated(Ok("master".into()))),
        );
        assert!(!a.form.open);
        assert!(!a.form.submitting);
        assert!(a.form.editing.is_none());
        assert!(a.detail.is_none());
        assert_eq!(a.status, "Event updated");
    }

    #[test]
    fn event_updated_err_surfaces_error() {
        let (mut a, _rx) = app();
        a.form.submitting = true;
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventUpdated(Err("bad".into()))),
        );
        assert!(!a.form.submitting);
        assert_eq!(a.form.error.as_deref(), Some("bad"));
    }

    #[test]
    fn view_and_metadata_helpers_run() {
        let (mut a, _rx) = app();
        let _ = update(&mut a, Message::Engine(UiEvent::ToggleWidget));
        let id = a.widget.unwrap();
        // Agenda mode.
        let _ = view(&a, id);
        // Form mode.
        a.form.open = true;
        let _ = view(&a, id);
        assert_eq!(title(&a, id), "Calendar");
        let _ = theme(&a, id);
    }
}
