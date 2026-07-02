//! The "today at a glance" agenda: a header with sync/add controls, per-calendar
//! visibility chips, and a scrollable, color-coded list of today's events.

use chrono::Local;
use iced::widget::{button, checkbox, column, container, row, scrollable, text, Space};
use iced::{Alignment, Color, Element, Length};

use crate::app::{App, Message};
use crate::google::model::Occurrence;

pub fn view(app: &App) -> Element<'_, Message> {
    let header = row![
        text("Today").size(20),
        iced::widget::space::horizontal(),
        button(text(&app.status).size(12)).on_press(Message::SyncNow),
    ]
    .align_y(Alignment::Center)
    .spacing(8);

    // Per-calendar visibility chips.
    let mut chips = row![].spacing(8);
    for cal in &app.calendars {
        let id = cal.id.clone();
        chips = chips.push(
            checkbox(cal.visible)
                .label(cal.summary.clone())
                .on_toggle(move |v| Message::SetCalendarVisible(id.clone(), v)),
        );
    }

    // Today's occurrences from visible calendars.
    let today = Local::now().date_naive();
    let visible_ids: std::collections::HashSet<&str> = app
        .calendars
        .iter()
        .filter(|c| c.visible)
        .map(|c| c.id.as_str())
        .collect();

    let mut list = column![].spacing(6);
    let mut count = 0;
    for occ in &app.occurrences {
        if occ.start.date_naive() != today {
            continue;
        }
        if !visible_ids.contains(occ.calendar_id.as_str()) {
            continue;
        }
        list = list.push(event_row(app, occ));
        count += 1;
    }
    if count == 0 {
        list = list.push(text("No more events today 🎉").size(14));
    }

    let add_btn = button(text("＋ Add event")).on_press(Message::OpenAddForm);

    let body = column![
        header,
        chips,
        iced::widget::rule::horizontal(1),
        scrollable(list).height(Length::Fill),
        row![iced::widget::space::horizontal(), add_btn],
    ]
    .spacing(10)
    .padding(4);

    container(body).padding(8).height(Length::Fill).into()
}

fn event_row<'a>(app: &'a App, occ: &'a Occurrence) -> Element<'a, Message> {
    let dot_color = app
        .calendars
        .iter()
        .find(|c| c.id == occ.calendar_id)
        .map(|c| parse_hex(&c.color))
        .unwrap_or(Color::from_rgb(0.5, 0.5, 0.5));

    let when = if occ.all_day {
        "All day".to_string()
    } else if occ.end > occ.start {
        format!("{}–{}", occ.start.format("%H:%M"), occ.end.format("%H:%M"))
    } else {
        occ.start.format("%H:%M").to_string()
    };

    let dot = container(
        Space::new()
            .width(Length::Fixed(10.0))
            .height(Length::Fixed(10.0)),
    )
    .style(move |_| container::Style {
        background: Some(dot_color.into()),
        border: iced::border::rounded(5),
        ..Default::default()
    });

    let mut title_col = column![text(occ.title.clone()).size(15)].spacing(1);
    if let Some(loc) = &occ.location {
        if !loc.is_empty() {
            title_col = title_col.push(text(format!("📍 {loc}")).size(11));
        }
    }

    row![
        text(when).size(13).width(Length::Fixed(96.0)),
        container(dot).center_y(Length::Fixed(20.0)),
        title_col,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// Parse `#RRGGBB` into an iced Color; falls back to gray.
fn parse_hex(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        ) {
            return Color::from_rgb8(r, g, b);
        }
    }
    Color::from_rgb(0.5, 0.5, 0.5)
}
