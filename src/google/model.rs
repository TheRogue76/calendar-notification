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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn occ(start: DateTime<Local>, minutes: Vec<i64>) -> Occurrence {
        Occurrence {
            event_id: "evt".into(),
            calendar_id: "cal".into(),
            title: "T".into(),
            location: None,
            start,
            end: start,
            all_day: false,
            reminders: minutes
                .into_iter()
                .map(|m| ReminderRule { minutes: m })
                .collect(),
        }
    }

    #[test]
    fn occurrence_key_combines_id_and_start() {
        let start = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let o = occ(start, vec![]);
        let key = o.occurrence_key();
        assert!(key.starts_with("evt::"));
        assert!(key.contains(&start.to_rfc3339()));
    }

    #[test]
    fn reminder_fire_times_subtract_minutes() {
        let start = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let o = occ(start, vec![10, 30]);
        let fires: Vec<_> = o.reminder_fire_times().collect();
        assert_eq!(fires.len(), 2);
        let start_utc = start.with_timezone(&Utc);
        assert_eq!(fires[0], (start_utc - chrono::Duration::minutes(10), 10));
        assert_eq!(fires[1], (start_utc - chrono::Duration::minutes(30), 30));
    }

    #[test]
    fn reminder_fire_times_empty_when_no_rules() {
        let start = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        assert_eq!(occ(start, vec![]).reminder_fire_times().count(), 0);
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
