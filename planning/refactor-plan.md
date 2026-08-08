# pid-ctl Structural Refactor Plan

## Context

A staff-engineer structural review of `/home/user/pid-ctl` surfaced ten issues that are making the codebase harder to maintain, inflating bug risk, and hindering LLM editing. The most acute problems are all in the binary crate `crates/pid-ctl/`:

- `main.rs` has grown to 1,203 LOC and doubles as both the CLI entry point and a catch-all for twelve `pub(crate)` helpers that the `tune` module reaches back into (`tune/mod.rs:10-21`). This welds `tune` to the binary crate.
- `run_loop` (`main.rs:222-393`, `#[allow(clippy::too_many_lines)]`) and `tune::run` (`tune/mod.rs:45-360`, same allow) duplicate ~70% of their per-tick logic. Bug fixes in one are easy to miss in the other.
- An `Option<&mut std::fs::File>` log-file handle is threaded through 15+ signatures, tangling logging with business logic. A process-global `AtomicBool` (`json_events.rs:13`) with RAII guard suppresses stderr during the TUI — correct today, but a latent race the moment threading is introduced.
- `ControllerSession` (`app/mod.rs:57-70`) conflates PID orchestration, persistence, and failure-count escalation.
- `LoopArgs` has 36 fields including four `explicit_*: bool` twins whose invariant (`explicit_foo == true ⇔ user supplied foo`) must be manually maintained across parse + runtime sites.
- 40+ scattered `#[cfg(unix)]` guards, two LOW-severity socket cleanups, and zero unit tests for the event-loop logic (only subprocess integration tests exist).

The integration tests at `crates/pid-ctl/tests/req/*.rs` (~4490 LOC, spawned via `assert_cmd`) are the canonical behavior contract and must keep passing byte-for-byte throughout every phase.

The intended outcome: a sequenced refactor that (a) extracts shared helpers into the library, (b) unifies the two tick paths behind a testable driver, (c) replaces the global log/suppress plumbing with an owned `Logger`, and (d) tightens `ControllerSession` and `LoopArgs`. Each phase is independently shippable.

---

# pid-ctl Staff-Engineer Refactor Plan

All 10 findings independently validated. Additional context I confirmed:

