//! The add-event form: state, field messages, view, and assembly into a
//! [`NewEvent`] for the engine to insert.

use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Length};

use crate::app::Message;
use crate::engine::CalendarView;
use crate::google::model::{EventDetails, NewEvent};
use crate::ui::recurrence::{Recurrence, RecurrenceKind, Weekdays};

/// Identifies the event an open form is editing (vs. creating a new one).
#[derive(Debug, Clone)]
pub struct EditTarget {
    pub calendar_id: String,
    pub event_id: String,
}

/// A calendar option for the pick_list (Display = its name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarChoice {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for CalendarChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// Field-level messages emitted by the form.
#[derive(Debug, Clone)]
pub enum FormMsg {
    Title(String),
    AllDay(bool),
    StartDate(String),
    StartTime(String),
    EndDate(String),
    EndTime(String),
    Location(String),
    Description(String),
    Guests(String),
    Calendar(CalendarChoice),
    Recurrence(RecurrenceKind),
    ToggleWeekday(usize, bool),
    UntilEnabled(bool),
    UntilDate(String),
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub open: bool,
    pub title: String,
    pub all_day: bool,
    pub start_date: String,
    pub start_time: String,
    pub end_date: String,
    pub end_time: String,
    pub location: String,
    pub description: String,
    pub guests: String,
    pub calendar_id: Option<String>,
    pub recurrence: RecurrenceKind,
    pub weekdays: Weekdays,
    pub until_enabled: bool,
    pub until_date: String,
    pub error: Option<String>,
    pub submitting: bool,
    /// `Some` when the form is editing an existing event rather than creating
    /// one; carries the ids to patch. Cleared by [`reset`](FormState::reset).
    pub editing: Option<EditTarget>,
}

impl Default for FormState {
    fn default() -> Self {
        let now = Local::now();
        let today = now.date_naive();
        let next_hour = (now + chrono::Duration::hours(1))
            .format("%H:00")
            .to_string();
        let hour_after = (now + chrono::Duration::hours(2))
            .format("%H:00")
            .to_string();
        Self {
            open: false,
            title: String::new(),
            all_day: false,
            start_date: today.format("%Y-%m-%d").to_string(),
            start_time: next_hour,
            end_date: today.format("%Y-%m-%d").to_string(),
            end_time: hour_after,
            location: String::new(),
            description: String::new(),
            guests: String::new(),
            calendar_id: None,
            recurrence: RecurrenceKind::None,
            weekdays: Weekdays::from_date(today),
            until_enabled: false,
            until_date: today.format("%Y-%m-%d").to_string(),
            error: None,
            submitting: false,
            editing: None,
        }
    }
}

impl FormState {
    /// Reset to a fresh form, preserving the selected calendar. Also clears the
    /// edit target (via `default`), so the next submit creates rather than edits.
    pub fn reset(&mut self) {
        let keep = self.calendar_id.clone();
        *self = FormState::default();
        self.calendar_id = keep;
    }

    /// Build a form pre-filled from a fetched event, in edit mode. The recurrence
    /// controls stay at their defaults and are hidden in the view — the series
    /// RRULE is preserved server-side rather than re-derived from the presets.
    pub fn prefill(details: &EventDetails) -> Self {
        FormState {
            editing: Some(EditTarget {
                calendar_id: details.calendar_id.clone(),
                event_id: details.event_id.clone(),
            }),
            title: details.title.clone(),
            all_day: details.all_day,
            start_date: details.start.format("%Y-%m-%d").to_string(),
            start_time: details.start.format("%H:%M").to_string(),
            end_date: details.end.format("%Y-%m-%d").to_string(),
            end_time: details.end.format("%H:%M").to_string(),
            location: details.location.clone().unwrap_or_default(),
            description: details.description.clone().unwrap_or_default(),
            guests: details.attendees.join(", "),
            calendar_id: Some(details.calendar_id.clone()),
            ..FormState::default()
        }
    }

