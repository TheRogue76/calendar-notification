//! Thin wrapper over `google-calendar3`'s `CalendarHub`: list calendars, list
//! (server-expanded) event occurrences, and insert new events. All raw API
//! shapes are converted to the domain types in [`crate::google::model`] here so
//! nothing else touches the generated structs.

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use google_calendar3::api::{Event, EventAttendee, EventDateTime, EventReminder, EventReminders};
use google_calendar3::CalendarHub;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use super::auth::Auth;
use super::model::{Calendar, NewEvent, Occurrence, ReminderRule};

type Hub = CalendarHub<HttpsConnector<HttpConnector>>;

pub struct GoogleClient {
    hub: Hub,
}

impl GoogleClient {
    /// Build the hub from an authenticator using a rustls hyper client.
    pub fn new(auth: Auth) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("no native root certificates found")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Self {
            hub: CalendarHub::new(client, auth),
        }
    }

    /// All calendars the user can see.
    pub async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let (_resp, list) = self
            .hub
            .calendar_list()
            .list()
            .doit()
            .await
            .context("listing calendars")?;

        let mut out = Vec::new();
        for entry in list.items.unwrap_or_default() {
            let Some(id) = entry.id else { continue };
            // Only calendars we can actually read events from. `freeBusyReader`
            // and `none` calendars appear in the list but return 404 on
            // events.list, so drop them to avoid noise.
            let access = entry.access_role.as_deref().unwrap_or("reader");
            if !matches!(access, "reader" | "writer" | "owner") {
                continue;
            }
            out.push(Calendar {
                summary: entry.summary.unwrap_or_else(|| id.clone()),
                color: entry.background_color.unwrap_or_default(),
                primary: entry.primary.unwrap_or(false),
                id,
            });
        }
        Ok(out)
    }

    /// List occurrences in `[time_min, time_max)` for one calendar. Recurring
    /// events are expanded server-side (`single_events(true)`), so each item is
    /// already a concrete instance carrying its own reminder settings.
    pub async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<Occurrence>> {
        let (_resp, events) = self
            .hub
            .events()
            .list(&encode_calendar_id(calendar_id))
            .time_min(time_min)
            .time_max(time_max)
            .single_events(true)
            .order_by("startTime")
            .doit()
            .await
            .with_context(|| format!("listing events for {calendar_id}"))?;

        let calendar_defaults = reminder_rules(events.default_reminders.as_deref());

        let mut out = Vec::new();
        for ev in events.items.unwrap_or_default() {
            if let Some(occ) = to_occurrence(calendar_id, ev, &calendar_defaults) {
                out.push(occ);
            }
        }
        Ok(out)
    }

    /// Create a new event; returns the created event id.
    pub async fn insert_event(&self, new: &NewEvent) -> Result<String> {
        let event = build_event(new);
        let (_resp, created) = self
            .hub
            .events()
            .insert(event, &encode_calendar_id(&new.calendar_id))
            .doit()
            .await
            .context("inserting event")?;
        Ok(created.id.unwrap_or_default())
    }
}

/// Percent-encode the characters that would otherwise break the request URL.
///
/// The generated `google-calendar3` client substitutes calendar ids into the
/// URL path *without* encoding (`uri_replacement(..., url_encode = false)`), so
/// a `#` — present in every Google holiday calendar id
/// (`en.swedish#holiday@group.v.calendar.google.com`) — is parsed by `Url` as a
/// fragment and silently truncates the path to `/calendars/en.swedish`, which
/// the server routes to `Calendars.Get`. Encoding `#`/`?`/`%` ourselves keeps
/// the id intact; the server decodes it back.
fn encode_calendar_id(id: &str) -> String {
    id.replace('%', "%25").replace('#', "%23").replace('?', "%3F")
}

/// Convert Google reminder overrides to our popup-only rules.
fn reminder_rules(reminders: Option<&[EventReminder]>) -> Vec<ReminderRule> {
    reminders
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            // We only surface popup/display reminders locally; Google delivers
            // email reminders itself.
            r.method.as_deref().map(|m| m == "popup").unwrap_or(true)
        })
        .filter_map(|r| r.minutes.map(|m| ReminderRule { minutes: m as i64 }))
        .collect()
}

