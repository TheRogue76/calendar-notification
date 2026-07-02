# This repo's idioms, with line-referenced examples

Concrete patterns pulled from the current source. When you write new code, match
the nearest existing example. (Line numbers drift — grep the symbol if a citation
looks off; the *pattern* is what matters.)

## Module docs explain *why*, not just *what*

Every file opens with a `//!` block stating the rationale or the constraint that
shaped the code. Examples:

- `engine.rs:1` — explains the dedicated background runtime "so it never contends
  with iced's own executor — this is the plan's isolation strategy".
- `notify.rs:9` — documents *why* `show_reminder` is `async`: the sync `show()`
  "internally spins up a blocking zbus runtime, which panics … inside the engine's
  tokio loop".
- `google/model.rs:1` — "deliberately decoupled from the generated `google-calendar3`
  structs so the rest of the app never touches the raw API shapes".

New file → open with a `//!` that says why it exists and any non-obvious constraint.
New public item → `///` with a one-line first sentence.

## Error handling: `anyhow` everywhere (see also error-handling.md)

- Signature + import: `use anyhow::{Context, Result};` then `-> Result<T>`
  (`config.rs:9`, `google/client.rs:6`).
- Static context: `.context("listing calendars")?` (`client.rs:50`).
- Interpolated context: `.with_context(|| format!("reading {}", path.display()))?`
  (`config.rs:107`).
- Human formatting in logs: `warn!("could not persist config: {e:#}")`
  (`engine.rs:108`); `error!("notification failed: {e:#}")` (`engine.rs:265`).
- Foreign-error boundary: `Err(anyhow::Error::new(e)).with_context(|| format!(...))`
  (`client.rs:109`).
- `anyhow::bail!("offline")` for early error return (`engine.rs`, test `FakeSource`).

## Control-flow idioms

- **`let ... else`** to skip/bail instead of nesting: `let Some(id) = entry.id else { continue };`
  (`client.rs:54`).
- **`matches!`** for variant checks: `if !matches!(access, "reader" | "writer" | "owner")`
  (`client.rs:64`); pervasive in test assertions
  (`matches!(ev, UiEvent::Occurrences(_))`, `engine.rs:625`).
- **Iterator pipelines over manual loops**: `.iter().map(|c| { … }).collect()`
  (`engine.rs:120`), `.map(...).filter(...).unwrap_or_else(...)` for the color
  fallback (`engine.rs:128`).
- **`Option` combinators**: `prefs.map(|p| p.visible).unwrap_or(true)` (`engine.rs:133`);
  `.entry(id).or_default().visible = v` (`engine.rs:276`);
  `.entry(...).or_insert_with(|| CalendarPrefs { … })` (`config.rs:80`).
- **No `unwrap`/`expect` in runtime paths.** The only `expect`s are startup
  invariants with a message: `.expect("no native root certificates found")`
  (`client.rs:29`). Errors are propagated with `?` or converted to a `UiEvent`.
  `unwrap()` is used freely in tests only.

## Traits & async

- **One abstraction seam, `CalendarSource`** (`engine.rs:28`), declared
  `#[allow(async_fn_in_trait)]` with plain `async fn` methods — **no `async-trait`
  crate**. The engine is generic over it: `struct Engine<C: CalendarSource>`
  (`engine.rs:85`). Real impl: `GoogleClient` (`client.rs:41`). Test impl:
  `FakeSource` (`engine.rs:423`) and a fresh one in `tests/integration.rs`.
- **The `tokio::select!` loop** (`engine.rs:369`) merges command-hub, poll, and
  reminder arms. The dormant-arm trick — `std::future::pending::<()>().await` when
  nothing is scheduled (`engine.rs:365`) — avoids any `unwrap`/precondition coupling.
  Reuse this shape rather than adding a second runtime or a busy-wait.

## Domain / API decoupling

- Domain types live in `google/model.rs` (`Calendar`, `Occurrence`, `ReminderRule`,
  `NewEvent`) and derive `Debug`/`Clone` (+ `PartialEq`/`Eq`/`Copy` where cheap,
  e.g. `ReminderRule` at `model.rs:19`).
- **All** `google-calendar3` ↔ domain conversion is isolated in `client.rs`
  (`to_occurrence`, `reminder_rules`, `build_event`, `parse_start/parse_end`).
  Nothing else imports the generated `api::*` structs.
- New domain concept → add to `model.rs`. New API mapping → add to `client.rs`.

## Types over primitives / small newtypes

- `ReminderRule { minutes: i64 }` (`model.rs:20`) is a one-field struct rather than
  a bare `i64`, so reminder math reads meaningfully.
- Behavior hangs off the domain type as methods: `Occurrence::occurrence_key`
  (`model.rs:43`) and `Occurrence::reminder_fire_times` returning
  `impl Iterator<Item = (DateTime<Utc>, i64)> + '_` (`model.rs:48`) — exposes the
  iterator (C-ITER / C-INTERMEDIATE) instead of allocating a `Vec`.

## Serde config shape

- `Config`/`CalendarPrefs` derive `Serialize`/`Deserialize` with `#[serde(default)]`
  and named default fns (`default_true`, `default_poll_interval`) so partial TOML
  fills sensible defaults (`config.rs:14`–`70`). `has_credentials` validates
  (`config.rs:74`); `ensure_calendar` merges without clobbering (`config.rs:79`).

## Naming quick-reference (as used here)

- Testable cores use an `_at`/`_to` suffix: `load_or_create_at`, `save_to`,
  `config_path`, `token_path`.
- Free helpers are verbs/nouns without a receiver: `encode_calendar_id`,
  `deadline_for`, `lead_phrase`, `reminder_rules`, `build_event`.
- Enums for messages/commands: `UiEvent`, `Command`, `Message` (in `app.rs`),
  `FormMsg` — one variant per distinct event, documented with `///`.