    /// Apply a field edit.
    pub fn update(&mut self, msg: FormMsg) {
        self.error = None;
        match msg {
            FormMsg::Title(v) => self.title = v,
            FormMsg::AllDay(v) => self.all_day = v,
            FormMsg::StartDate(v) => self.start_date = v,
            FormMsg::StartTime(v) => self.start_time = v,
            FormMsg::EndDate(v) => self.end_date = v,
            FormMsg::EndTime(v) => self.end_time = v,
            FormMsg::Location(v) => self.location = v,
            FormMsg::Description(v) => self.description = v,
            FormMsg::Guests(v) => self.guests = v,
            FormMsg::Calendar(c) => self.calendar_id = Some(c.id),
            FormMsg::Recurrence(r) => self.recurrence = r,
            FormMsg::ToggleWeekday(i, on) => {
                if i < 7 {
                    self.weekdays.days[i] = on;
                }
            }
            FormMsg::UntilEnabled(v) => self.until_enabled = v,
            FormMsg::UntilDate(v) => self.until_date = v,
        }
    }

    /// Build a [`NewEvent`] from the current fields, or an error message.
    pub fn build(&self, calendars: &[CalendarView]) -> Result<NewEvent, String> {
        if self.title.trim().is_empty() {
            return Err("Title is required".into());
        }
        let calendar_id = self
            .calendar_id
            .clone()
            .or_else(|| calendars.iter().find(|c| c.primary).map(|c| c.id.clone()))
            .or_else(|| calendars.first().map(|c| c.id.clone()))
            .ok_or("No calendar available")?;

        let start_date = parse_date(&self.start_date, "start date")?;
        let end_date = parse_date(&self.end_date, "end date")?;

        let (start, end) = if self.all_day {
            (local_midnight(start_date)?, local_midnight(end_date)?)
        } else {
            let start_time = parse_time(&self.start_time, "start time")?;
            let end_time = parse_time(&self.end_time, "end time")?;
            let start = local_datetime(start_date, start_time)?;
            let end = local_datetime(end_date, end_time)?;
            if end <= start {
                return Err("End must be after start".into());
            }
            (start, end)
        };

        let recurrence = self.recurrence_rule().to_rrule(self.until());

        let attendees = self
            .guests
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        Ok(NewEvent {
            calendar_id,
            title: self.title.trim().to_string(),
            location: non_empty(&self.location),
            description: non_empty(&self.description),
            all_day: self.all_day,
            start,
            end,
            attendees,
            recurrence,
        })
    }

    fn recurrence_rule(&self) -> Recurrence {
        match self.recurrence {
            RecurrenceKind::None => Recurrence::None,
            RecurrenceKind::Daily => Recurrence::Daily,
            RecurrenceKind::Weekly => Recurrence::Weekly(self.weekdays),
            RecurrenceKind::Monthly => Recurrence::Monthly,
            RecurrenceKind::Yearly => Recurrence::Yearly,
        }
    }

    fn until(&self) -> Option<NaiveDate> {
        if self.until_enabled {
            NaiveDate::parse_from_str(&self.until_date, "%Y-%m-%d").ok()
        } else {
            None
        }
    }
}

// -- view ------------------------------------------------------------------

const WEEKDAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub fn view<'a>(form: &'a FormState, calendars: &'a [CalendarView]) -> Element<'a, Message> {
    let field = |label: &'a str, input: Element<'a, Message>| {
        column![text(label).size(12), input].spacing(2)
    };

    let editing = form.editing.is_some();
    let heading = if editing { "Edit event" } else { "New event" };

    let mut content = column![
        row![
            text(heading).size(18),
            iced::widget::space::horizontal(),
            button("Cancel").on_press(Message::CloseAddForm),
        ]
        .align_y(Alignment::Center),
        field(
            "Title",
            text_input("Event title", &form.title)
                .on_input(|s| Message::Form(FormMsg::Title(s)))
                .into(),
        ),
        checkbox(form.all_day)
            .label("All day")
            .on_toggle(|v| Message::Form(FormMsg::AllDay(v))),
    ]
    .spacing(10)
    .padding(4);

    // Start / end rows
    if form.all_day {
        content = content.push(field(
            "Start date",
            text_input("YYYY-MM-DD", &form.start_date)
                .on_input(|s| Message::Form(FormMsg::StartDate(s)))
                .into(),
        ));
        content = content.push(field(
            "End date",
            text_input("YYYY-MM-DD", &form.end_date)
                .on_input(|s| Message::Form(FormMsg::EndDate(s)))
                .into(),
        ));
    } else {
        content = content.push(field(
            "Start",
            row![
                text_input("YYYY-MM-DD", &form.start_date)
                    .on_input(|s| Message::Form(FormMsg::StartDate(s))),
                text_input("HH:MM", &form.start_time)
                    .on_input(|s| Message::Form(FormMsg::StartTime(s)))
                    .width(Length::Fixed(90.0)),
            ]
            .spacing(6)
            .into(),
        ));
        content = content.push(field(
            "End",
            row![
                text_input("YYYY-MM-DD", &form.end_date)
                    .on_input(|s| Message::Form(FormMsg::EndDate(s))),
                text_input("HH:MM", &form.end_time)
                    .on_input(|s| Message::Form(FormMsg::EndTime(s)))
                    .width(Length::Fixed(90.0)),
            ]
            .spacing(6)
            .into(),
        ));
    }

    content = content.push(field(
        "Location",
        text_input("Where", &form.location)
            .on_input(|s| Message::Form(FormMsg::Location(s)))
            .into(),
    ));
    content = content.push(field(
        "Description",
        text_input("Notes", &form.description)
            .on_input(|s| Message::Form(FormMsg::Description(s)))
            .into(),
    ));
    content = content.push(field(
        "Guests (comma-separated emails)",
        text_input("a@x.com, b@y.com", &form.guests)
            .on_input(|s| Message::Form(FormMsg::Guests(s)))
            .into(),
    ));

    // Calendar picker
    let choices: Vec<CalendarChoice> = calendars
        .iter()
        .map(|c| CalendarChoice {
            id: c.id.clone(),
            name: c.summary.clone(),
        })
        .collect();
    let selected = form
        .calendar_id
        .as_ref()
        .and_then(|id| choices.iter().find(|c| &c.id == id).cloned())
        .or_else(|| {
            calendars
                .iter()
                .find(|c| c.primary)
                .map(|c| CalendarChoice {
                    id: c.id.clone(),
                    name: c.summary.clone(),
                })
        });
    content = content.push(field(
        "Calendar",
        pick_list(choices, selected, |c| Message::Form(FormMsg::Calendar(c))).into(),
    ));

    // Recurrence controls (creating only). When editing, the existing series
    // RRULE is preserved server-side, so we hide these rather than round-trip an
    // arbitrary RRULE through the preset picker.
    if !editing {
        content = content.push(field(
            "Repeat",
            pick_list(Recurrence::ALL.to_vec(), Some(form.recurrence), |r| {
                Message::Form(FormMsg::Recurrence(r))
            })
            .into(),
        ));

        // Weekday selector (weekly only)
        if form.recurrence == RecurrenceKind::Weekly {
            let mut days = row![].spacing(4);
            for (i, label) in WEEKDAY_LABELS.iter().enumerate() {
                days = days.push(
                    checkbox(form.weekdays.days[i])
                        .label(*label)
                        .on_toggle(move |v| Message::Form(FormMsg::ToggleWeekday(i, v))),
                );
            }
            content = content.push(days);
        }

        // Optional until-date
        if form.recurrence != RecurrenceKind::None {
            content = content.push(
                checkbox(form.until_enabled)
                    .label("Ends on")
                    .on_toggle(|v| Message::Form(FormMsg::UntilEnabled(v))),
            );
            if form.until_enabled {
                content = content.push(
                    text_input("YYYY-MM-DD", &form.until_date)
                        .on_input(|s| Message::Form(FormMsg::UntilDate(s))),
                );
            }
        }
    }

    if let Some(err) = &form.error {
        content = content.push(text(err.clone()).size(13));
    }

    let submit_label = if form.submitting {
        "Saving…"
    } else if editing {
        "Save changes"
    } else {
        "Save event"
    };
    let mut submit = button(submit_label);
    if !form.submitting {
        submit = submit.on_press(Message::SubmitForm);
    }
    content = content.push(row![iced::widget::space::horizontal(), submit]);

    container(scrollable_body(content)).padding(8).into()
}

