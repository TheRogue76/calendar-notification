# Error handling in this repo (`anyhow`)

This is a **binary application**, so it uses `anyhow` end-to-end (Rust guideline
M-APP-ERROR: "Applications may use Anyhow"). **Do not add `thiserror`** or define
custom error enums — that's the library pattern, and it isn't used here. The
general rule of thumb (background, not what this repo does): `thiserror` for
libraries whose callers match on error variants; `anyhow` for applications that
log/display errors. See <https://github.com/dtolnay/anyhow>.

## The patterns, in order of how often you'll write them

### 1. Signature and imports
```rust
use anyhow::{Context, Result};

pub fn load_or_create_at(path: &Path) -> Result<Self> { … }
```
`Result` here is `anyhow::Result<T>` (alias for `Result<T, anyhow::Error>`).

### 2. Add context on every `?`
Static string — use `.context(...)`:
```rust
.doit().await.context("listing calendars")?;              // client.rs:50
let text = toml::to_string_pretty(self).context("serializing config")?;  // config.rs:123
```
Interpolated — use `.with_context(|| format!(...))` (lazy, only formats on error):
```rust
std::fs::read_to_string(path)
    .with_context(|| format!("reading {}", path.display()))?;   // config.rs:107
```
Context reads as "what we were trying to do", lower-case, no trailing period —
they stack into a chain like `writing /path: permission denied`.

### 3. Format errors for humans in logs with `{e:#}`
The alternate `#` selector prints the full `anyhow` cause chain on one line:
```rust
warn!("could not persist config: {e:#}");     // engine.rs:108
error!("notification failed: {e:#}");          // engine.rs:265
warn!("events fetch failed for {}: {e:#}", c.id);
```
Use `tracing` macros (`warn!`/`error!`/`info!`/`debug!`), never `println!`.

### 4. Wrap a foreign (non-`anyhow`) error at the boundary
```rust
return Err(anyhow::Error::new(e))
    .with_context(|| format!("listing events for {calendar_id}"));   // client.rs:109
```

### 5. Early returns / validation
```rust
anyhow::bail!("offline");                       // return an error now
anyhow::ensure!(cond, "message with {value}");  // bail unless cond holds
```

### 6. Convert an error to a UI-facing string when it crosses the channel boundary
The engine reports failures to the UI as `String`, not `anyhow::Error` (which
isn't `Clone`):
```rust
let result = self.client.insert_event(&new).await.map_err(|e| format!("{e:#}"));
self.emit(UiEvent::EventCreated(result));       // engine.rs:290
```

## When to propagate vs. swallow

- **Propagate with `?`** in the fallible I/O cores (`config.rs`, `client.rs`) — the
  caller decides.
- **Swallow-and-report** for non-fatal runtime conditions in the engine loop: log
  at `warn!` with `{e:#}` and emit a `UiEvent::Status` instead of tearing down the
  loop (see `resync`, `engine.rs:160`). An offline sync must not kill the daemon.
- **Ignore intentionally** where a failure is truly nothing: `let _ = self.ui_tx.send(ev);`
  (`engine.rs:114`) — the UI may not be listening yet; the `let _ =` makes the
  intent explicit (and keeps clippy quiet).

## Panics

Panics are for *programming bugs*, not recoverable conditions (M-PANIC-ON-BUG).
The two `expect`s in runtime code are startup invariants that can't sensibly
continue, each with a message (`client.rs:29`, plus the thread spawn in `main.rs`).
A panic anywhere takes the whole process down by design (fail-fast hook → exit 101;
see `CLAUDE.md`). Prefer converting fallible work to `Result`/`Option` over
`unwrap`/`catch_unwind`.
