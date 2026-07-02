//! The event detail pane: shown when an agenda row is selected. It takes over
//! the widget window (like the add-event form) and offers Back + Edit. The full
//! event is fetched on selection, so it can show description, guests, and
//! recurrence that the lightweight sync doesn't carry.

use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Alignment, Color, Element, Length};

use crate::app::{DetailState, Message};
use crate::engine::CalendarView;
use crate::google::model::EventDetails;
use crate::ui::agenda::{format_when, parse_hex};

pub fn view<'a>(
    detail: &'a DetailState,
    calendars: &'a [CalendarView],
    delete_prompt: bool,
    selected_instance: Option<&'a str>,
) -> Element<'a, Message> {
    let title: &str = match detail {
        DetailState::Loading { title } => title,
        DetailState::Loaded(d) => &d.title,
        DetailState::Failed { title, .. } => title,
    };

    let mut header = row![
        button("‹ Back").on_press(Message::CloseDetail),
        iced::widget::space::horizontal(),
    ]
    .align_y(Alignment::Center)
    .spacing(8);
    // Edit + delete actions are only offered once the full event has loaded.
    if let DetailState::Loaded(d) = detail {
        for action in header_actions(d, delete_prompt, selected_instance) {
            header = header.push(action);
        }
    }

    let body = match detail {
        DetailState::Loading { .. } => column![text("Loading…").size(14)].spacing(8),
        DetailState::Failed { message, .. } => column![
            text(title).size(20),
            text(format!("Couldn't load event: {message}")).size(13),
        ]
        .spacing(8),
        DetailState::Loaded(d) => loaded_body(d, calendars),
    };

    let content = column![header, iced::widget::rule::horizontal(1), body]
        .spacing(14)
        .padding(4);

    container(scrollable(content).height(Length::Fill))
        .padding(12)
        .height(Length::Fill)
        .into()
}

/// The header's right-side buttons for a loaded event. Normally `Edit` + a
/// `Delete` that expands (inline, no modal) into a confirmation: a single
/// `Confirm delete` for one-off events, or a `this event` / `all events` choice
/// for recurring ones (the scope choice doubles as the confirmation).
fn header_actions<'a>(
    d: &'a EventDetails,
    delete_prompt: bool,
    selected_instance: Option<&'a str>,
) -> Vec<Element<'a, Message>> {
    if !delete_prompt {
        return vec![
            button("✎ Edit").on_press(Message::EditSelected).into(),
            button("🗑 Delete")
                .on_press(Message::RequestDelete)
                .style(button::danger)
                .into(),
        ];
    }

    let cancel = button("Cancel").on_press(Message::CancelDelete).into();

    if d.recurrence.is_empty() {
        vec![
            button("⚠ Confirm delete")
                .on_press(Message::ConfirmDelete {
                    calendar_id: d.calendar_id.clone(),
                    event_id: d.event_id.clone(),
                })
                .style(button::danger)
                .into(),
            cancel,
        ]
    } else {
        // "this event" needs the per-instance id; fall back to the series master
        // if it's somehow absent. "all events" targets the master.
        let instance_id = selected_instance.unwrap_or(&d.event_id).to_string();
        vec![
            button("Delete this event")
                .on_press(Message::ConfirmDelete {
                    calendar_id: d.calendar_id.clone(),
                    event_id: instance_id,
                })
                .style(button::danger)
                .into(),
            button("Delete all events")
                .on_press(Message::ConfirmDelete {
                    calendar_id: d.calendar_id.clone(),
                    event_id: d.event_id.clone(),
                })
                .style(button::danger)
                .into(),
            cancel,
        ]
    }
}

