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
use crate::ui::setup::{self, SetupMsg, SetupState};
use crate::ui::{agenda, detail};

/// Engine → UI channel, installed by `main` before the daemon starts. The
/// subscription builder must be a bare `fn` (iced 0.14), so it can't capture the
/// receiver — we hand it over through this static instead.
static UI_RX: OnceLock<Mutex<Option<UnboundedReceiver<UiEvent>>>> = OnceLock::new();

pub fn install_ui_receiver(rx: UnboundedReceiver<UiEvent>) {
    let _ = UI_RX.set(Mutex::new(Some(rx)));
}

/// The iced daemon's executor: a 2-worker tokio runtime instead of iced's
/// default one-worker-per-core `Runtime::new()`. The UI side only drives the
/// subscription bridge and window tasks, so per-core workers are pure idle
/// thread (and malloc-arena) overhead. Mirrors iced's own tokio backend impl.
pub struct UiExecutor(tokio::runtime::Runtime);

impl iced::Executor for UiExecutor {
    fn new() -> Result<Self, iced::futures::io::Error> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map(Self)
    }

    fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        // Detach: dropping the JoinHandle lets the spawned task run to
        // completion on the runtime.
        drop(self.0.spawn(future));
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }

    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        self.0.block_on(future)
    }
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
    /// `event_id` is the edit/delete-series target (series master for a
    /// recurring event); `instance_id` is the per-instance id, needed to delete
    /// just this occurrence.
    SelectEvent {
        calendar_id: String,
        event_id: String,
        instance_id: String,
        title: String,
    },
    /// Leave the detail pane, back to the agenda.
    CloseDetail,
    /// Edit the currently-loaded event: pre-fill and open the form.
    EditSelected,
    /// Delete pressed: expand the inline confirm / recurring-scope buttons.
    RequestDelete,
    /// Back out of the delete confirmation.
    CancelDelete,
    /// Commit a delete of the given event id (the view resolves instance vs
    /// series).
    ConfirmDelete {
        calendar_id: String,
        event_id: String,
    },
    /// A field edit on the credential-setup screen.
    Setup(SetupMsg),
    /// Save the entered credentials (kicks off authorize + sync in the engine).
    SubmitSetup,
    /// Close the setup screen without saving.
    CancelSetup,
    /// Open Google Cloud Console in the user's browser (to create the OAuth client).
    OpenSetupConsole,
}