fn to_occurrence(
    calendar_id: &str,
    ev: Event,
    calendar_defaults: &[ReminderRule],
) -> Option<Occurrence> {
    let event_id = ev.id.clone()?;
    let (start_dt, all_day) = parse_start(ev.start.as_ref())?;
    let end_dt = parse_end(ev.end.as_ref()).unwrap_or(start_dt);

    // Effective reminders: event overrides win; otherwise fall back to the
    // calendar's defaults (matching Google's useDefault semantics).
    let reminders = match &ev.reminders {
        Some(EventReminders {
            use_default: Some(false),
            overrides: Some(ov),
        }) => reminder_rules(Some(ov.as_slice())),
        _ => calendar_defaults.to_vec(),
    };

    Some(Occurrence {
        event_id,
        calendar_id: calendar_id.to_string(),
        title: ev.summary.unwrap_or_else(|| "(no title)".to_string()),
        location: ev.location,
        start: start_dt,
        end: end_dt,
        all_day,
        reminders,
    })
}

/// Returns (start instant in local tz, is_all_day).
fn parse_start(edt: Option<&EventDateTime>) -> Option<(DateTime<Local>, bool)> {
    let edt = edt?;
    if let Some(dt) = edt.date_time {
        Some((dt.with_timezone(&Local), false))
    } else if let Some(date) = edt.date {
        let midnight = date.and_hms_opt(0, 0, 0)?;
        let local = midnight.and_local_timezone(Local).single()?;
        Some((local, true))
    } else {
        None
    }
}

fn parse_end(edt: Option<&EventDateTime>) -> Option<DateTime<Local>> {
    let edt = edt?;
    if let Some(dt) = edt.date_time {
        Some(dt.with_timezone(&Local))
    } else if let Some(date) = edt.date {
        date.and_hms_opt(0, 0, 0)?
            .and_local_timezone(Local)
            .single()
    } else {
        None
    }
}

fn build_event(new: &NewEvent) -> Event {
    let (start, end) = if new.all_day {
        let start_date = new.start.date_naive();
        // Google's all-day end date is exclusive; ensure it's after start.
        let mut end_date = new.end.date_naive();
        if end_date <= start_date {
            end_date = start_date.succ_opt().unwrap_or(start_date);
        }
        (
            EventDateTime {
                date: Some(start_date),
                date_time: None,
                time_zone: None,
            },
            EventDateTime {
                date: Some(end_date),
                date_time: None,
                time_zone: None,
            },
        )
    } else {
        (
            EventDateTime {
                date: None,
                date_time: Some(new.start.with_timezone(&Utc)),
                time_zone: None,
            },
            EventDateTime {
                date: None,
                date_time: Some(new.end.with_timezone(&Utc)),
                time_zone: None,
            },
        )
    };

    let attendees = if new.attendees.is_empty() {
        None
    } else {
        Some(
            new.attendees
                .iter()
                .map(|email| EventAttendee {
                    email: Some(email.clone()),
                    ..Default::default()
                })
                .collect(),
        )
    };

    let recurrence = if new.recurrence.is_empty() {
        None
    } else {
        Some(new.recurrence.clone())
    };

    Event {
        summary: Some(new.title.clone()),
        location: new.location.clone(),
        description: new.description.clone(),
        start: Some(start),
        end: Some(end),
        attendees,
        recurrence,
        // Leave reminders on useDefault so the calendar's defaults apply.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::encode_calendar_id;

    #[test]
    fn encodes_hash_in_holiday_calendar_id() {
        assert_eq!(
            encode_calendar_id("en.swedish#holiday@group.v.calendar.google.com"),
            "en.swedish%23holiday@group.v.calendar.google.com"
        );
    }

    #[test]
    fn leaves_normal_ids_untouched() {
        assert_eq!(encode_calendar_id("primary"), "primary");
        assert_eq!(
            encode_calendar_id("abc@group.calendar.google.com"),
            "abc@group.calendar.google.com"
        );
    }
}