fn loaded_body<'a>(
    d: &'a EventDetails,
    calendars: &'a [CalendarView],
) -> iced::widget::Column<'a, Message> {
    let dot_color = calendars
        .iter()
        .find(|c| c.id == d.calendar_id)
        .map(|c| parse_hex(&c.color))
        .unwrap_or(Color::from_rgb(0.5, 0.5, 0.5));

    let dot = container(
        Space::new()
            .width(Length::Fixed(12.0))
            .height(Length::Fixed(12.0)),
    )
    .style(move |_| container::Style {
        background: Some(dot_color.into()),
        border: iced::border::rounded(6),
        ..Default::default()
    });

    let title_row = row![
        container(dot).center_y(Length::Fixed(24.0)),
        text(d.title.clone()).size(20),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut body = column![
        title_row,
        text(format_when(d.start, d.end, d.all_day)).size(14)
    ]
    .spacing(10);

    if let Some(loc) = &d.location {
        if !loc.is_empty() {
            body = body.push(text(format!("📍 {loc}")).size(13));
        }
    }
    if let Some(desc) = &d.description {
        if !desc.is_empty() {
            body = body.push(text(format!("Notes: {desc}")).size(13));
        }
    }
    if !d.attendees.is_empty() {
        body = body.push(text(format!("Guests: {}", d.attendees.join(", "))).size(13));
    }
    if !d.recurrence.is_empty() {
        body = body.push(text(format!("Repeats: {}", d.recurrence.join("; "))).size(12));
    }

    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    fn cals() -> Vec<CalendarView> {
        vec![CalendarView {
            id: "p".into(),
            summary: "Primary".into(),
            color: "#4285F4".into(),
            primary: true,
            visible: true,
            notify: true,
        }]
    }

    fn details() -> EventDetails {
        let start = Local::now();
        EventDetails {
            calendar_id: "p".into(),
            event_id: "evt".into(),
            title: "Standup".into(),
            location: Some("Room 1".into()),
            description: Some("notes".into()),
            all_day: false,
            start,
            end: start + chrono::Duration::hours(1),
            attendees: vec!["a@x.com".into(), "b@y.com".into()],
            recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
        }
    }

    #[test]
    fn view_builds_in_every_state() {
        let _ = view(
            &DetailState::Loading { title: "T".into() },
            &cals(),
            false,
            None,
        );
        let _ = view(&DetailState::Loaded(details()), &cals(), false, None);
        let _ = view(
            &DetailState::Failed {
                title: "T".into(),
                message: "boom".into(),
            },
            &cals(),
            false,
            None,
        );
    }

    #[test]
    fn view_builds_delete_confirmation_states() {
        // One-off event: single confirm button.
        let _ = view(&DetailState::Loaded(details()), &cals(), true, Some("inst"));

        // Recurring event: this-event / all-events scope choice.
        let mut d = details();
        d.recurrence = vec!["RRULE:FREQ=DAILY".into()];
        let _ = view(&DetailState::Loaded(d), &cals(), true, Some("inst-123"));

        // Recurring but no instance id available: falls back to the master.
        let mut d = details();
        d.recurrence = vec!["RRULE:FREQ=DAILY".into()];
        let _ = view(&DetailState::Loaded(d), &cals(), true, None);
    }

    #[test]
    fn header_actions_switch_on_prompt_and_recurrence() {
        let mut one_off = details();
        one_off.recurrence = vec![];
        assert_eq!(header_actions(&one_off, false, None).len(), 2); // Edit + Delete
        assert_eq!(header_actions(&one_off, true, None).len(), 2); // Confirm + Cancel

        let recurring = details(); // details() carries an RRULE
        assert!(!recurring.recurrence.is_empty());
        // this event + all events + cancel
        assert_eq!(header_actions(&recurring, true, Some("inst")).len(), 3);
    }

    #[test]
    fn loaded_body_handles_missing_optional_fields() {
        // Empty/None location, description, guests, recurrence, and an all-day
        // event on a calendar with no color match (falls back to gray).
        let start = Local::now();
        let d = EventDetails {
            calendar_id: "unknown".into(),
            event_id: "e".into(),
            title: "Bare".into(),
            location: None,
            description: Some(String::new()),
            all_day: true,
            start,
            end: start,
            attendees: vec![],
            recurrence: vec![],
        };
        let _ = view(&DetailState::Loaded(d), &cals(), false, None);
    }
}