/// State of the detail pane, which takes over the window when an event is
/// selected (mirrors how the add-event form takes over).
#[derive(Debug, Clone)]
pub enum DetailState {
    /// Details are being fetched; show the title we already have. `event_id`
    /// is the id we asked the engine for — only a matching [`UiEvent::EventLoaded`]
    /// response may take over the pane (a slow response for a previously
    /// selected event must not).
    Loading { event_id: String, title: String },
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
    /// Per-instance id of the selected occurrence, kept so a recurring event can
    /// be deleted as just-this-instance (the detail pane loads the series master).
    pub selected_instance: Option<String>,
    /// Whether the detail pane's inline delete confirmation is showing.
    pub delete_prompt: bool,
    /// When `Some`, the credential-setup screen takes over the window (highest
    /// priority — shown before form/detail/agenda).
    pub setup: Option<SetupState>,
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
            selected_instance: None,
            delete_prompt: false,
            setup: None,
        }
    }

    fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn toggle_widget(&mut self) -> Task<Message> {
        if let Some(id) = self.widget.take() {
            window::close(id)
        } else {
            self.open_widget()
        }
    }

    /// Open the widget window if it isn't already open (no-op otherwise). Used
    /// by the tray toggle's open branch and by the setup screen, which must
    /// force the window open regardless of its current state.
    fn open_widget(&mut self) -> Task<Message> {
        if self.widget.is_some() {
            return Task::none();
        }
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
            instance_id,
            title,
        } => {
            app.detail = Some(DetailState::Loading {
                event_id: event_id.clone(),
                title,
            });
            app.selected_instance = Some(instance_id);
            app.delete_prompt = false;
            app.send(Command::LoadEvent {
                calendar_id,
                event_id,
            });
            Task::none()
        }
        Message::CloseDetail => {
            app.detail = None;
            app.selected_instance = None;
            app.delete_prompt = false;
            Task::none()
        }
        Message::EditSelected => {
            if let Some(DetailState::Loaded(details)) = &app.detail {
                app.form = FormState::prefill(details);
                app.form.open = true;
            }
            Task::none()
        }
        Message::RequestDelete => {
            app.delete_prompt = true;
            Task::none()
        }
        Message::CancelDelete => {
            app.delete_prompt = false;
            Task::none()
        }
        Message::ConfirmDelete {
            calendar_id,
            event_id,
        } => {
            app.delete_prompt = false;
            app.send(Command::DeleteEvent {
                calendar_id,
                event_id,
            });
            Task::none()
        }
        Message::Setup(m) => {
            if let Some(setup) = &mut app.setup {
                setup.update(m);
            }
            Task::none()
        }
        Message::SubmitSetup => {
            let Some(setup) = &mut app.setup else {
                return Task::none();
            };
            if setup.client_id.trim().is_empty() || setup.client_secret.trim().is_empty() {
                setup.error = Some("Both Client ID and Client secret are required".into());
                return Task::none();
            }
            setup.submitting = true;
            setup.error = None;
            let client_id = setup.client_id.trim().to_string();
            let client_secret = setup.client_secret.trim().to_string();
            app.send(Command::SaveCredentials {
                client_id,
                client_secret,
            });
            Task::none()
        }
        Message::CancelSetup => {
            // Close the setup screen and tell the engine to abort any in-flight
            // OAuth (harmless if none is pending). Before the engine's
            // non-blocking rework this couldn't truly abort the flow; now it does.
            app.setup = None;
            app.send(Command::CancelSetup);
            Task::none()
        }
        Message::OpenSetupConsole => {
            // Best-effort: hand the console URL to the desktop's URL opener.
            // Reap the child in a detached thread so a quick-exiting `xdg-open`
            // doesn't linger as a zombie (std's Child doesn't wait on drop); we
            // don't care about the result. Not unit-tested (external process).
            if let Ok(mut child) = std::process::Command::new("xdg-open")
                .arg(setup::CONSOLE_URL)
                .spawn()
            {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
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
        UiEvent::EventLoaded { event_id, result } => {
            // Apply only while the pane is waiting for exactly this event;
            // drop responses that are stale (pane closed, or the user selected
            // a different event since the request went out).
            if let Some(DetailState::Loading {
                event_id: expected,
                title,
            }) = &app.detail
            {
                if *expected == event_id {
                    app.detail = Some(match result {
                        Ok(details) => DetailState::Loaded(details),
                        Err(message) => DetailState::Failed {
                            title: title.clone(),
                            message,
                        },
                    });
                }
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
        UiEvent::EventDeleted(Ok(())) => {
            app.detail = None;
            app.selected_instance = None;
            app.delete_prompt = false;
            app.status = "Event deleted".to_string();
            Task::none()
        }
        UiEvent::EventDeleted(Err(e)) => {
            // Keep the detail pane open so the user can retry; surface the error.
            app.delete_prompt = false;
            app.status = e;
            Task::none()
        }
        UiEvent::OpenSetup {
            client_id,
            client_secret,
        } => {
            app.setup = Some(SetupState::new(client_id, client_secret));
            // Force the window open so the setup screen is actually visible even
            // when triggered from the tray with no window showing.
            app.open_widget()
        }
        UiEvent::SetupResult(Ok(())) => {
            app.setup = None;
            app.status = "Connected to Google".to_string();
            Task::none()
        }
        UiEvent::SetupResult(Err(e)) => {
            if let Some(setup) = &mut app.setup {
                setup.submitting = false;
                setup.error = Some(e);
            }
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
    if let Some(setup) = &app.setup {
        setup::view(setup)
    } else if app.form.open {
        add_event::view(&app.form, &app.calendars)
    } else if let Some(detail) = &app.detail {
        detail::view(
            detail,
            &app.calendars,
            app.delete_prompt,
            app.selected_instance.as_deref(),
        )
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
                instance_id: "master_20260702".into(),
                title: "Standup".into(),
            },
        );
        assert!(matches!(a.detail, Some(DetailState::Loading { .. })));
        assert_eq!(a.selected_instance.as_deref(), Some("master_20260702"));
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
    fn request_and_cancel_delete_toggle_prompt() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        let _ = update(&mut a, Message::RequestDelete);
        assert!(a.delete_prompt);
        let _ = update(&mut a, Message::CancelDelete);
        assert!(!a.delete_prompt);
    }

    #[test]
    fn confirm_delete_sends_command_and_clears_prompt() {
        let (mut a, mut rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        a.delete_prompt = true;
        let _ = update(
            &mut a,
            Message::ConfirmDelete {
                calendar_id: "p".into(),
                event_id: "master".into(),
            },
        );
        assert!(!a.delete_prompt);
        match rx.try_recv() {
            Ok(Command::DeleteEvent {
                calendar_id,
                event_id,
            }) => {
                assert_eq!(calendar_id, "p");
                assert_eq!(event_id, "master");
            }
            other => panic!("expected DeleteEvent, got {other:?}"),
        }
    }

    #[test]
    fn event_deleted_ok_closes_pane_and_clears_state() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        a.selected_instance = Some("inst".into());
        a.delete_prompt = true;
        let _ = update(&mut a, Message::Engine(UiEvent::EventDeleted(Ok(()))));
        assert!(a.detail.is_none());
        assert!(a.selected_instance.is_none());
        assert!(!a.delete_prompt);
        assert_eq!(a.status, "Event deleted");
    }

    #[test]
    fn event_deleted_err_keeps_pane_and_surfaces_status() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        a.delete_prompt = true;
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventDeleted(Err("nope".into()))),
        );
        assert!(a.detail.is_some());
        assert!(!a.delete_prompt);
        assert_eq!(a.status, "nope");
    }

    #[test]
    fn close_detail_clears_delete_state() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loaded(details()));
        a.selected_instance = Some("inst".into());
        a.delete_prompt = true;
        let _ = update(&mut a, Message::CloseDetail);
        assert!(a.detail.is_none());
        assert!(a.selected_instance.is_none());
        assert!(!a.delete_prompt);
    }

    fn loaded_ok(event_id: &str) -> UiEvent {
        UiEvent::EventLoaded {
            event_id: event_id.into(),
            result: Ok(details()),
        }
    }

    #[test]
    fn event_loaded_transitions_loading_to_loaded_and_ignores_stale() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loading {
            event_id: "master".into(),
            title: "T".into(),
        });
        let _ = update(&mut a, Message::Engine(loaded_ok("master")));
        assert!(matches!(a.detail, Some(DetailState::Loaded(_))));

        // A load that arrives after the pane was closed is ignored.
        a.detail = None;
        let _ = update(&mut a, Message::Engine(loaded_ok("master")));
        assert!(a.detail.is_none());
    }

    #[test]
    fn event_loaded_for_a_different_event_is_ignored() {
        let (mut a, _rx) = app();
        // The user selected "other" after the request for "master" went out;
        // the slow "master" response must not take over the pane.
        a.detail = Some(DetailState::Loading {
            event_id: "other".into(),
            title: "Other".into(),
        });
        let _ = update(&mut a, Message::Engine(loaded_ok("master")));
        assert!(
            matches!(&a.detail, Some(DetailState::Loading { event_id, .. }) if event_id == "other"),
            "stale response must leave the pane waiting for the newer event"
        );
    }

    #[test]
    fn event_loaded_error_becomes_failed() {
        let (mut a, _rx) = app();
        a.detail = Some(DetailState::Loading {
            event_id: "master".into(),
            title: "T".into(),
        });
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::EventLoaded {
                event_id: "master".into(),
                result: Err("boom".into()),
            }),
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
    fn ui_executor_spawns_enters_and_blocks_on() {
        use iced::Executor as _;
        let ex = UiExecutor::new().expect("runtime builds");
        let (tx, rx) = std::sync::mpsc::channel();
        ex.spawn(async move {
            let _ = tx.send(42);
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            42
        );
        assert_eq!(ex.block_on(async { 7 }), 7);
        assert_eq!(ex.enter(|| 1), 1);
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
    fn setup_field_edits_are_delegated() {
        let (mut a, _rx) = app();
        a.setup = Some(SetupState::default());
        let _ = update(&mut a, Message::Setup(SetupMsg::ClientId("id".into())));
        let _ = update(&mut a, Message::Setup(SetupMsg::ClientSecret("sec".into())));
        let s = a.setup.as_ref().unwrap();
        assert_eq!(s.client_id, "id");
        assert_eq!(s.client_secret, "sec");
    }

    #[test]
    fn submit_setup_sends_save_credentials_and_marks_submitting() {
        let (mut a, mut rx) = app();
        a.setup = Some(SetupState::new("id".into(), "sec".into()));
        let _ = update(&mut a, Message::SubmitSetup);
        assert!(a.setup.as_ref().unwrap().submitting);
        match rx.try_recv() {
            Ok(Command::SaveCredentials {
                client_id,
                client_secret,
            }) => {
                assert_eq!(client_id, "id");
                assert_eq!(client_secret, "sec");
            }
            other => panic!("expected SaveCredentials, got {other:?}"),
        }
    }

    #[test]
    fn submit_setup_with_blank_fields_errors_and_sends_nothing() {
        let (mut a, mut rx) = app();
        a.setup = Some(SetupState::new("id".into(), "   ".into()));
        let _ = update(&mut a, Message::SubmitSetup);
        let s = a.setup.as_ref().unwrap();
        assert!(!s.submitting);
        assert!(s.error.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cancel_setup_closes_the_screen() {
        let (mut a, _rx) = app();
        a.setup = Some(SetupState::default());
        let _ = update(&mut a, Message::CancelSetup);
        assert!(a.setup.is_none());
    }

    #[test]
    fn open_setup_event_prefills_and_opens_window() {
        let (mut a, _rx) = app();
        assert!(a.widget.is_none());
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::OpenSetup {
                client_id: "id".into(),
                client_secret: "sec".into(),
            }),
        );
        let s = a.setup.as_ref().expect("setup screen opened");
        assert_eq!(s.client_id, "id");
        assert_eq!(s.client_secret, "sec");
        assert!(
            a.widget.is_some(),
            "window forced open for the setup screen"
        );
    }

    #[test]
    fn setup_result_ok_closes_screen_err_shows_error() {
        let (mut a, _rx) = app();
        a.setup = Some(SetupState::new("id".into(), "sec".into()));
        a.setup.as_mut().unwrap().submitting = true;
        let _ = update(&mut a, Message::Engine(UiEvent::SetupResult(Ok(()))));
        assert!(a.setup.is_none());
        assert_eq!(a.status, "Connected to Google");

        // Failure keeps the screen and surfaces the error.
        a.setup = Some(SetupState::new("id".into(), "sec".into()));
        a.setup.as_mut().unwrap().submitting = true;
        let _ = update(
            &mut a,
            Message::Engine(UiEvent::SetupResult(Err("bad".into()))),
        );
        let s = a.setup.as_ref().unwrap();
        assert!(!s.submitting);
        assert_eq!(s.error.as_deref(), Some("bad"));
    }

    #[test]
    fn view_renders_setup_when_present() {
        let (mut a, _rx) = app();
        let _ = update(&mut a, Message::Engine(UiEvent::ToggleWidget));
        let id = a.widget.unwrap();
        a.setup = Some(SetupState::new("id".into(), "sec".into()));
        // Setup takes priority over every other pane.
        let _ = view(&a, id);
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
        // Detail mode, including the expanded delete prompt.
        a.form.open = false;
        a.detail = Some(DetailState::Loaded(details()));
        a.selected_instance = Some("inst".into());
        a.delete_prompt = true;
        let _ = view(&a, id);
        assert_eq!(title(&a, id), "Calendar");
        let _ = theme(&a, id);
    }
}
