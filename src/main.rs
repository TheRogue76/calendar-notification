//! Thin binary wrapper. All logic lives in the library (see `lib.rs`) so it can
//! be exercised by unit and integration tests.

fn main() -> iced::Result {
    calendar_notification::run()
}