fn scrollable_body(content: iced::widget::Column<'_, Message>) -> Element<'_, Message> {
    iced::widget::scrollable(content)
        .height(Length::Fill)
        .into()
}

// -- parsing helpers -------------------------------------------------------

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_date(s: &str, what: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| format!("Invalid {what} (use YYYY-MM-DD)"))
}

fn parse_time(s: &str, what: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").map_err(|_| format!("Invalid {what} (use HH:MM)"))
}

fn local_datetime(date: NaiveDate, time: NaiveTime) -> Result<chrono::DateTime<Local>, String> {
    Local
        .from_local_datetime(&date.and_time(time))
        .single()
        .ok_or_else(|| "Ambiguous local time".into())
}

fn local_midnight(date: NaiveDate) -> Result<chrono::DateTime<Local>, String> {
    // 00:00:00 is always a valid time, so this unwrap can never fire.
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is a valid time");
    local_datetime(date, midnight)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    fn cals() -> Vec<CalendarView> {
        vec![
            CalendarView {
                id: "p".into(),
                summary: "Primary".into(),
                color: "#ffffff".into(),
                primary: true,
                visible: true,
                notify: true,
            },
            CalendarView {
                id: "w".into(),
                summary: "Work".into(),
                color: "#0000ff".into(),
                primary: false,
                visible: true,
                notify: true,
            },
        ]
    }

    /// A deterministic, valid timed form (doesn't depend on wall-clock).
    fn valid_form() -> FormState {
        let mut f = FormState::default();
        f.title = "Standup".into();
        f.start_date = "2026-07-02".into();
        f.start_time = "09:00".into();
        f.end_date = "2026-07-02".into();
        f.end_time = "09:30".into();
        f
    }

    #[test]
    fn build_requires_title() {
        let f = FormState::default();
        assert_eq!(f.build(&cals()).unwrap_err(), "Title is required");
    }

    #[test]
    fn build_valid_timed_event() {
        let ev = valid_form().build(&cals()).unwrap();
        assert_eq!(ev.title, "Standup");
        assert_eq!(ev.calendar_id, "p"); // defaults to primary
        assert!(!ev.all_day);
        assert!(ev.recurrence.is_empty());
        assert!(ev.end > ev.start);
    }

    #[test]
    fn build_defaults_to_primary_then_first() {
        let mut f = valid_form();
        f.calendar_id = None;
        // primary present -> "p"
        assert_eq!(f.build(&cals()).unwrap().calendar_id, "p");
        // no primary -> first
        let no_primary = vec![CalendarView {
            id: "only".into(),
            summary: "Only".into(),
            color: String::new(),
            primary: false,
            visible: true,
            notify: true,
        }];
        assert_eq!(f.build(&no_primary).unwrap().calendar_id, "only");
    }

    #[test]
    fn build_no_calendar_errors() {
        let mut f = valid_form();
        f.calendar_id = None;
        assert_eq!(f.build(&[]).unwrap_err(), "No calendar available");
    }

    #[test]
    fn build_rejects_bad_dates_times_and_ordering() {
        let mut f = valid_form();
        f.start_date = "nope".into();
        assert!(f.build(&cals()).unwrap_err().contains("start date"));

        let mut f = valid_form();
        f.start_time = "25:99".into();
        assert!(f.build(&cals()).unwrap_err().contains("start time"));

        let mut f = valid_form();
        f.end_time = "08:00".into(); // before start
        assert_eq!(f.build(&cals()).unwrap_err(), "End must be after start");
    }

    #[test]
    fn build_all_day_ignores_time() {
        let mut f = valid_form();
        f.all_day = true;
        f.end_time = "00:00".into(); // would be invalid for timed, fine for all-day
        let ev = f.build(&cals()).unwrap();
        assert!(ev.all_day);
    }

    #[test]
    fn build_parses_attendees_and_recurrence() {
        let mut f = valid_form();
        f.guests = " a@x.com , ,b@y.com ".into();
        f.location = "  ".into(); // blank -> None
        f.description = "note".into();
        f.recurrence = RecurrenceKind::Weekly;
        f.weekdays = Weekdays {
            days: [true, false, false, false, false, false, false],
        };
        f.until_enabled = true;
        f.until_date = "2026-12-31".into();
        let ev = f.build(&cals()).unwrap();
        assert_eq!(
            ev.attendees,
            vec!["a@x.com".to_string(), "b@y.com".to_string()]
        );
        assert!(ev.location.is_none());
        assert_eq!(ev.description.as_deref(), Some("note"));
        assert_eq!(
            ev.recurrence,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20261231T235959Z"]
        );
    }

    #[test]
    fn update_sets_fields_and_clears_error() {
        let mut f = FormState::default();
        f.error = Some("stale".into());
        f.update(FormMsg::Title("Hi".into()));
        assert_eq!(f.title, "Hi");
        assert!(f.error.is_none());
        f.update(FormMsg::AllDay(true));
        assert!(f.all_day);
        f.update(FormMsg::ToggleWeekday(2, true));
        assert!(f.weekdays.days[2]);
        f.update(FormMsg::ToggleWeekday(99, true)); // out of range -> ignored
        f.update(FormMsg::Calendar(CalendarChoice {
            id: "w".into(),
            name: "Work".into(),
        }));
        assert_eq!(f.calendar_id.as_deref(), Some("w"));
        f.update(FormMsg::Recurrence(RecurrenceKind::Monthly));
        assert_eq!(f.recurrence, RecurrenceKind::Monthly);
        // remaining string setters
        for m in [
            FormMsg::StartDate("d".into()),
            FormMsg::StartTime("t".into()),
            FormMsg::EndDate("d".into()),
            FormMsg::EndTime("t".into()),
            FormMsg::Location("l".into()),
            FormMsg::Description("desc".into()),
            FormMsg::Guests("g".into()),
            FormMsg::UntilEnabled(true),
            FormMsg::UntilDate("2026-01-01".into()),
        ] {
            f.update(m);
        }
        assert!(f.until_enabled);
    }

    #[test]
    fn reset_preserves_calendar() {
        let mut f = valid_form();
        f.calendar_id = Some("w".into());
        f.reset();
        assert_eq!(f.calendar_id.as_deref(), Some("w"));
        assert!(f.title.is_empty());
    }

    #[test]
    fn until_respects_toggle_and_validity() {
        let mut f = FormState::default();
        f.until_enabled = false;
        assert!(f.until().is_none());
        f.until_enabled = true;
        f.until_date = "2026-12-31".into();
        assert!(f.until().is_some());
        f.until_date = "garbage".into();
        assert!(f.until().is_none());
    }

    #[test]
    fn view_builds_in_all_modes() {
        // Timed, no recurrence.
        let _ = view(&valid_form(), &cals());

        // All-day, weekly recurrence with until, an error, and submitting.
        let mut f = valid_form();
        f.all_day = true;
        f.recurrence = RecurrenceKind::Weekly;
        f.until_enabled = true;
        f.error = Some("boom".into());
        f.submitting = true;
        let _ = view(&f, &cals());
    }

    fn details() -> EventDetails {
        let start = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        EventDetails {
            calendar_id: "w".into(),
            event_id: "master".into(),
            title: "Retro".into(),
            location: Some("Room 2".into()),
            description: Some("notes".into()),
            all_day: false,
            start,
            end: start + chrono::Duration::hours(1),
            attendees: vec!["a@x.com".into(), "b@y.com".into()],
            recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
        }
    }

    #[test]
    fn prefill_populates_fields_and_sets_edit_target() {
        let f = FormState::prefill(&details());
        let target = f.editing.as_ref().expect("editing target set");
        assert_eq!(target.calendar_id, "w");
        assert_eq!(target.event_id, "master");
        assert_eq!(f.title, "Retro");
        assert_eq!(f.location, "Room 2");
        assert_eq!(f.description, "notes");
        assert_eq!(f.guests, "a@x.com, b@y.com");
        assert_eq!(f.calendar_id.as_deref(), Some("w"));
        assert_eq!(f.start_date, "2026-07-02");
        assert_eq!(f.start_time, "09:00");
        // A prefilled form rebuilds into a valid NewEvent for the update path.
        assert!(f.build(&cals()).is_ok());
    }

    #[test]
    fn view_builds_in_edit_mode() {
        // Edit mode hides the recurrence controls and relabels the buttons.
        let f = FormState::prefill(&details());
        let _ = view(&f, &cals());
    }
}
