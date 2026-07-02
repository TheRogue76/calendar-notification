//! Recurrence presets and their serialization to iCalendar RRULE strings.
//!
//! We expose a small, friendly set of presets in the UI (none / daily / weekly
//! with weekday selection / monthly / yearly), each with an optional end date,
//! and serialize the chosen preset to the RRULE line Google Calendar expects.

use chrono::{Datelike, NaiveDate};

/// Weekday flags for the weekly preset (Mon..Sun).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Weekdays {
    pub days: [bool; 7], // index 0 = Monday .. 6 = Sunday
}

impl Weekdays {
    const CODES: [&'static str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

    /// Preselect the weekday of `date` so the weekly preset has a sensible default.
    pub fn from_date(date: NaiveDate) -> Self {
        let mut days = [false; 7];
        // chrono: Mon=0 .. Sun=6 via num_days_from_monday
        days[date.weekday().num_days_from_monday() as usize] = true;
        Self { days }
    }

    fn byday(&self) -> Option<String> {
        let codes: Vec<&str> = Self::CODES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.days[*i])
            .map(|(_, c)| *c)
            .collect();
        if codes.is_empty() {
            None
        } else {
            Some(codes.join(","))
        }
    }
}

/// A recurrence preset chosen in the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recurrence {
    None,
    Daily,
    Weekly(Weekdays),
    Monthly,
    Yearly,
}

impl Recurrence {
    /// Menu label for the pick_list.
    pub const ALL: [RecurrenceKind; 5] = [
        RecurrenceKind::None,
        RecurrenceKind::Daily,
        RecurrenceKind::Weekly,
        RecurrenceKind::Monthly,
        RecurrenceKind::Yearly,
    ];

    /// Serialize to RRULE lines (without the "RRULE:" prefix — the API takes the
    /// full line, so we include it). Returns empty for [`Recurrence::None`].
    ///
    /// `until` is an optional inclusive end date.
    pub fn to_rrule(&self, until: Option<NaiveDate>) -> Vec<String> {
        let freq = match self {
            Recurrence::None => return Vec::new(),
            Recurrence::Daily => "DAILY",
            Recurrence::Weekly(_) => "WEEKLY",
            Recurrence::Monthly => "MONTHLY",
            Recurrence::Yearly => "YEARLY",
        };

        let mut rule = format!("RRULE:FREQ={freq}");

        if let Recurrence::Weekly(wd) = self {
            if let Some(byday) = wd.byday() {
                rule.push_str(&format!(";BYDAY={byday}"));
            }
        }

        if let Some(date) = until {
            // UNTIL in RRULE is a date-time in UTC (Z). End of that day.
            rule.push_str(&format!(";UNTIL={}T235959Z", date.format("%Y%m%d")));
        }

        vec![rule]
    }
}

/// A `Copy` tag used as the `pick_list` selection type (the payload-carrying
/// [`Recurrence::Weekly`] variant isn't `Copy`, so the UI selects a kind and
/// keeps weekday state separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceKind {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl std::fmt::Display for RecurrenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RecurrenceKind::None => "Does not repeat",
            RecurrenceKind::Daily => "Daily",
            RecurrenceKind::Weekly => "Weekly",
            RecurrenceKind::Monthly => "Monthly",
            RecurrenceKind::Yearly => "Yearly",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_has_no_rule() {
        assert!(Recurrence::None.to_rrule(None).is_empty());
    }

    #[test]
    fn daily_rule() {
        assert_eq!(Recurrence::Daily.to_rrule(None), vec!["RRULE:FREQ=DAILY"]);
    }

    #[test]
    fn weekly_with_days_and_until() {
        let wd = Weekdays {
            days: [true, false, true, false, false, false, false],
        };
        let until = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(
            Recurrence::Weekly(wd).to_rrule(Some(until)),
            vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261231T235959Z"]
        );
    }
}
