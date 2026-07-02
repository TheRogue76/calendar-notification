---
name: idiomatic-rust
description: Write and review idiomatic Rust in the calendar-notification repo. Use whenever adding or modifying Rust code (any src/*.rs), reviewing a Rust diff, or making API/naming/error-handling/trait/async decisions. Leads with the authoritative Rust guidelines, then this repo's concrete conventions.
---

# Idiomatic Rust for `calendar-notification`

This skill keeps committed Rust idiomatic. It is **guidelines-first**: apply the
authoritative Rust guidelines below, then the repo-specific conventions that
layer on top. For architecture/gotchas (the two-runtime split, rustls provider,
calendar-id encoding, window lifecycle), see `CLAUDE.md` — this skill is about
*how the code is written*, not what it does.

`edition = "2021"`. Strictness is enforced in CI (`cargo clippy --all-targets --
-D warnings`, `cargo fmt --check`), not by in-source `#![deny]` attributes — so
**clippy warnings are hard errors**. Treat clippy as authoritative and obey it.

## Core idiomatic principles (from the Rust API + MS Pragmatic guidelines)

Apply these by default. The full checklists live in the reference files — consult
them when designing a public type, trait, or error, or when reviewing.

- **Naming (RFC 430).** `snake_case` fns/modules, `CamelCase` types, `SCREAMING_SNAKE`
  consts. Conversions follow `as_`/`to_`/`into_` (C-CONV); iterator producers are
  `iter`/`iter_mut`/`into_iter` (C-ITER); getters drop the `get_` prefix (C-GETTER).
  Consistent word order across the crate (C-WORD-ORDER). Names are short and free of
  weasel words (M-SHORT-NAMES, M-WEASEL-WORDS).
- **Types over primitives.** Convey meaning with types, not `bool`/`Option` args
  (C-CUSTOM-TYPE); use newtypes for static distinctions and to guard invariants
  (C-NEWTYPE, M-STRONG-TYPES-GUARD). Prefer concrete types > generics > `dyn`
  (M-DI-HIERARCHY).
- **Derive the common traits eagerly** where they make sense: `Debug`, `Clone`,
  `PartialEq`/`Eq`, `Copy` for small POD, `Default` (C-COMMON-TRAITS). **All public
  types implement `Debug`** (C-DEBUG, M-PUBLIC-DEBUG); types meant to be read
  implement `Display` (M-PUBLIC-DISPLAY). Conversions use `From`/`TryFrom`/`AsRef`
  (C-CONV-TRAITS) — a `From` impl, not a `map_err` closure, for error conversion
  (M-FROM-ERROR).
- **Errors are meaningful and well-behaved** (C-GOOD-ERR). Validate arguments
  (C-VALIDATE). Detected programming bugs `panic!`; recoverable conditions return
  `Result` (M-PANIC-ON-BUG). Prefer `?` over `unwrap`/`expect` (C-QUESTION-MARK).
- **`async fn` directly** (not hand-returned futures) where possible (M-ASYNC-FN);
  give long-running loops yield points (M-YIELD-POINTS).
- **Docs.** Every module opens with a `//!` doc; every public item gets a `///` doc
  whose first sentence is one line (~15 words) (M-FIRST-DOC-SENTENCE). Document
  panics/errors and any magic values (C-FAILURE, M-DOCUMENTED-MAGIC).
- **Avoid statics** (M-AVOID-STATICS) and avoid `unsafe` (M-UNSAFE) — this repo has
  neither in normal paths (one deliberate `static OnceLock` bridge in `app.rs`, see
  `CLAUDE.md`). **Macros are a last resort** (M-MACRO-LAST-RESORT).
- **Structs have private fields** unless they are plain data carriers (C-STRUCT-PRIVATE);
  keep each item reachable by one path (M-SINGLE-ITEM-PATH); no glob re-exports
  (M-NO-GLOB-REEXPORTS) — this repo re-exports explicitly in `mod.rs`.
- Prefer structured logging via `tracing`, never `println!` in runtime code
  (M-LOG-NOT-PRINT).

## This repo's conventions (match these exactly)

1. **Error handling = `anyhow`, never `thiserror`.** This is a binary app, so it uses
   `anyhow` throughout (M-APP-ERROR). Fallible fns return `anyhow::Result<T>` with
   `use anyhow::{Context, Result};`. Attach context on every `?`; format errors for
   humans with `{e:#}` in logs. See `references/error-handling.md`.
2. **Control-flow idioms.** `let Some(x) = opt else { continue };`, `matches!` for
   variant checks, iterator chains (`filter_map`/`map`/`collect`), `map`/`map_or`/
   `unwrap_or`/`unwrap_or_else`/`unwrap_or_default`, `.entry(..).or_default()` /
   `.or_insert_with(..)`. No `unwrap()`/`expect()` in runtime paths — the two existing
   `expect`s are startup invariants with an explanatory string. `unwrap()` is fine in
   tests.
3. **One abstraction trait, `async fn` in-trait, no `async-trait` crate.** The engine
   is generic over `CalendarSource` (`engine.rs:29`), declared with
   `#[allow(async_fn_in_trait)]` and plain `async fn` methods. Follow this pattern for
   new seams; the real impl is `GoogleClient`, tests use `FakeSource`.
4. **Domain types stay decoupled from the API.** Types in `google/model.rs` never
   expose `google-calendar3` structs; all conversion is isolated in `google/client.rs`
   (M-DONT-LEAK-TYPES). New domain concepts go in `model.rs`; new conversions in
   `client.rs`.
5. **Module docs explain *why*.** Open each file with a `//!` block that states the
   rationale/constraint, not just the what (see `engine.rs`, `notify.rs`, `config.rs`).
6. **Injectable-core testability split.** A public I/O wrapper delegates to a
   path-injectable core: `save()`→`save_to(path)`, `load_or_create()`→
   `load_or_create_at(path)`, `run()`→`run_loop(engine, rx)`. Preserve this shape for
   anything that touches disk/network so it stays unit-testable. (See the
   `rust-testing` skill.)

For concrete, line-referenced examples of every convention above, read
`references/repo-conventions.md`.

## Before finishing any Rust change

Run and make clean (see the `rust-testing` skill for coverage):

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Add tests for new logic (the repo holds a ≥80% coverage gate) — invoke the
`rust-testing` skill for how.

## Reference files

- `references/rust-api-guidelines.md` — the full Rust API Guidelines checklist (C-*).
- `references/pragmatic-rust-guidelines.md` — Microsoft Pragmatic Guidelines (M-*),
  marked for which apply to this binary app vs libraries only.
- `references/repo-conventions.md` — this repo's idioms with `file:line` examples.
- `references/error-handling.md` — the `anyhow` patterns in depth.
