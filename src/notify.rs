//! Desktop notification wrapper over `notify-rust` (freedesktop
//! `org.freedesktop.Notifications` via pure-Rust zbus).

use anyhow::Result;
use chrono::Local;

use crate::google::model::Occurrence;

/// Show a reminder notification for an upcoming occurrence.
///
/// `minutes_before` is the lead time of the reminder rule that fired, used to
/// phrase the summary ("in 10 minutes").
pub fn show_reminder(occ: &Occurrence, minutes_before: i64) -> Result<()> {
    let when = lead_phrase(minutes_before);
    let summary = format!("{} — {when}", occ.title);

    let mut body = if occ.all_day {
        "All day".to_string()
    } else {
        format!("{}", occ.start.with_timezone(&Local).format("%H:%M"))
    };
    if let Some(loc) = &occ.location {
        if !loc.is_empty() {
            body.push_str(&format!("\n📍 {loc}"));
        }
    }

    notify_rust::Notification::new()
        .summary(&summary)
        .body(&body)
        .icon("x-office-calendar")
        .appname("Calendar")
        .timeout(notify_rust::Timeout::Milliseconds(10_000))
        .show()?;

    Ok(())
}

fn lead_phrase(minutes: i64) -> String {
    match minutes {
        0 => "now".to_string(),
        1 => "in 1 minute".to_string(),
        m if m < 60 => format!("in {m} minutes"),
        60 => "in 1 hour".to_string(),
        m if m % 60 == 0 => format!("in {} hours", m / 60),
        m if m < 1440 => format!("in {}h {}m", m / 60, m % 60),
        m if m % 1440 == 0 => format!("in {} days", m / 1440),
        m => format!("in {} days", m / 1440),
    }
}
