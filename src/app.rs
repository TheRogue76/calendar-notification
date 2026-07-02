//! The iced daemon: application state, the update loop, the subscription that
//! bridges engine events + window-close events into messages, and the widget
//! window lifecycle.
//!
//! Runs on the main thread with iced's own executor; all Google/tray/scheduler
//! work happens on a separate background runtime (see `main.rs`) and reaches us
//! only through `UI_RX` (engine → UI) and `cmd_tx` (UI → engine).

use std::sync::{Mutex, OnceLock};

use iced::futures::{SinkExt, Stream};
use iced::widget::text;
use iced::{window, Element, Size, Subscription, Task, Theme};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::engine::{CalendarView, Command, UiEvent};
use crate::google::model::Occurrence;
use crate::ui::add_event::{self, FormMsg, FormState};
use crate::ui::agenda;

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
}

pub struct App {
    cmd_tx: UnboundedSender<Command>,
    pub calendars: Vec<CalendarView>,
    pub occurrences: Vec<Occurrence>,
    pub status: String,
    pub widget: Option<window::Id>,
    pub form: FormState,
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
                exit_on_close_request: false,
                ..window::Settings::default()
            };
            let (id, open) = window::open(settings);
            self.widget = Some(id);
            open.discard()
        }
    }
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
                    app.send(Command::InsertEvent(new));
                }
                Err(e) => app.form.error = Some(e),
            }
            Task::none()
        }
        Message::SetCalendarVisible(id, v) => {
            app.send(Command::SetVisible(id, v));
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

// A tiny helper so the module has a place to surface build errors during dev.
#[allow(dead_code)]
fn _assert_element(app: &App) -> Element<'_, Message> {
    text(app.status.clone()).into()
}
