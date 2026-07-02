---
name: rust-testing
description: Write tests and keep coverage green in the calendar-notification repo. Use when adding or changing Rust that needs test coverage, writing unit/integration tests, or checking the ≥80% cargo llvm-cov gate. Leads with the Rust testing guidelines, then this repo's concrete patterns.
---

# Testing & coverage for `calendar-notification`

A change here is **incomplete until its new logic is tested**. CI enforces a
**≥80% line-coverage gate on both the whole project and each PR's changed lines**
(`codecov.yml`, via `cargo llvm-cov`), so untested new code shows up as a red
check. This skill is guidelines-first, then the repo's concrete patterns.

## Testing principles (from the Rust guidelines)

- **Integration tests live under `tests/`** (M-INTEGRATION-TESTS) and exercise the
  crate as an external consumer would.
- **I/O and system calls are mockable** behind a trait (M-MOCKABLE-SYSCALLS) so
  logic is unit-testable without network/GUI/D-Bus. This repo's seam is the
  `CalendarSource` trait.
- **Tests don't assert ground truth back to themselves** (M-TAUTOLOGICAL-TESTS) —
  assert real behavior and edge boundaries, not that a constant equals itself.
- Test utilities are `#[cfg(test)]`-gated (M-TEST-UTIL), never shipped in release.

## This repo's testing conventions (match these)

1. **Unit tests are inline**, at the bottom of each module:
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;
       // …
   }
   ```
   They can reach private fns — that's where most coverage comes from. Group with
   section comments (`// -- next_reminder ---`). See `engine.rs:394`, `config.rs:144`.
2. **Small builder helpers** at the top of the test module keep cases terse:
   `cal(...)`, `occ(...)`, `engine_with(...)`, `drain(...)` (`engine.rs:446`–`500`).
   Reuse/extend these rather than hand-building structs in every test.
3. **`tempfile::tempdir()` for filesystem isolation** — tests **never** touch the
   real `~/.config`. Inject the temp path via the injectable core (`config.rs:205`,
   `engine.rs:486`).
4. **The injectable-core split is what makes logic testable.** Public I/O wrappers
   delegate to a path/dependency-injectable core: `save`→`save_to`,
   `load_or_create`→`load_or_create_at`, `run`→`run_loop(engine, rx)`. Tests build
   an `Engine` directly with a `FakeSource` + injected `config_path`. **Preserve
   this split** for any new disk/network code.
5. **Mock the network with a `FakeSource`**, not HTTP mocks. `impl CalendarSource
   for FakeSource` returns canned data / flips `fail_calendars` to simulate offline
   (`engine.rs:405`). `tests/integration.rs` defines its own external `FakeSource`
   to prove the trait is publicly implementable.
6. **`#[test]` for sync, `#[tokio::test]` for async** (`engine.rs:581`,
   `resync`/`handle_command` tests). Assert on channel output by `drain`-ing the
   `UiEvent` receiver and matching variants with `matches!`.
7. **Assertions carry messages** explaining intent:
   `assert!(!cfg.has_credentials(), "blank secret does not count");`.
8. When a test mutates a `Default::default()` field-by-field, the module may need
   `#![allow(clippy::field_reassign_with_default)]` at its top (`config.rs:146`).

Concrete, copyable snippets: `references/testing-patterns.md`.

## Coverage workflow

Run before finishing any change that adds/changes logic:
```bash
cargo llvm-cov --summary-only    # table; check the total and your file
cargo llvm-cov --open            # HTML report to see uncovered lines
```
If your edit dropped coverage or added untested logic, add tests until you're back
at/above 80%. Full details, the CI command, and the intentionally-uncovered list:
`references/coverage.md`.

**Don't** pad coverage with brittle mocks of GUI/network/D-Bus/OAuth. Those layers
(`lib.rs::run`, `google/auth.rs`, `GoogleClient` network bodies, `notify::show_reminder`,
`main.rs`) are intentionally uncovered — if new logic genuinely can't be unit-tested,
say so explicitly rather than mocking those layers.

## Before finishing
```bash
cargo fmt
cargo clippy --all-targets -- -D warnings   # tests are linted too — keep them clean
cargo test
cargo llvm-cov --summary-only
```
