# Microsoft Pragmatic Rust Guidelines checklist (M-*)

Design guidelines for idiomatic Rust that scales, building on the Rust API
Guidelines. Source: <https://microsoft.github.io/rust-guidelines/> (checklist at
<https://microsoft.github.io/rust-guidelines/guidelines/checklist/index.html>).

This is a **single binary crate** (also exposed as a lib for testing), so the
*Application* and *Universal/Correctness/Docs/Testing* items apply directly; the
*Library / Interoperability / UX* items apply to the crate's public surface
(what `tests/integration.rs` consumes) but with less ceremony than a published
library; *Project (workspace)*, *FFI*, and `-sys` items **do not apply** (no
workspace, no FFI).

## Universal — apply everywhere
- **M-UPSTREAM-GUIDELINES** — Follow the upstream Rust API Guidelines.
- **M-STATIC-VERIFICATION** — Lean on the compiler/types for correctness.
- **M-LINT-OVERRIDE-EXPECT** — Prefer `#[expect(...)]` over `#[allow(...)]` for lint overrides so stale suppressions surface. (Repo currently uses `#[allow]`; use `#[expect]` for new suppressions where the lint is reliably triggered.)
- **M-PUBLIC-DEBUG** — Public types are `Debug`.
- **M-PUBLIC-DISPLAY** — Public types meant to be read are `Display`.
- **M-WEASEL-WORDS** — Names are free of weasel words ("manager", "helper", "util").
- **M-SHORT-NAMES** — Item names are short; the module path already gives context.
- **M-REGULAR-FN** — Prefer free/regular functions over associated functions when there's no clear receiver (this repo's free helpers: `encode_calendar_id`, `deadline_for`, `lead_phrase`).
- **M-DOCUMENTED-MAGIC** — Magic values are documented (e.g. `REMINDER_WINDOW_HOURS`, the 5-min staleness cutoff).
- **M-LOG-STRUCTURED** — Use structured logging with message templates (`tracing`).

## Correctness — apply everywhere
- **M-UNSAFE** — `unsafe` needs a written reason and should be avoided (repo has none).
- **M-UNSAFE-IMPLIES-UB** / **M-UNSOUND** — Never write unsound code.
- **M-PANIC-IS-STOP** — A panic means "stop the program" (repo's fail-fast hook exits 101).
- **M-PANIC-ON-BUG** — Detected programming bugs are panics; recoverable conditions are `Result`.
- **M-PANIC-MESSAGE** — Custom panics/`expect` carry a helpful message.

## Documentation — apply everywhere
- **M-FIRST-DOC-SENTENCE** — First doc sentence is one line, ~15 words.
- **M-MODULE-DOCS** — Comprehensive `//!` module docs (this repo does this well — keep it up).
- **M-CANONICAL-DOCS** — Use canonical sections (`# Errors`, `# Panics`, `# Examples`).
- **M-DOC-INLINE** — Mark re-exported `pub use` items with `#[doc(inline)]` where helpful.

## Applications — apply directly
- **M-APP-ERROR** — Applications may use `anyhow` (or derivatives). **This repo uses `anyhow`** — do not introduce `thiserror`.
- **M-MIMALLOC-APPS** / **M-TARGET-CPU** — Optional perf tuning (allocator, target-cpu); not currently used, not required.

## Library surface / UX — apply with judgment to the public API
- **M-DI-HIERARCHY** — Prefer concrete types over generics over `dyn` traits.
- **M-ERRORS-CANONICAL-STRUCTS** / **M-FROM-ERROR** — For structured errors, use canonical error structs and `From` (not `map_err`) for conversion. *N.B. this repo deliberately uses `anyhow` end-to-end instead — these library-style rules are the exception here.*
- **M-INIT-BUILDER** / **M-BUILD-RESULT** — Complex construction uses builders that validate in `.build()`.
- **M-ESSENTIAL-FN-INHERENT** — Essential functionality is inherent methods, not extension traits.
- **M-BALANCED-MODULES** — Modules balanced in size/scope (one concern per file — matches this repo).
- **M-NO-PRELUDE** / **M-NO-GLOB-REEXPORTS** — No preludes; no glob re-exports (repo re-exports explicitly in `mod.rs`).
- **M-PARAMETER-CONSISTENCY** — Consistent parameter ordering across related fns.
- **M-COLLECTION-TRAITS** — Collections implement the appropriate iterator traits.
- **M-ASYNC-FN** — Prefer `async fn` over returning a `Future` by hand.
- **M-DONT-LEAK-TYPES** — Don't leak external/generated types (repo isolates `google-calendar3` in `client.rs`).
- **M-IMPL-ASREF** — Accept `impl AsRef<...>` where feasible.

## Resilience — apply to testable code
- **M-MOCKABLE-SYSCALLS** — I/O and system calls are mockable behind a trait (repo's `CalendarSource`).
- **M-INTEGRATION-TESTS** — Integration tests live under `tests/`.
- **M-TEST-UTIL** — Test utilities are feature-gated / `#[cfg(test)]`.
- **M-STRONG-TYPES** / **M-STRONG-TYPES-GUARD** — Use the proper type family; newtypes guard invariants.
- **M-AVOID-STATICS** — Avoid statics (repo has one deliberate `OnceLock` bridge, documented).
- **M-LOG-NOT-PRINT** — Production code uses telemetry (`tracing`), not `println!`.

## Performance — apply on hot paths only
- **M-YIELD-POINTS** — Long-running tasks have yield points (the engine's `tokio::select!` loop).
- **M-HOTPATH** / **M-THROUGHPUT** — Profile before optimizing; don't micro-optimize cold paths.
- **M-MEM-REUSE**, **M-BOX-DST**, **M-SHRINK-TO-FIT**, **M-FAST-HASHER**, **M-INITIAL-CAPACITY** — Allocation/capacity tuning where measurement justifies it.

## Macros — apply if you ever reach for one
- **M-MACRO-LAST-RESORT** — Macros are a last resort; prefer functions/generics.
- **M-EXAMPLE-OVER-PROC** — Prefer macros-by-example over proc macros.
- **M-MACROS-DONT-LIE** — Macros don't lie about the signatures they expand to.

## AI-facing — this codebase is edited by agents
- **M-DESIGN-FOR-AI** — Design with AI use in mind (clear names, single obvious path).
- **M-SINGLE-ITEM-PATH** — Each item is visible through exactly one path.
- **M-TAUTOLOGICAL-TESTS** — Tests do not assert ground truth back to themselves.
- **M-RUST-SHAPED** — Solve Rust problems the Rust way; don't port another language's patterns.

## Not applicable to this repo
- **Project / workspace**: M-CARGO-WORKSPACE, M-CRATES-IN-WORKSPACE, M-CRATES-FLAT-FOLDER (single crate).
- **FFI**: M-ISOLATE-DLL-STATE, M-FFI-TRANSLATES, M-FFI-NAMING (no FFI).
- **`-sys`**: M-SYS-CRATES (no native sys crate).
- **M-LATEST-EDITION**: repo pins `edition = "2021"` deliberately; don't bump without cause.