- `main.rs` is exactly 1203 lines; `tune/mod.rs` is 430; tune imports 12 items from `crate::` (main.rs's `pub(crate)` surface).
- The `tui` feature is `default = ["tui"]` — ratatui/crossterm are optional deps. The `#[cfg(feature = "tui")] mod tune` lives in the binary crate.
- `run_pipe` also handles its own tick-shaped code path independently of both `run_loop_tick` and `tune_tick` — it is simpler but shares the "process_pv → emit iteration JSON → handle state_write_failed" skeleton.
- `run_once` is another variation of the same pattern (≈ 60 lines lines 108–166).
- `LoopArgs` session_config ignores the `explicit_*` booleans (they're not part of `SessionConfig`), so those twins are CLI-side only.
- `Response::ErrorUnknownCommand` and `ErrorUnknownParam` have `ok: bool` (set false in practice) — every variant has `ok` per finding #4.
- `sleep_with_socket` in main.rs:842–876 is a second caller of `handle_socket_request`. Tune has its own inline caller (tune/mod.rs:148–169). That's two callers total.

---

## Sequencing Overview

The 10 issues have a strict dependency chain; you cannot pull the high-severity items in any order.

```
Phase 0 (groundwork, no behavior change)
  └─ #4 socket_send_and_print ok accessor   (LOW, isolated)
  └─ #5 collapse 4 socket wrapper fns       (LOW, isolated)
  └─ #6 central cfg(unix) module boundary   (MED, cosmetic)

Phase 1 (lift shared helpers out of main.rs)
  └─ #1 extract main.rs helpers into library modules   (HIGH)
        ├─ app/loop_runtime.rs    (build_pv_source, build_cv_sink, open_log_optional, print_iteration_json, handle_dt_skip_state_write, emit_state_write_failure, apply_measured_dt, write_safe_cv, MeasuredDt)
        ├─ app/socket_dispatch.rs (handle_socket_request, apply_runtime_interval, SocketSideEffect)
        └─ cli/error.rs           (CliError, OutputFormat, LoopArgs already in cli/types.rs)

Phase 2 (extract Logger abstraction) — depends on #1
  └─ #8 replace Option<&mut File> with Logger sink     (HIGH)
        └─ json_events.rs becomes Logger-parameterized; same sites just pass &mut logger
  └─ #7 move AtomicBool into Logger.suppress_stderr field (LOW) — free once #8 lands

Phase 3 (extract tick abstraction) — depends on #1 and #8
  └─ #3 shared `tick()` driver taking a TickObserver   (HIGH)
        └─ both run_loop and tune::run call Ticker::step(...)
  └─ #10 unit tests for tick logic     (MED) — free once #3 exposes a testable surface

Phase 4 (simplify ControllerSession surface) — depends on #3 clarifying the contract
  └─ #9 split ControllerSession or isolate persistence escalation (MED)

Phase 5 (CLI ergonomics, no tick touch)
  └─ #2 collapse explicit_* twins into UserSet<T>      (MED)

Phase 6 (optional) — tune migration — depends on everything above
  └─ alternative (a): move tune to library — evaluated below, recommend keeping in bin
```

**Rationale for the order:** The top three HIGH-severity findings are entangled. You can't extract a shared tick (#3) until the helpers it needs (`build_pv_source`, `apply_measured_dt`, `write_safe_cv`, `emit_state_write_failure`) live outside `main.rs`, which is #1. You also don't want to extract a shared tick while `Option<&mut File>` is still threaded (#8) because you'd refactor the tick twice. Phase 0 is pure-win cleanup independent of everything. Phases 4–5 are ergonomic follow-ups that benefit from the earlier structural work.

---

## Per-Issue Target End State

### #1 — main.rs decomposition (HIGH)

**Target end state**

`main.rs` shrinks to the subcommand dispatcher + thin `run_*` entry points (< 400 LOC). Shared helpers move to library modules so `tune` can depend on them without reaching back into the binary.

**New files**
- `crates/pid-ctl/src/app/loop_runtime.rs` — new. Hosts:
  - `pub enum MeasuredDt { Skip, Use(f64) }`
  - `pub fn apply_measured_dt(raw_dt: f64, min_dt: f64, max_dt: f64, dt_clamp: bool, quiet: bool, logger: &mut Logger) -> MeasuredDt`
  - `pub fn write_safe_cv(safe_cv: Option<f64>, sink: &mut dyn CvSink, session: &mut ControllerSession)`
  - `pub fn handle_dt_skip_state_write(err: Option<StateStoreError>, session: &ControllerSession, state_path: Option<&Path>, logger: &mut Logger, quiet: bool)`
  - `pub fn emit_state_write_failure(session: &ControllerSession, state_path: Option<&Path>, logger: &mut Logger, err: &StateStoreError, quiet: bool)`
  - `pub fn flush_state_at_shutdown(session: &mut ControllerSession, state_path: Option<&Path>, logger: &mut Logger)`
  - `pub fn millis_round_u64(ms: f64) -> u64`
- `crates/pid-ctl/src/app/adapters_build.rs` — new. Hosts `build_pv_source` and `build_cv_sink`. These are binary-only today because they depend on `cli::types::{PvSourceConfig, CvSinkConfig}`. Resolve by moving the two small config enums into `app::adapters_build` (or a neutral `app::io_config`) so the library owns them. `cli/types.rs` re-exports them.
- `crates/pid-ctl/src/app/socket_dispatch.rs` — new (unix only). Hosts `handle_socket_request`, `handle_socket_set`, `apply_gain_param`, `apply_runtime_interval`, `SocketSideEffect`. Takes `&mut dyn LoopControls` (a small trait) rather than `&mut LoopArgs` so the library does not need to know `LoopArgs` shape.
  - `pub trait LoopControls { fn interval(&self) -> Duration; fn set_interval(&mut self, d: Duration); fn max_dt(&self) -> f64; fn set_max_dt_unless_explicit(&mut self, v: f64); fn pv_stdin_timeout(&self) -> Duration; fn set_pv_stdin_timeout_unless_explicit(&mut self, d: Duration); fn state_write_interval(&self) -> Option<Duration>; fn set_state_write_interval_unless_explicit(&mut self, d: Option<Duration>); }`
  - `impl LoopControls for LoopArgs` stays in `cli/types.rs`.
- `crates/pid-ctl/src/bin_support/` (or rename the existing `cli` module) keeps `CliError`, `OutputFormat`, `print_iteration_json`. `CliError` stays binary-only; library code returns typed errors that `main.rs` converts.

**Deletions**
- 12 `pub(crate)` items removed from `main.rs`.
- The `crate::*` wildcard `use` from tune/mod.rs line 4 goes away; tune imports from `pid_ctl::app::loop_runtime` and `pid_ctl::app::socket_dispatch`.

**Reused utilities**
- `json_events::*` stays where it is (it's already in the library).
- `schedule::next_deadline_after_tick` — already library.
- `pid_ctl::socket` — already library.

**Test impact**
- Integration tests: **unchanged.** No CLI surface change.
- `app/mod.rs` existing unit tests stay.
- `src/tune/tests.rs` imports shift from `crate::CliError` / `crate::LoopArgs` to re-exports; sub-import churn only.

---

### #2 — `LoopArgs` explicit_* twins → `UserSet<T>` (MED)

**Target end state**

Replace four `(value: T, explicit_*: bool)` pairs with one wrapper.

```rust
// in a new cli/user_set.rs
pub(crate) enum UserSet<T> { Explicit(T), Default(T) }
impl<T> UserSet<T> {
    pub fn value(&self) -> &T;
    pub fn into_value(self) -> T;
    pub fn is_explicit(&self) -> bool;
    pub fn set_if_default(&mut self, v: T);   // no-op if Explicit
}
```

`LoopArgs` changes:
- `min_dt: UserSet<f64>` (previously `min_dt: f64` + `explicit_min_dt: bool`)
- `max_dt: UserSet<f64>`
- `pv_stdin_timeout: UserSet<Duration>`
- `state_write_interval: UserSet<Option<Duration>>`

`apply_runtime_interval` (now in `app::socket_dispatch`) uses `set_if_default` rather than consulting twin booleans. The `LoopControls` trait's three `*_unless_explicit` methods delegate to `UserSet::set_if_default`.

Call sites that read the values (e.g. `apply_measured_dt(args.min_dt, args.max_dt, ...)`) learn to call `.value()` or the args struct grows thin accessors `args.min_dt() -> f64`.

**Files**
- `crates/pid-ctl/src/cli/user_set.rs` — new (small, ~40 LOC).
- `crates/pid-ctl/src/cli/types.rs` — 8 field edits, remove 4 `explicit_*` fields.
- `crates/pid-ctl/src/cli/parse.rs` — ~10 line edits at lines 264, 277–279, 319–320, 324, 326, 337–340.
- `crates/pid-ctl/src/main.rs` — `apply_runtime_interval` now lives in `app::socket_dispatch` (moved in #1); body uses `set_if_default`.

**Reused utilities**
None new. Internal helper only.

**Test impact**
- Integration tests: **unchanged.**
- `src/tune/tests.rs` constructs `LoopArgs` directly (grep confirms) — the test builder will need updates. Expected: ~10 test files under `src/tune/` that construct `LoopArgs { min_dt: 0.01, ... }` become `LoopArgs { min_dt: UserSet::Default(0.01), ... }`. Add a `Default` impl for convenience.

---

### #3 — shared `tick()` driver (HIGH)

**Target end state**

A single `Ticker::step(&mut self, ctx: TickContext<'_>) -> TickStepResult` consumed by both `run_loop` and `tune::run`. Divergent concerns (stdout iteration JSON, TUI screen ownership, verbose stderr) are parameterized via a `TickObserver` trait rather than duplicated.

**New module**: `crates/pid-ctl/src/app/ticker.rs`

```rust
pub struct TickContext<'a> {
    pub scaled_pv: f64,
    pub dt: f64,
    pub session: &'a mut ControllerSession,
    pub cv_sink: &'a mut dyn CvSink,
    pub logger: &'a mut Logger,
    pub state_path: Option<&'a Path>,
    pub cv_fail_after: u32,
    pub safe_cv: Option<f64>,
    pub quiet: bool,
}

pub trait TickObserver {
    fn on_success(&mut self, outcome: &TickOutcome);
    fn on_cv_fail(&mut self, error: &TickError, consecutive: u32);
}

pub enum TickStepResult {
    Ok(TickOutcome),
    CvFailTransient { consecutive: u32 },
    CvFailExhausted(CliErrorLike),       // convertable to CliError at binary edge
}

pub fn step(
    ctx: TickContext<'_>,
    cv_fail_count: &mut u32,
    observer: &mut dyn TickObserver,
) -> TickStepResult;
```

Concrete observers:
- `LoopObserver` in `main.rs` — writes iteration JSON to stdout when `OutputFormat::Json`, verbose eprintln.
- `TuneObserver` in `tune/mod.rs` — updates `ui.last_record`, `ui.push_history`.

Both `run_loop_tick` and `tune_tick` become ~10-line glue: build context, build observer, call `app::ticker::step`.

The `on_dt_skipped` → `handle_dt_skip_state_write` path stays outside `ticker::step` because `tune::run` needs to redraw after a skip. Ticker only owns the happy path + CV-fail transitions.

**Files**
- `crates/pid-ctl/src/app/ticker.rs` — new, ~120 LOC.
- `crates/pid-ctl/src/main.rs` — delete `run_loop_tick`; `run_loop` shrinks ~30 LOC.
- `crates/pid-ctl/src/tune/mod.rs` — delete `tune_tick` (lines 363–412); `run` shrinks ~50 LOC.

**Reused utilities**
- `ControllerSession::process_pv`, `TickOutcome`, `TickError` already correct.
- `write_safe_cv` (moved to `app::loop_runtime` in #1).
- `json_events::emit_cv_write_failed`, `emit_d_term_skipped`.

**Test impact**
- Integration tests: **unchanged.** The wire behavior (stdout/stderr/state/log bytes) must be identical. Diff-check `req_loop.rs`, `req_fail_after.rs`, `req_cv_write_policy.rs`, `req_reliability.rs`, `req_tune.rs`, `req_tune_pty.rs`.
- **NEW unit tests** in `crates/pid-ctl/src/app/ticker.rs` (addresses #10). Use fake `CvSink`, in-memory `Logger`, and a recording `TickObserver`.

---

### #4 — `socket_send_and_print` direct `ok` accessor (LOW)

**Target end state**

Add `pub const fn ok(&self) -> bool` on `Response` (in `socket.rs`). Replace the serde_json round-trip with `response.ok()`. Exit code path unchanged.

**Files**
- `crates/pid-ctl/src/socket.rs` — add one method (~15 LOC match arms).
- `crates/pid-ctl/src/main.rs` — 4 lines at 1132–1135 become one.

**Reused utilities** None.

**Test impact** Integration tests: **unchanged.** Exit-code parity checked by `req_socket.rs` which asserts on `cmd.assert().failure()/success()`.

---

### #5 — collapse 4 socket wrapper fns (LOW)

**Target end state**

Single helper `run_socket_cmd(raw: &SocketOnlyArgs, req: pid_ctl::socket::Request, label: &str)`. `Hold/Resume/Reset/Save` arms in the main dispatcher call it inline — the dispatcher gains 4 one-liners, deletes 4 three-line fns.

Alternative (equivalent LOC but clearer): keep the helpers but reduce to 2-line bodies and doc-group them.

**Files**
- `crates/pid-ctl/src/main.rs` — lines 1146–1168 replaced.

**Reused utilities** `socket_send_and_print`, `get_socket_path` (already present).

**Test impact** Integration tests: **unchanged.**

---

### #6 — centralize `#[cfg(unix)]` boundary (MED)

**Target end state**

51 `cfg(unix)` occurrences reduced to ~10 by introducing one library module that the binary unconditionally consumes:

- `pid_ctl::app::socket_dispatch` (from #1) is entirely gated `#[cfg(unix)]` at module level so callers need only one `#[cfg(unix)]` at the `mod` declaration (in `app/mod.rs`).
- Wire `mod.rs` pattern: `#[cfg(unix)] pub mod socket_dispatch;` — callers say `pid_ctl::app::socket_dispatch::handle_socket_request(...)` still inside a `#[cfg(unix)]` block in `main.rs`, but socket-related helpers don't need individual guards.
- For `SubCommand` variants Set/Hold/Resume/Reset/Save: keep variant-level gates (clap requires them) but extract `run_socket_dispatch(cmd: &SubCommand)` that matches only those variants inside a single `#[cfg(unix)]` block in `main.rs` — reduces the 5 scattered guards at lines 85–94 to one.
- `cli/types.rs` `LoopArgs::socket_path` / `socket_mode`: keep `#[cfg(unix)]` (struct field) — unavoidable.
- `cli/raw.rs`: same.

Net: ~40 → ~12 `cfg(unix)` annotations. No new trait/abstraction.

**Files**
- `crates/pid-ctl/src/main.rs` — consolidate the 5 Set/Hold/Resume/Reset/Save arms under one guard (lines 85–94).
- `crates/pid-ctl/src/app/mod.rs` — add `#[cfg(unix)] pub mod socket_dispatch;`.
- No deletions, no new files beyond what #1 already adds.

**Reused utilities** None.

**Test impact** Integration tests: **unchanged.** Confirm with cross-compile check `cargo check --target x86_64-pc-windows-gnu` if available, else only Unix.

---

### #7 — process-global `AtomicBool` → Logger field (LOW, free after #8)

**Target end state**

`Logger` (from #8) owns a `suppress_stderr: bool` field. `tune::run` constructs `Logger::new().suppressed()` instead of calling a global mutator. No more process-global state; safe to thread any future concurrency through.

**Files**
- `crates/pid-ctl/src/json_events.rs` — delete lines 13, 19, 32–43 (static + guard). `emit_line` takes `&mut Logger` and consults `logger.suppress_stderr`.
- `crates/pid-ctl/src/tune/mod.rs` — delete line 46 (the `_suppress_structured_json_stderr` guard binding).

**Reused utilities** Everything in `json_events` stays; only the plumbing changes.

**Test impact**
- `json_events::tests::suppress_guard_still_appends_to_log` (line 261) — **will need update**: it constructs a Logger explicitly instead of calling the global guard. Same assertion, different setup.
- Integration tests: **unchanged.** Behavior identical.

---

### #8 — `Option<&mut File>` → `Logger` trait (HIGH)

**Target end state**

Introduce `pub struct Logger` (concrete — not a trait object) in `crates/pid-ctl/src/app/logger.rs` or alongside `json_events`:

```rust
pub struct Logger {
    file: Option<std::fs::File>,
    suppress_stderr: bool,
}

impl Logger {
    pub fn open(path: Option<&Path>) -> io::Result<Self>;
    pub fn none() -> Self;
    pub fn suppressed(self) -> Self;
    pub fn write_event<T: Serialize>(&mut self, event: &T);
    pub fn write_iteration_line<T: Serialize>(&mut self, rec: &T);  // file-only, skips stderr
    pub fn file_mut(&mut self) -> Option<&mut File>;    // escape hatch for tune export
}
```

All 22 call sites that currently take `&mut Option<File>` take `&mut Logger` instead. `json_events::emit_*` functions become methods on `Logger` or take `&mut Logger`:

```rust
impl Logger {
    pub fn emit_dt_skipped(&mut self, raw_dt: f64, min_dt: f64, max_dt: f64);
    // ... one per event_struct macro entry
}
```

The `event_struct!` macro is updated once to generate `impl Logger` methods instead of free `emit_*` functions.

**Why concrete struct not trait object**: no runtime polymorphism needed (there is exactly one implementation). A concrete struct avoids vtable overhead, inlines better, and keeps the API monomorphic. See Alternative (c) below.

**Files**
- `crates/pid-ctl/src/json_events.rs` — macro adapts; `emit_line` takes `&mut Logger`.
- `crates/pid-ctl/src/app/logger.rs` (or fold into `json_events.rs`) — new.
- `crates/pid-ctl/src/main.rs` — every `log_file: &mut Option<std::fs::File>` becomes `logger: &mut Logger`.
- `crates/pid-ctl/src/tune/mod.rs` — same.
- `crates/pid-ctl/src/tune/input.rs`, `tune/export.rs` — same.
- `crates/pid-ctl/src/app/loop_runtime.rs` (from #1) — same.

**Reused utilities**
- `open_log_optional` → `Logger::open`.
- `writeln!` for iteration records → `Logger::write_iteration_line`.

**Test impact**
- Integration tests: **unchanged** (wire output byte-identical).
- `json_events::tests` — signatures adapt (4 tests).
- `src/tune/tests.rs` — wherever tests pass `&mut Option<File>`, switch to `Logger::none()`.

---

### #9 — split `ControllerSession` (MED)

**Target end state**

`ControllerSession` shrinks to PID orchestration; persistence extracted into a `SnapshotPersister` that holds `StateStore + StateLock + flush_interval + last_flush + state_fail_count + state_fail_after`.

```rust
// app/snapshot_persister.rs — new
pub struct SnapshotPersister {
    store: Option<StateStore>,
    _lock: Option<StateLock>,
    flush_interval: Option<Duration>,
    last_flush: Option<Instant>,
    fail_count: u32,
    fail_after: u32,
}
impl SnapshotPersister {
    pub fn persist(&mut self, snapshot: &StateSnapshot) -> Result<(), StateStoreError>;
    pub fn force_flush(&mut self, snapshot: &StateSnapshot) -> Option<StateStoreError>;
    pub fn set_flush_interval(&mut self, d: Option<Duration>);
    pub fn fail_count(&self) -> u32;
    pub fn fail_escalated(&self) -> bool;
    pub fn has_store(&self) -> bool;
}

// app/mod.rs
pub struct ControllerSession {
    controller: PidController,
    snapshot: StateSnapshot,
    persister: SnapshotPersister,
}
```

Public API on `ControllerSession` stays byte-identical (delegates). This keeps all existing callers unchanged. Unit tests that specifically want to exercise persistence/escalation logic can construct `SnapshotPersister` in isolation.

See Alternative (b) below for why I recommend a 2-way split, not 3.

**Files**
- `crates/pid-ctl/src/app/snapshot_persister.rs` — new, ~90 LOC.
- `crates/pid-ctl/src/app/mod.rs` — `ControllerSession` delegates; field count drops from 7 to 3 directly-owned.

**Reused utilities** `StateStore`, `StateLock`, `StateStoreError` — already library.

**Test impact**
- Integration tests: **unchanged.**
- `app::tests` in `app/mod.rs` (3 tests at 559–637) — **unchanged** (they hit the public API).
- **NEW** unit tests for `SnapshotPersister` (coalescing, escalation, force_flush) land here.

---

### #10 — unit tests for tick logic (MED, free after #3)

**Target end state**

New test modules `#[cfg(test)] mod tests` in `app/ticker.rs` with:
- Happy path: process_pv OK → observer notified → state_write_failed propagated.
- CV fail transient: consecutive counter advances, no escalation.
- CV fail at threshold: `safe_cv` written once, CliError-shaped exhaustion result returned.
- D-term skip surfaces via observer.
- State-write-failed during `process_pv` surfaces.

These tests need a `FakeCvSink` (can fail on demand) and a recording `TickObserver`. ~120 LOC of tests.

**Files**
- `crates/pid-ctl/src/app/ticker.rs` — `#[cfg(test)] mod tests`.

**Test impact**
- Integration tests: **unchanged.**
- New `cargo test -p pid-ctl --lib` surface.

---

## Alternatives Evaluation

### (a) Move `tune` to the library vs. keep in binary — **RECOMMEND: keep in binary**

After #1, `tune` no longer reaches back into `main.rs`; its only cross-boundary call is `json_events`, `app::loop_runtime`, `app::socket_dispatch` — all library. The remaining binding reasons to keep tune in the binary: (1) it owns stdout/stderr terminal control, which is a binary concern; (2) it uses `crate::CliError` which is the binary's error type; (3) moving it imposes a `tui` feature on the library, which forces downstream consumers of `pid-ctl` as a library (if any) to pull ratatui/crossterm from the dep tree or flip a feature flag. Keeping it in the binary preserves library purity for free.

**Tradeoff**: library migration would let tune have its own integration test binary that drives the tick loop without subprocess spawning; keeping it in the binary preserves dependency hygiene at the cost of not unlocking that test mode. Worth it — the existing PTY tests (`req_tune_pty.rs`) already cover tune.

### (b) Split `ControllerSession` into 2 or 3 structs vs. keep unified — **RECOMMEND: 2-way split (session + persister)**

Three concerns (PID math, persistence, failure escalation) but PID math is already in `pid_ctl_core::PidController`; `ControllerSession` is only combining "wrap controller" and "persist its state." Splitting escalation into a third struct would add a third hop (controller → persister → escalator) with no consumer that needs escalation without persistence. The 2-way split (controller orchestration vs. snapshot persistence+escalation) matches the actual coupling: fail_count and fail_after *belong* with the persister because they track *persistence* failures specifically.

**Tradeoff**: 3-way split would be purer separation but adds a layer with no consumer. The 2-way split shrinks `ControllerSession` enough to make the unit tests on `SnapshotPersister` tractable.

### (c) Replace `Option<&mut File>` with trait object vs. concrete `Logger` struct vs. leave it — **RECOMMEND: concrete `Logger` struct**

There is exactly one `Logger` implementation needed (file + stderr, with suppress flag). `dyn Logger` adds a vtable and forces every emit site to import a trait. A concrete struct is simpler, inlines better, and leaves room to add a trait in the future if (e.g.) a syslog or tracing sink becomes necessary. The 22 call sites threading `&mut Option<File>` collapse to `&mut Logger` either way.

**Tradeoff**: Trait object gives test-mockability (a no-op logger for unit tests), but `Logger::none()` + `Logger::with_memory_buffer()` (for tests) covers that with no trait.

### (d) Extract shared `tick` vs. leave duplication — **RECOMMEND: extract (issue #3 as designed)**

The duplication between `run_loop_tick` and `tune_tick` is 70% textual and semantic. The differences (stdout JSON vs. no stdout, verbose eprintln vs. TUI redraw, history push) are observable side effects — perfect fit for an observer trait. Leaving duplication invites the two copies to drift: a bug fix in one is easy to miss in the other, and `req_tune.rs` / `req_loop.rs` have overlapping but not identical coverage. This is the single highest-leverage extraction.

**Tradeoff**: The observer trait adds one indirection. In exchange, #10 (unit tests for tick logic) becomes practical — the subprocess-only test suite gains a faster, more targeted inner-loop test tier.

### (e) `UserSet<T>` vs. keep twins — **RECOMMEND: `UserSet<T>`**

The twin booleans are a maintenance trap: the invariant "explicit_foo is true iff the user set foo" is enforced at one place (parse.rs) but must be read at distant sites (apply_runtime_interval). `UserSet<T>` makes it type-enforced. Four fields × two values = 8 fewer struct fields, and `set_if_default` is self-documenting.

**Tradeoff**: `UserSet<Duration>` and `UserSet<Option<Duration>>` are slightly awkward at construction sites. Mitigate with a `Default` impl on `LoopArgs` for tests.

### (f) Replace AtomicBool with threaded state vs. leave it — **RECOMMEND: thread it (as part of #8)**

The `AtomicBool` is a process-global with RAII guard. Today it's safe because the binary is single-threaded; but it also forces `json_events::emit_*` to consult global state on every event even when no tune is running. Once #8 threads a `Logger` through, the suppress bit is just a field on the Logger — no more static, no more guard, no latent race. This is free once #8 is in flight, so do not leave it.

**Tradeoff**: None significant. Pure win after #8.

---

## CLI Stability Risks

Integration tests at `crates/pid-ctl/tests/req/*.rs` assert on:
1. **Exit codes**: 0, 1, 2, 3, 4, 5 have distinct meanings. `CliError::new(4, ...)` for state persistence in `run_once` (main.rs:154) and `CliError::new(5, ...)` for CV write must be preserved. #1 and #3 must return `CliError` at the binary boundary identically.
2. **Stdout bytes**: `print_iteration_json` writes exact serde_json bytes with trailing `\n`. Any refactor changing serializer config or buffer handling breaks `req_stdout_contract.rs` and `req_once_pipe.rs`.
3. **Stderr bytes**: Human-readable lines like `"dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — skipping tick"` (main.rs:534) are asserted by string predicates (`contains(...)`). Format strings must be preserved byte-for-byte.
4. **Structured NDJSON on stderr**: `json_events::emit_*` produces stable JSON events — the event field names (`"event":"dt_skipped"` etc.) and payload schema are tested in `req_reliability.rs`.
5. **Socket wire protocol**: `Request`/`Response` JSON shape is asserted in `req_socket.rs` (raw `UnixStream::write_all("...")`). #4 (`Response::ok()` method) is additive and safe; do not rename any variant or field.
6. **TUI exit markers**: `req_tune.rs` and `req_tune_pty.rs` assert on `export_line_stderr` output.

**Specific risks per phase**:
- Phase 1 (#1): If `build_cv_sink`/`build_pv_source` move and subtly change their closure over `LoopArgs`, defaults could shift. Mitigation: move them as-is, keep signatures byte-identical.
- Phase 2 (#8): `Logger` must serialize events via the same `serde_json::to_string` path. If the macro expands differently (e.g., reordering fields), `req_reliability.rs` JSON equality breaks. Mitigation: keep `event_struct!` field order identical.
- Phase 3 (#3): `run_loop_tick` writes iteration JSON via `serde_json::to_writer(&mut handle, record)` then `writeln!`. `tune_tick` writes via `print_iteration_json` which uses the same path. Consolidate onto one implementation, not a third.
- Phase 5 (#2): `UserSet` must not change the serialized form of anything (it's CLI-side only, not persisted).

**Risks to flag upfront to the user**:
- `CliError` public surface: `exit_code` and `message` are `pub(crate)` — confirm no test reaches into them (grep confirms: zero hits). Safe to refactor internally.
- `Response::ok` method addition (#4): verify no downstream consumer deserializes Response with `#[serde(deny_unknown_fields)]`. Grep confirms it doesn't — adding a method is not a schema change. Safe.

---

## Verification Strategy

Run between every phase:
```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings   # confirms no-tui path
cargo test --workspace
cargo test -p pid-ctl --test requirements                                     # the behavior contract
```

Per-phase focused runs:

**Phase 0 (#4, #5, #6)**:
- `cargo test -p pid-ctl --test requirements req_socket` — covers socket exit codes and wrapper commands.
- `cargo check --no-default-features` — confirms `cfg(unix)` consolidation still compiles on both TUI/non-TUI.

**Phase 1 (#1)**:
- Full `cargo test --workspace`.
- Manual: `cargo run -p pid-ctl -- loop --help` — confirm help text unchanged (clap output).

**Phase 2 (#8, #7)**:
- `cargo test -p pid-ctl --test requirements req_reliability` — strictest on event JSON.
- `cargo test -p pid-ctl --lib json_events::tests` — guard test signatures adapted.

**Phase 3 (#3, #10)**:
- `cargo test -p pid-ctl --test requirements req_loop req_fail_after req_cv_write_policy req_tune req_tune_pty` — every file that exercises a tick.
- `cargo test -p pid-ctl --lib app::ticker::tests` — new unit suite.

**Phase 4 (#9)**:
- `cargo test -p pid-ctl --lib app::tests app::snapshot_persister::tests`.
- `cargo test -p pid-ctl --test requirements req_state_write_interval req_state_schema req_state_commands req_locking`.

**Phase 5 (#2)**:
- `cargo test -p pid-ctl --test requirements req_flag_precedence req_flag_validation`.
- `cargo test -p pid-ctl --lib tune::tests`.

**CI gate for the full refactor**: re-run `cargo test -p pid-ctl --test requirements -- --include-ignored` to cover the `loop_basic_iterations` timing test and any other ignored ones.

---

## Open Risks & Things You Might Be Missing

1. **`ControllerSession` conflates three concerns — is there a hidden reason?**
   I checked. All three fields (`state_fail_count`, `state_fail_after`, `last_flush`) are *only* read from within `persist_snapshot` / `force_flush` / `state_fail_escalated` / `state_fail_count`. They are not touched by `process_pv`'s PID path directly. **The conflation is accidental, not load-bearing.** The 2-way split is safe.

2. **Why does `tune` pull from `main.rs` rather than a shared module?**
   Historical accident, per the code layout. `tune` was added after the helpers were already in `main.rs`, and the `pub(crate)` escape hatch was the path of least resistance. There is no architectural reason — #1 unblocks this cleanly.

3. **Hidden coupling #1: `json_events` depends on `crate::app::{STATE_SCHEMA_VERSION, now_iso8601}`.**
   This is the library crate calling itself, so it's fine. Just be aware when you introduce `Logger` — keep `Logger` in `json_events` or right next to it, don't create a circular dep.

4. **Hidden coupling #2: `SUPPRESS_STRUCTURED_JSON_STDERR` is set by `tune::run` before any session is constructed.**
   If `Logger` is threaded through, `suppress_stderr` needs to be set when the Logger is *created*, not after. Check `ControllerSession::new` error path: when `tune::run` constructs a session that fails, does it emit via stderr with the wrong suppression state? Looking at tune/mod.rs:46–48: the guard is taken before `ControllerSession::new`. The refactor must preserve this: Logger must be constructed and suppressed *before* any `emit_*` call that might happen during session init. Trivial to preserve; call it out to whoever implements #8.

5. **`run_pipe` and `run_once` are not mentioned in the findings but are tick-shaped.**
   Both (`main.rs:108–166` and `168–219`) call `session.process_pv`, emit `d_term_skipped`, write iteration log lines. After #3 ships an `app::ticker::step`, both are candidates to migrate too — they'd shrink to 20 LOC each. Not in the current scope, but flag for a follow-up issue.

6. **`LoopArgs` session_config does *not* use the `explicit_*` booleans.**
   Good news: #2 (`UserSet<T>`) touches only CLI layer, not the library session API. No SessionConfig churn.

7. **Clap derive conflict with `UserSet<T>`.**
   `LoopArgs` is not a clap derive struct (that's `LoopRawArgs` in `cli/raw.rs`). `LoopArgs` is the *parsed* form. So `UserSet<T>` is safe — clap never sees it. Confirmed via `cli/raw.rs` read.

8. **`StateSnapshot` schema version vs. refactor.**
   `STATE_SCHEMA_VERSION` is asserted in `req_state_schema.rs`. Nothing in this plan changes the snapshot shape; splitting `ControllerSession` does not touch the `StateSnapshot` struct. Safe.

9. **`sleep_with_socket` and tune's inline socket-service loop are NOT identical.**
   `sleep_with_socket` (main.rs:844) services socket in 50ms chunks while sleeping; tune polls crossterm events with 50ms `event::poll` and services socket between ticks. Do not attempt to unify these two — they have genuinely different scheduling contracts. Only `handle_socket_request` (the *dispatcher*) should be shared; the *service loop* stays separate.

10. **`--tune` and `--quiet` are mutually exclusive** (tune/mod.rs:205 comment). After #2 (`UserSet`) and #8 (`Logger`), re-verify this invariant is still enforced at parse time (it's a clap-level constraint, not a runtime check).

11. **Potential scope risk on #1.** Moving `CvSinkConfig`/`PvSourceConfig` to the library (so `build_pv_source`/`build_cv_sink` can live there) is a breaking library API addition. Acceptable — `pid-ctl` the library is not published on crates.io (per workspace layout), so no external consumers. Confirm with the user before landing, but low risk.

---

### Critical Files for Implementation

- `/home/user/pid-ctl/crates/pid-ctl/src/main.rs`
- `/home/user/pid-ctl/crates/pid-ctl/src/tune/mod.rs`
- `/home/user/pid-ctl/crates/pid-ctl/src/app/mod.rs`
- `/home/user/pid-ctl/crates/pid-ctl/src/json_events.rs`
- `/home/user/pid-ctl/crates/pid-ctl/src/cli/types.rs`
