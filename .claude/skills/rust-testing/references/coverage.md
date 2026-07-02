# Coverage: commands, the gate, and what's intentionally uncovered

The project holds a **≥80% line-coverage goal** (currently ~84%) measured by
`cargo llvm-cov`. Keep it there or higher; **any code you add should come with
tests**.

## Commands
```bash
cargo llvm-cov --summary-only    # per-file + total table (run before/after edits)
cargo llvm-cov --open            # HTML report — shows exactly which lines are uncovered
```
Requires the `llvm-tools-preview` rustup component and `cargo-llvm-cov` (both in the
repo's permission allowlist). Use `llvm-cov`, **not** tarpaulin, for the number that
matches CI.

## The CI gate (don't get surprised by a red check)
- `.github/workflows/ci.yml` `coverage` job runs:
  ```bash
  cargo llvm-cov --all-features --workspace --codecov --output-path codecov.json
  ```
  and uploads to Codecov with `fail_ci_if_error: true`.
- `codecov.yml` sets **two** gates, each target **80%**:
  - `project` (whole-repo, 2% threshold), and
  - `patch` — **your PR's changed lines must be ≥80% covered.** This is the one that
    turns red when you ship untested new logic, and it comments on the PR.
- `src/main.rs` is in Codecov's `ignore` list.

So: after adding logic, run `cargo llvm-cov --summary-only`, open the HTML report if
the number dipped, and add tests for the uncovered lines you introduced.

## Intentionally uncovered — don't chase these with brittle mocks
These need a live GUI / network / D-Bus / OAuth and aren't unit-testable. Verify
them by running the app, not by mocking the world:
- `lib.rs::run` — wires runtimes, threads, channels, launches the iced daemon.
- `google/auth.rs` — `yup-oauth2` interactive InstalledFlow.
- `GoogleClient` network method **bodies** in `google/client.rs` (the *pure helpers*
  like `encode_calendar_id`, `reminder_rules`, `to_occurrence`, `build_event` **are**
  tested — keep those covered).
- `notify::show_reminder` — real freedesktop notification (its pure helper
  `lead_phrase` is fully tested).
- `main.rs` — thin entry point (also Codecov-ignored).
- Much of the `ksni` tray / iced rendering path.

If a new piece of logic genuinely falls into one of these buckets, **say so
explicitly** in your summary rather than silently skipping it or faking the layer.

## Rule of thumb
Put testable logic in a pure helper or the injectable core (`*_at`/`save_to`/
`run_loop`, a `CalendarSource` method) so it *can* be covered, then cover it. If you
find yourself wanting to mock OAuth/HTTP/D-Bus to hit a number, restructure so the
logic lives in a pure function instead.
