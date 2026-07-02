//! Domain types shared across sync, scheduler, and UI. These are deliberately
//! decoupled from the generated `google-calendar3` structs so the rest of the
//! app never touches the raw API shapes.

use chrono::{DateTime, Local, Utc};

/// A calendar the user has access to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub summary: String,
    /// Google-provided background color (hex), used unless the user overrides it.
    pub color: String,
    pub primary: bool,
}

/// One reminder rule: fire `minutes` before the event via `method`
/// (we only act on popup/display reminders locally).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReminderRule {
    pub minutes: i64,
}

/// A single concrete occurrence of an event (recurring events are expanded into
/// one `Occurrence` per instance within the working window).
#[derive(Debug, Clone)]
pub struct Occurrence {
    /// Google event id (shared across occurrences of a recurring series).
    pub event_id: String,
    pub calendar_id: String,
    pub title: String,
    pub location: Option<String>,
    /// Start instant. For all-day events this is local midnight of the day.
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    /// Effective reminder rules (event overrides, or calendar defaults).
    pub reminders: Vec<ReminderRule>,
}

impl Occurrence {
    /// Stable key identifying this specific occurrence (event + start instant).
    pub fn occurrence_key(&self) -> String {
        format!("{}::{}", self.event_id, self.start.to_rfc3339())
    }

    /// Fire instants for each reminder rule (start − minutes).
    pub fn reminder_fire_times(&self) -> impl Iterator<Item = (DateTime<Utc>, i64)> + '_ {
        let start_utc = self.start.with_timezone(&Utc);
        self.reminders
            .iter()
            .map(move |r| (start_utc - chrono::Duration::minutes(r.minutes), r.minutes))
    }
}

/// A new event to create, assembled by the add-event form.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub calendar_id: String,
    pub title: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub all_day: bool,
    /// For timed events. For all-day events these carry the date at midnight.
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub attendees: Vec<String>,
    /// RRULE lines (without the leading "RRULE:"), empty for non-recurring.
    pub recurrence: Vec<String>,
}
