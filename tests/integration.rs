//! Integration tests exercising the crate's public API across module
//! boundaries (as an external consumer would). Fine-grained logic is covered by
//! the in-module unit tests; these guard the public surface and the key
//! cross-module contracts.

use calendar_notification::config::Config;
use calendar_notification::engine::CalendarSource;
use calendar_notification::google::model::{Calendar, NewEvent, Occurrence, ReminderRule};
use calendar_notification::ui::recurrence::{Recurrence, Weekdays};

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};

#[test]
fn config_roundtrips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    let mut cfg = Config::load_or_create_at(&path).unwrap();
    assert!(!cfg.has_credentials());

    cfg.client_id = "id".into();
    cfg.client_secret = "sec".into();
    cfg.poll_interval_minutes = 10;
    cfg.ensure_calendar("cal-1", "#abcdef");
    cfg.save_to(&path).unwrap();

    let reloaded = Config::load_or_create_at(&path).unwrap();
    assert!(reloaded.has_credentials());
    assert_eq!(reloaded.poll_interval_minutes, 10);
    assert_eq!(reloaded.calendars["cal-1"].color, "#abcdef");
}

#[test]
fn recurrence_serializes_to_rrule() {
    let thursday = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
    let wd = Weekdays::from_date(thursday);
    let rules = Recurrence::Weekly(wd).to_rrule(Some(thursday));
    assert_eq!(rules.len(), 1);
    assert!(rules[0].starts_with("RRULE:FREQ=WEEKLY;BYDAY=TH"));
    assert!(rules[0].contains("UNTIL=20260702"));

    assert!(Recurrence::None.to_rrule(None).is_empty());
}

#[test]
fn occurrence_reminder_math_is_public() {
    let start = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
    let occ = Occurrence {
        event_id: "e".into(),
        calendar_id: "c".into(),
        title: "t".into(),
        location: None,
        start,
        end: start,
        all_day: false,
        reminders: vec![ReminderRule { minutes: 15 }, ReminderRule { minutes: 60 }],
    };
    let fires: Vec<_> = occ.reminder_fire_times().collect();
    assert_eq!(fires.len(), 2);
    assert_eq!(fires[0].1, 15);
    assert!(occ.occurrence_key().starts_with("e::"));
}

/// A minimal external implementation of the public [`CalendarSource`] trait,
/// proving it's usable by downstream code / test doubles.
struct FakeSource;

impl CalendarSource for FakeSource {
    async fn list_calendars(&self) -> anyhow::Result<Vec<Calendar>> {
        Ok(vec![Calendar {
            id: "primary".into(),
            summary: "Primary".into(),
            color: "#4285F4".into(),
            primary: true,
        }])
    }
    async fn list_events(
        &self,
        _calendar_id: &str,
        _min: DateTime<Utc>,
        _max: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Occurrence>> {
        Ok(vec![])
    }
    async fn insert_event(&self, _new: &NewEvent) -> anyhow::Result<String> {
        Ok("created-id".into())
    }
}

#[tokio::test]
async fn calendar_source_trait_is_publicly_implementable() {
    let src = FakeSource;
    let cals = src.list_calendars().await.unwrap();
    assert_eq!(cals.len(), 1);
    assert!(cals[0].primary);
    assert!(src
        .list_events("primary", Utc::now(), Utc::now())
        .await
        .unwrap()
        .is_empty());

    let new = NewEvent {
        calendar_id: "primary".into(),
        title: "Hi".into(),
        location: None,
        description: None,
        all_day: false,
        start: Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap(),
        end: Local.with_ymd_and_hms(2026, 7, 2, 10, 0, 0).unwrap(),
        attendees: vec![],
        recurrence: vec![],
    };
    assert_eq!(src.insert_event(&new).await.unwrap(), "created-id");
}
