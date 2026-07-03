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
use super::model::{Calendar, EventDetails, NewEvent, Occurrence, ReminderRule};

type Hub = CalendarHub<HttpsConnector<HttpConnector>>;

/// `fields` masks for Google's partial-response support: fetch only what the
/// domain conversions actually read (the JSON — camelCase — field names).
/// Keep in sync with [`to_occurrence`] / `list_calendars`; a field named here
/// but missing server-side is a 400, so test any change against the live API.
const EVENT_LIST_FIELDS: &str =
    "items(id,recurringEventId,summary,location,start,end,reminders),defaultReminders";
const CALENDAR_LIST_FIELDS: &str = "items(id,summary,backgroundColor,primary,deleted,accessRole)";

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
}

impl crate::engine::CalendarSource for GoogleClient {
    /// All calendars the user can see.
    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let (_resp, list) = self
            .hub
            .calendar_list()
            .list()
            // Partial response: only the fields the conversion below reads.
            .param("fields", CALENDAR_LIST_FIELDS)
            .doit()
            .await
            .context("listing calendars")?;

        let mut out = Vec::new();
        for entry in list.items.unwrap_or_default() {
            let Some(id) = entry.id else { continue };
            // Skip stale/removed subscriptions that Google keeps in the list.
            if entry.deleted == Some(true) {
                tracing::debug!("skipping deleted calendar {id}");
                continue;
            }
            // Only calendars we can actually read events from. `freeBusyReader`
            // and `none` calendars appear in the list but return 404 on
            // events.list, so drop them to avoid noise.
            let access = entry.access_role.as_deref().unwrap_or("reader");
            if !matches!(access, "reader" | "writer" | "owner") {
                tracing::debug!("skipping calendar {id} (access_role={access})");
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
    async fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<Occurrence>> {
        let result = self
            .hub
            .events()
            .list(&encode_calendar_id(calendar_id))
            .time_min(time_min)
            .time_max(time_max)
            .single_events(true)
            .order_by("startTime")
            // Partial response: full event resources carry description,
            // attendee lists, conference data, etc. on every poll, while
            // `to_occurrence` only reads these fields.
            .param("fields", EVENT_LIST_FIELDS)
            .doit()
            .await;

        let (_resp, events) = match result {
            Ok(v) => v,
            Err(e) => {
                // A 404 means the calendar is in the list but isn't queryable
                // (stale/removed subscription). Skip it quietly rather than
                // spamming a warning every sync; propagate everything else.
                let msg = e.to_string();
                if msg.contains("notFound") || msg.contains("\"code\":404") {
                    tracing::debug!("skipping unreadable calendar {calendar_id}: {msg}");
                    return Ok(Vec::new());
                }
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("listing events for {calendar_id}"));
            }
        };

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
    async fn insert_event(&self, new: &NewEvent) -> Result<String> {
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

    /// Fetch a single event (the series master for a recurring event) with the
    /// fields the detail pane and edit form need.
    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<EventDetails> {
        let (_resp, ev) = self
            .hub
            .events()
            .get(&encode_calendar_id(calendar_id), event_id)
            .doit()
            .await
            .with_context(|| format!("getting event {event_id}"))?;
        to_event_details(calendar_id, event_id, ev)
    }

    /// Patch an existing event (whole series for a recurring event); returns the
    /// event id.
    async fn update_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        ev: &NewEvent,
    ) -> Result<String> {
        let event = build_patch(ev);
        let (_resp, updated) = self
            .hub
            .events()
            .patch(event, &encode_calendar_id(calendar_id), event_id)
            .doit()
            .await
            .context("updating event")?;
        Ok(updated.id.unwrap_or_default())
    }

    /// Delete an event. `event_id` may be a single expanded instance (cancels
    /// that occurrence) or the series master (removes the whole series). Unlike
    /// insert/patch/get, `delete().doit()` yields only a `Response` (empty body),
    /// so there's nothing to destructure.
    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        self.hub
            .events()
            .delete(&encode_calendar_id(calendar_id), event_id)
            .doit()
            .await
            .with_context(|| format!("deleting event {event_id}"))?;
        Ok(())
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
    id.replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
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
    let recurring_event_id = ev.recurring_event_id.clone();
    let (start_dt, all_day) = parse_start(ev.start.as_ref())?;
    let end_dt = parse_end(ev.end.as_ref()).unwrap_or(start_dt);

    // Effective reminders, matching Google's useDefault semantics:
    //   useDefault=false + overrides -> those overrides win;
    //   useDefault=false + no overrides -> the event has *no* reminders;
    //   anything else (useDefault=true / unset) -> the calendar's defaults.
    let reminders = match &ev.reminders {
        Some(EventReminders {
            use_default: Some(false),
            overrides: Some(ov),
        }) => reminder_rules(Some(ov.as_slice())),
        Some(EventReminders {
            use_default: Some(false),
            overrides: None,
        }) => Vec::new(),
        _ => calendar_defaults.to_vec(),
    };

    Some(Occurrence {
        event_id,
        recurring_event_id,
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

/// Start/end `EventDateTime`s for an event: all-day events carry dates (with an
/// exclusive end, per Google), timed events carry UTC instants.
fn event_times(new: &NewEvent) -> (EventDateTime, EventDateTime) {
    if new.all_day {
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
    }
}

/// Attendee list for the API. Callers decide how to represent "no guests":
/// insert omits the field (`None`), patch must send `Some(vec![])` so an
/// emptied guest list actually clears the attendees.
fn event_attendees(new: &NewEvent) -> Vec<EventAttendee> {
    new.attendees
        .iter()
        .map(|email| EventAttendee {
            email: Some(email.clone()),
            ..Default::default()
        })
        .collect()
}

fn build_event(new: &NewEvent) -> Event {
    let (start, end) = event_times(new);

    let recurrence = if new.recurrence.is_empty() {
        None
    } else {
        Some(new.recurrence.clone())
    };

    let attendees = event_attendees(new);
    Event {
        summary: Some(new.title.clone()),
        location: new.location.clone(),
        description: new.description.clone(),
        start: Some(start),
        end: Some(end),
        attendees: (!attendees.is_empty()).then_some(attendees),
        recurrence,
        // Leave reminders on useDefault so the calendar's defaults apply.
        ..Default::default()
    }
}

/// Build the `Event` for a PATCH update. Differences from [`build_event`]:
/// blank `location`/`description` are sent as `Some("")` and an empty guest
/// list as `Some(vec![])` so they can be cleared (PATCH ignores fields left as
/// `None`), while `recurrence`/`reminders` are omitted entirely so the existing
/// series RRULE and reminder overrides are preserved (the edit form doesn't
/// expose them).
fn build_patch(new: &NewEvent) -> Event {
    let (start, end) = event_times(new);
    Event {
        summary: Some(new.title.clone()),
        location: Some(new.location.clone().unwrap_or_default()),
        description: Some(new.description.clone().unwrap_or_default()),
        start: Some(start),
        end: Some(end),
        attendees: Some(event_attendees(new)),
        ..Default::default()
    }
}

/// Convert a fully-fetched `Event` into our [`EventDetails`] domain type.
fn to_event_details(calendar_id: &str, event_id: &str, ev: Event) -> Result<EventDetails> {
    let (start, all_day) = parse_start(ev.start.as_ref()).context("event has no start time")?;
    let end = parse_end(ev.end.as_ref()).unwrap_or(start);
    let attendees = ev
        .attendees
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.email)
        .collect();
    Ok(EventDetails {
        calendar_id: calendar_id.to_string(),
        event_id: event_id.to_string(),
        title: ev.summary.unwrap_or_else(|| "(no title)".to_string()),
        location: ev.location,
        description: ev.description,
        all_day,
        start,
        end,
        attendees,
        recurrence: ev.recurrence.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    // -- encode_calendar_id --------------------------------------------------

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

    #[test]
    fn encodes_percent_and_question() {
        assert_eq!(encode_calendar_id("a%b?c"), "a%25b%3Fc");
    }

    // -- reminder_rules ------------------------------------------------------

    fn reminder(method: Option<&str>, minutes: Option<i32>) -> EventReminder {
        EventReminder {
            method: method.map(|s| s.to_string()),
            minutes,
        }
    }

    #[test]
    fn reminder_rules_keeps_popup_and_untyped_skips_email() {
        let rs = vec![
            reminder(Some("popup"), Some(10)),
            reminder(Some("email"), Some(60)),
            reminder(None, Some(5)),       // untyped treated as popup
            reminder(Some("popup"), None), // no minutes -> dropped
        ];
        let out = reminder_rules(Some(&rs));
        assert_eq!(
            out,
            vec![ReminderRule { minutes: 10 }, ReminderRule { minutes: 5 }]
        );
    }

    #[test]
    fn reminder_rules_none_is_empty() {
        assert!(reminder_rules(None).is_empty());
    }

    // -- parse_start / parse_end --------------------------------------------

    #[test]
    fn parse_start_timed_is_not_all_day() {
        let dt = Utc.with_ymd_and_hms(2026, 7, 2, 8, 0, 0).unwrap();
        let edt = EventDateTime {
            date_time: Some(dt),
            date: None,
            time_zone: None,
        };
        let (start, all_day) = parse_start(Some(&edt)).unwrap();
        assert!(!all_day);
        assert_eq!(start, dt.with_timezone(&Local));
    }

    #[test]
    fn parse_start_date_is_all_day() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let edt = EventDateTime {
            date_time: None,
            date: Some(d),
            time_zone: None,
        };
        let (_start, all_day) = parse_start(Some(&edt)).unwrap();
        assert!(all_day);
    }

    #[test]
    fn parse_start_none_and_empty() {
        assert!(parse_start(None).is_none());
        let empty = EventDateTime {
            date_time: None,
            date: None,
            time_zone: None,
        };
        assert!(parse_start(Some(&empty)).is_none());
        assert!(parse_end(Some(&empty)).is_none());
    }

    // -- to_occurrence -------------------------------------------------------

    fn timed_event(id: Option<&str>) -> Event {
        let dt = Utc.with_ymd_and_hms(2026, 7, 2, 8, 0, 0).unwrap();
        Event {
            id: id.map(|s| s.to_string()),
            summary: Some("Meeting".into()),
            location: Some("Room".into()),
            start: Some(EventDateTime {
                date_time: Some(dt),
                date: None,
                time_zone: None,
            }),
            end: Some(EventDateTime {
                date_time: Some(dt + chrono::Duration::hours(1)),
                date: None,
                time_zone: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn to_occurrence_missing_id_is_none() {
        assert!(to_occurrence("cal", timed_event(None), &[]).is_none());
    }

    #[test]
    fn to_occurrence_uses_calendar_defaults_when_use_default() {
        let ev = timed_event(Some("e1"));
        let defaults = vec![ReminderRule { minutes: 15 }];
        let occ = to_occurrence("cal", ev, &defaults).unwrap();
        assert_eq!(occ.event_id, "e1");
        assert_eq!(occ.calendar_id, "cal");
        assert!(!occ.all_day);
        assert_eq!(occ.reminders, defaults);
    }

    #[test]
    fn to_occurrence_overrides_win_over_defaults() {
        let mut ev = timed_event(Some("e1"));
        ev.reminders = Some(EventReminders {
            use_default: Some(false),
            overrides: Some(vec![reminder(Some("popup"), Some(2))]),
        });
        let occ = to_occurrence("cal", ev, &[ReminderRule { minutes: 15 }]).unwrap();
        assert_eq!(occ.reminders, vec![ReminderRule { minutes: 2 }]);
    }

    #[test]
    fn to_occurrence_use_default_false_without_overrides_has_no_reminders() {
        let mut ev = timed_event(Some("e1"));
        // useDefault=false with no overrides means the event opted out of
        // reminders entirely — calendar defaults must NOT be applied.
        ev.reminders = Some(EventReminders {
            use_default: Some(false),
            overrides: None,
        });
        let occ = to_occurrence("cal", ev, &[ReminderRule { minutes: 15 }]).unwrap();
        assert!(occ.reminders.is_empty());
    }

    #[test]
    fn to_occurrence_missing_summary_gets_placeholder() {
        let mut ev = timed_event(Some("e1"));
        ev.summary = None;
        let occ = to_occurrence("cal", ev, &[]).unwrap();
        assert_eq!(occ.title, "(no title)");
    }

    // -- build_event ---------------------------------------------------------

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn base_new() -> NewEvent {
        NewEvent {
            calendar_id: "primary".into(),
            title: "Title".into(),
            location: Some("Loc".into()),
            description: Some("Desc".into()),
            all_day: false,
            start: local(2026, 7, 2, 9, 0),
            end: local(2026, 7, 2, 10, 0),
            attendees: vec![],
            recurrence: vec![],
        }
    }

    #[test]
    fn build_event_timed_uses_utc_datetime() {
        let ev = build_event(&base_new());
        let start = ev.start.unwrap();
        assert!(start.date.is_none());
        assert_eq!(
            start.date_time.unwrap(),
            local(2026, 7, 2, 9, 0).with_timezone(&Utc)
        );
        assert_eq!(ev.summary.unwrap(), "Title");
        assert_eq!(ev.location.unwrap(), "Loc");
        assert!(ev.attendees.is_none());
        assert!(ev.recurrence.is_none());
    }

    #[test]
    fn build_event_all_day_end_is_exclusive() {
        let mut n = base_new();
        n.all_day = true;
        n.start = local(2026, 7, 2, 0, 0);
        n.end = local(2026, 7, 2, 0, 0); // same day
        let ev = build_event(&n);
        assert_eq!(
            ev.start.unwrap().date.unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()
        );
        // exclusive end bumped to next day
        assert_eq!(
            ev.end.unwrap().date.unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 3).unwrap()
        );
    }

    #[test]
    fn build_event_multiday_all_day_preserves_end() {
        let mut n = base_new();
        n.all_day = true;
        n.start = local(2026, 7, 2, 0, 0);
        n.end = local(2026, 7, 5, 0, 0);
        let ev = build_event(&n);
        assert_eq!(
            ev.end.unwrap().date.unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 5).unwrap()
        );
    }

    #[test]
    fn build_event_maps_attendees_and_recurrence() {
        let mut n = base_new();
        n.attendees = vec!["a@x.com".into(), "b@y.com".into()];
        n.recurrence = vec!["RRULE:FREQ=DAILY".into()];
        let ev = build_event(&n);
        let att = ev.attendees.unwrap();
        assert_eq!(att.len(), 2);
        assert_eq!(att[0].email.as_deref(), Some("a@x.com"));
        assert_eq!(ev.recurrence.unwrap(), vec!["RRULE:FREQ=DAILY".to_string()]);
    }

    // -- build_patch ---------------------------------------------------------

    #[test]
    fn build_patch_sends_blank_fields_as_empty_to_clear_them() {
        let mut n = base_new();
        n.location = None;
        n.description = None;
        let ev = build_patch(&n);
        // Some("") lets PATCH clear the field; None would leave it unchanged.
        assert_eq!(ev.location.as_deref(), Some(""));
        assert_eq!(ev.description.as_deref(), Some(""));
    }

    #[test]
    fn build_patch_sends_empty_attendees_to_clear_guests() {
        let mut n = base_new();
        n.attendees = vec![];
        let ev = build_patch(&n);
        // Some(vec![]) lets PATCH remove all guests; None would leave them.
        let att = ev.attendees.expect("attendees must be present in a patch");
        assert!(att.is_empty(), "an emptied guest list clears attendees");

        // With guests present they are mapped as usual.
        n.attendees = vec!["a@x.com".into()];
        let ev = build_patch(&n);
        assert_eq!(ev.attendees.unwrap()[0].email.as_deref(), Some("a@x.com"));
    }

    #[test]
    fn build_patch_omits_recurrence_and_reminders_to_preserve_them() {
        let mut n = base_new();
        n.recurrence = vec!["RRULE:FREQ=DAILY".into()];
        let ev = build_patch(&n);
        // Left as None so the existing series RRULE / reminder overrides survive.
        assert!(ev.recurrence.is_none());
        assert!(ev.reminders.is_none());
        assert_eq!(ev.summary.as_deref(), Some("Title"));
    }

    // -- to_event_details ----------------------------------------------------

    #[test]
    fn to_event_details_maps_timed_event_with_guests_and_recurrence() {
        let mut ev = timed_event(Some("master"));
        ev.description = Some("notes".into());
        ev.recurrence = Some(vec!["RRULE:FREQ=WEEKLY".into()]);
        ev.attendees = Some(vec![
            EventAttendee {
                email: Some("a@x.com".into()),
                ..Default::default()
            },
            EventAttendee {
                email: None, // dropped
                ..Default::default()
            },
        ]);
        let d = to_event_details("cal", "master", ev).unwrap();
        assert_eq!(d.calendar_id, "cal");
        assert_eq!(d.event_id, "master");
        assert_eq!(d.title, "Meeting");
        assert!(!d.all_day);
        assert_eq!(d.description.as_deref(), Some("notes"));
        assert_eq!(d.attendees, vec!["a@x.com".to_string()]);
        assert_eq!(d.recurrence, vec!["RRULE:FREQ=WEEKLY".to_string()]);
    }

    #[test]
    fn to_event_details_errors_without_start() {
        let ev = Event {
            id: Some("e".into()),
            ..Default::default()
        };
        assert!(to_event_details("cal", "e", ev).is_err());
    }
}
