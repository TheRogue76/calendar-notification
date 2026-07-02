//! Desktop notification wrapper over `notify-rust` (freedesktop
//! `org.freedesktop.Notifications` via pure-Rust zbus).

use anyhow::Result;

use crate::google::model::Occurrence;

/// Show a reminder notification for an upcoming occurrence.
///
/// `minutes_before` is the lead time of the reminder rule that fired, used to
/// phrase the summary ("in 10 minutes").
///
/// Uses the async (`show_async`) API: the synchronous `show()` internally spins
/// up a blocking zbus runtime, which panics ("Cannot start a runtime from
/// within a runtime") when called from inside the engine's tokio loop.
pub async fn show_reminder(occ: &Occurrence, minutes_before: i64) -> Result<()> {
    let when = lead_phrase(minutes_before);
    let summary = format!("{} — {when}", occ.title);

    let mut body = if occ.all_day {
        "All day".to_string()
    } else {
        occ.start.format("%H:%M").to_string()
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
        .show_async()
        .await?;

    Ok(())
}

fn lead_phrase(minutes: i64) -> String {
    // Ranges are ordered smallest-first so each is only reached once the
    // shorter units are exhausted — a whole day reads as "1 day", not "24 hours".
    match minutes {
        0 => "now".to_string(),
        1 => "in 1 minute".to_string(),
        m if m < 60 => format!("in {m} minutes"),
        60 => "in 1 hour".to_string(),
        m if m < 1440 => {
            let (hours, mins) = (m / 60, m % 60);
            if mins == 0 {
                format!("in {hours} hours")
            } else {
                format!("in {hours}h {mins}m")
            }
        }
        1440 => "in 1 day".to_string(),
        m => {
            let (days, hours) = (m / 1440, (m % 1440) / 60);
            if hours == 0 {
                format!("in {days} days")
            } else {
                format!("in {days}d {hours}h")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lead_phrase;

    #[test]
    fn lead_phrase_covers_all_ranges() {
        assert_eq!(lead_phrase(0), "now");
        assert_eq!(lead_phrase(1), "in 1 minute");
        assert_eq!(lead_phrase(10), "in 10 minutes");
        assert_eq!(lead_phrase(59), "in 59 minutes");
        assert_eq!(lead_phrase(60), "in 1 hour");
        assert_eq!(lead_phrase(120), "in 2 hours");
        assert_eq!(lead_phrase(90), "in 1h 30m");
        assert_eq!(lead_phrase(1439), "in 23h 59m"); // just under a day
        assert_eq!(lead_phrase(1440), "in 1 day");
        assert_eq!(lead_phrase(2880), "in 2 days");
        assert_eq!(lead_phrase(1500), "in 1d 1h"); // 1 day + 1 hour
        assert_eq!(lead_phrase(4320), "in 3 days");
    }
}
