//! First-run / re-configure screen: paste the OAuth **Client ID** and **Client
//! secret** obtained from Google Cloud Console. Saving them drives the engine's
//! [`Command::SaveCredentials`](crate::engine::Command::SaveCredentials), which
//! authorizes and (on success) starts syncing.
//!
//! The GCP client itself still has to be created in the Cloud Console — the app
//! can't generate it — so the screen carries a one-line hint, a link to the
//! console, and a *Learn more* checklist of the steps.

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

use crate::app::Message;

/// The Google Cloud Console landing page, opened by the *Open Google Cloud
/// Console* link.
pub const CONSOLE_URL: &str = "https://console.cloud.google.com/";

/// State of the credential-setup screen.
#[derive(Debug, Clone, Default)]
pub struct SetupState {
    pub client_id: String,
    pub client_secret: String,
    /// Inline error from the last save attempt (bad credentials, denied, offline).
    pub error: Option<String>,
    /// A save is in flight (waiting on OAuth / Google), so the button is disabled.
    pub submitting: bool,
    /// Showing the GCP steps checklist instead of the credential form.
    pub show_help: bool,
}

/// Field-level messages emitted by the setup screen.
#[derive(Debug, Clone)]
pub enum SetupMsg {
    ClientId(String),
    ClientSecret(String),
    ShowHelp(bool),
}

impl SetupState {
    /// Open the screen prefilled with the current (possibly empty) credentials.
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            ..Self::default()
        }
    }

    /// Apply a field edit, clearing any stale error.
    pub fn update(&mut self, msg: SetupMsg) {
        self.error = None;
        match msg {
            SetupMsg::ClientId(v) => self.client_id = v,
            SetupMsg::ClientSecret(v) => self.client_secret = v,
            SetupMsg::ShowHelp(v) => self.show_help = v,
        }
    }
}

// -- view ------------------------------------------------------------------

pub fn view(state: &SetupState) -> Element<'_, Message> {
    let body = if state.show_help {
        help_view()
    } else {
        form_view(state)
    };
    container(scrollable(body).height(Length::Fill))
        .padding(12)
        .height(Length::Fill)
        .into()
}

fn form_view(state: &SetupState) -> Element<'_, Message> {
    let link = button(text("Open Google Cloud Console ↗").size(12))
        .style(button::text)
        .padding(0)
        .on_press(Message::OpenSetupConsole);
    let learn_more = button(text("Learn more").size(12))
        .style(button::text)
        .padding(0)
        .on_press(Message::Setup(SetupMsg::ShowHelp(true)));

    let mut content = column![
        text("Connect Google Calendar").size(18),
        text("Create a Desktop-app OAuth client in Google Cloud Console, then paste its Client ID and secret below.")
            .size(12),
        row![link, learn_more].spacing(16),
        column![
            text("Client ID").size(12),
            text_input("xxxxxx.apps.googleusercontent.com", &state.client_id)
                .on_input(|s| Message::Setup(SetupMsg::ClientId(s))),
        ]
        .spacing(2),
        column![
            text("Client secret").size(12),
            text_input("client secret", &state.client_secret)
                .secure(true)
                .on_input(|s| Message::Setup(SetupMsg::ClientSecret(s))),
        ]
        .spacing(2),
    ]
    .spacing(12)
    .padding(4);

    if state.submitting {
        content = content.push(text("Waiting for Google sign-in in your browser…").size(12));
    }
    if let Some(err) = &state.error {
        content = content.push(text(err.clone()).size(13));
    }

    let mut save = button(if state.submitting {
        "Connecting…"
    } else {
        "Save"
    });
    if !state.submitting {
        save = save.on_press(Message::SubmitSetup);
    }
    // Cancel stays active even while submitting — on purpose. The engine handles
    // SaveCredentials by blocking on the interactive OAuth consent, so during a
    // slow/abandoned sign-in the engine (and thus the tray) is unresponsive;
    // Cancel is a pure UI-thread message, so it's the user's only escape from the
    // "Connecting…" screen. It closes the screen but can't abort the in-flight
    // OAuth — a genuine abort needs the non-blocking rework tracked separately.
    let cancel = button("Cancel").on_press(Message::CancelSetup);
    content = content.push(row![iced::widget::space::horizontal(), cancel, save].spacing(8));

    content.into()
}

/// The steps needed in Google Cloud Console, mirroring README → "Google Cloud
/// OAuth setup".
const STEPS: [&str; 5] = [
    "1. Create a project at Google Cloud Console.",
    "2. APIs & Services → Library → enable the Google Calendar API.",
    "3. APIs & Services → OAuth consent screen → choose External, fill the minimal details, and add your Google account under Test users.",
    "4. APIs & Services → Credentials → Create credentials → OAuth client ID → Desktop app.",
    "5. Copy the Client ID and Client secret, then paste them on the previous screen.",
];

fn help_view() -> Element<'static, Message> {
    let mut content = column![text("Get your OAuth credentials").size(18)]
        .spacing(10)
        .padding(4);
    for step in STEPS {
        content = content.push(text(step).size(13));
    }
    content = content.push(
        button(text("Open Google Cloud Console ↗").size(12))
            .style(button::text)
            .padding(0)
            .on_press(Message::OpenSetupConsole),
    );
    content = content.push(row![
        iced::widget::space::horizontal(),
        button("Back").on_press(Message::Setup(SetupMsg::ShowHelp(false))),
    ]);
    content.into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    #[test]
    fn new_prefills_credentials() {
        let s = SetupState::new("id".into(), "sec".into());
        assert_eq!(s.client_id, "id");
        assert_eq!(s.client_secret, "sec");
        assert!(!s.show_help);
        assert!(s.error.is_none());
    }

    #[test]
    fn update_sets_fields_and_clears_error() {
        let mut s = SetupState::default();
        s.error = Some("stale".into());
        s.update(SetupMsg::ClientId("id".into()));
        assert_eq!(s.client_id, "id");
        assert!(s.error.is_none(), "editing clears the previous error");
        s.update(SetupMsg::ClientSecret("sec".into()));
        assert_eq!(s.client_secret, "sec");
        s.update(SetupMsg::ShowHelp(true));
        assert!(s.show_help);
    }

    #[test]
    fn view_builds_in_form_and_help_modes() {
        // Empty form.
        let _ = view(&SetupState::default());

        // Submitting, with an error and prefilled values.
        let mut s = SetupState::new("id".into(), "sec".into());
        s.submitting = true;
        s.error = Some("bad credentials".into());
        let _ = view(&s);

        // Help checklist.
        s.show_help = true;
        let _ = view(&s);
    }
}
