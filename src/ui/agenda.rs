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
    // Validate ASCII-hex on the raw bytes *before* the byte-index slices below.
    // `h.len()` counts bytes, so a 6-byte non-ASCII string (e.g. a multi-byte
    // UTF-8 char) would otherwise let the slices split a char boundary and
    // panic. `is_ascii_hexdigit` guarantees every byte is one ASCII char, so
    // the 0..2 / 2..4 / 4..6 slices land on boundaries.
    let bytes = h.as_bytes();
    if bytes.len() == 6 && bytes.iter().all(u8::is_ascii_hexdigit) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::engine::CalendarView;
    use crate::google::model::Occurrence;
    use chrono::{Duration, Local};
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn non_ascii_six_byte_color_does_not_panic() {
        // "€abc" is 6 bytes, but byte offsets 2 and 4 fall inside the 3-byte
        // '€'. The old slice-based parse panicked on this; now it falls back
        // to gray without panicking.
        let c = parse_hex("€abc");
        assert!((c.r - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn valid_hex_parses() {
        let c = parse_hex("#4285F4");
        assert_eq!(
            (c.r, c.g, c.b),
            (
                0x42 as f32 / 255.0,
                0x85 as f32 / 255.0,
                0xF4 as f32 / 255.0
            )
        );
    }

    #[test]
    fn parse_hex_short_or_invalid_is_gray() {
        for bad in ["", "#12", "gggggg", "#12345"] {
            let c = parse_hex(bad);
            assert!((c.r - 0.5).abs() < f32::EPSILON);
        }
    }

    fn cal(id: &str, color: &str, visible: bool) -> CalendarView {
        CalendarView {
            id: id.into(),
            summary: id.into(),
            color: color.into(),
            primary: false,
            visible,
            notify: true,
        }
    }

    fn occ(cal_id: &str, all_day: bool, loc: Option<&str>, offset_h: i64) -> Occurrence {
        let start = Local::now() + Duration::hours(offset_h);
        Occurrence {
            event_id: "e".into(),
            calendar_id: cal_id.into(),
            title: "Event".into(),
            location: loc.map(|s| s.into()),
            start,
            end: start + Duration::hours(1),
            all_day,
            reminders: vec![],
        }
    }

    fn app_with(cals: Vec<CalendarView>, occs: Vec<Occurrence>) -> App {
        let (tx, _rx) = unbounded_channel();
        let mut app = App::new(tx);
        app.calendars = cals;
        app.occurrences = occs;
        app
    }

    #[test]
    fn view_renders_todays_events() {
        // A timed event with location, an all-day event, and an event on a
        // hidden calendar (filtered out) + one on another day (filtered out).
        let app = app_with(
            vec![cal("p", "#4285F4", true), cal("h", "", false)],
            vec![
                occ("p", false, Some("Room 1"), 0),
                occ("p", true, None, 1),
                occ("h", false, None, 0),  // hidden calendar
                occ("p", false, None, 48), // not today
            ],
        );
        let _ = view(&app);
    }

    #[test]
    fn view_empty_state() {
        let app = app_with(vec![cal("p", "#4285F4", true)], vec![]);
        let _ = view(&app);
    }
}
