# Test patterns — pid-ctl

## Goals

- Requirements live in `pid-ctl_plan.md`; **tests name behaviors**, not internal helpers.
- **Social / behavioral tests:** Prefer **API interaction points** and **output-for-input correctness** — what callers observe when they supply inputs (CLI flags, `StepInput`, files, socket payloads). Assertions should survive refactors that preserve behavior: avoid coupling tests to private fields, internal function call order, or “how” the code is structured unless the plan documents that as part of the contract. Table and property tests still target **observable** outcomes (e.g. `StepResult.cv`, exit codes, NDJSON fields), not implementation trivia.
- **Core (`pid-ctl-core`):** tests inject `dt` as data — no wall-clock coupling, no `sleep` for correctness. See plan *Architecture & Code Structure* → principle **Core tests are not tied to the wall clock**.
- **`pid-ctl-sim` plant math:** Same idea — dynamics are explicit `dt` in `apply_cv`; no `Instant` inside the model. CLI integration tests exercise `init` / `print-pv` / `apply-cv` via `assert_cmd`.
- `pid-ctl-core`: table-driven and **proptest** checks live in `src/` next to the module under test; integration harness is `tests/requirements.rs`.
- `pid-ctl`: orchestration, state JSON, and CLI contracts use `tests/requirements.rs` modules; use **`assert_cmd`** + **`tempfile`** for subprocess/FS cases once the binary exists.

## Layout

| Location | Role |
|----------|------|
| `crates/pid-ctl-core/tests/requirements.rs` | Integration harness + smoke test |
| `crates/pid-ctl-core/tests/req/*.rs` | Core behavior vs plan (error convention, controller form, D-on-measurement, anti-windup, setpoint ramp, deadband, filter, output/slew, step I/O, dt, cross-cutting) — included via `#[path = ...]` (**not** top-level `tests/*.rs`, or Cargo builds one binary per file) |
| `crates/pid-ctl/tests/requirements.rs` | App/CLI harness + smoke test |
| `crates/pid-ctl/tests/req/*.rs` | Reliability, schema, CLI |
| `crates/pid-ctl-sim` | Library + `pid-ctl-sim` binary; integration tests in `tests/` (e.g. `cli.rs`) |

## Ignored tests

- `#[ignore = "..."]` marks behavior **not implemented yet**.
- Run: `cargo test -p pid-ctl-core --test requirements -- --ignored` (expect failures until `todo!` is replaced with real assertions).
- Remove `#[ignore]` when the test passes on CI.

## Dependencies (workspace-pinned)

Versions are centralized in the root `Cargo.toml` `[workspace.dependencies]`; bump there and inherit with `dep.workspace = true` in crates.

## Learnings

- **Iteration JSON / NDJSON `ts`:** Requirement tests assert the stable `ts` field via `tests/req/helpers.rs` (`assert_json_ts_iso8601_utc`) so stdout and `--log` lines stay aligned with `app::now_iso8601()` (ISO 8601 UTC, second precision) without pulling in extra date crates.
- **Cargo integration tests:** Only `crates/<pkg>/tests/*.rs` (one level) become test binaries. Putting every requirement file at `tests/req_foo.rs` creates **one binary per file** and duplicates runs. Keep a single `tests/requirements.rs` and pull modules from `tests/req/*.rs` via `#[path = "req/....rs"]` (see both crates).
- **`loop` deadline scheduling:** Pure `Instant` math for `next_deadline_after_tick` lives in `pid_ctl::schedule` with unit tests in `src/schedule.rs` — no wall-clock sleeps or subprocesses needed to prove deadline-based vs `now + interval` drift behavior.
- **Clippy `-D warnings`:** Avoid `assert!(true, "...")` smoke tests — use an empty `#[test] fn harness_smoke() {}` or a non-tautological assert.
- **`pid-ctl loop` subprocess tests:** For PV read failure paths, `--pv-cmd false` (non-zero exit) forces `read_pv` errors deterministically; pair with `Command::timeout` and assert on `--cv-file` contents. First tick waits one `--interval` before the first PV read, so timeouts should exceed that interval.
- **Subprocess + temp files:** `tempfile::tempdir()` is the default (child writes to the same OS temp as normal `pid-ctl` runs). Some sandboxed test runners only allow writes under a fixed workspace root; if subprocess FS assertions fail with missing files, re-run tests outside that restriction.
- **`pid-ctl` adapter wall-clock checks:** Bounded-wait behavior for `--pv-cmd` / `CmdPvSource` may use a short timeout vs `sleep` on **Unix** (`#[cfg(unix)]`) so CI stays fast; assert `ErrorKind::TimedOut` and elapsed well under the sleep duration, not an exact millisecond bound.
- **`loop` dt skip + state:** Use an enormous `--min-dt` (e.g. `1e9`) so every tick hits the skip path before PV read; seed `--state` with a known `iter` and assert it is unchanged while `updated_at` is populated after a short `Command::timeout`. Assert stderr mentions the skip (e.g. `skipping tick` / `min_dt`) without coupling to exact NDJSON event payloads.
- **Post-`dt_skip` D term:** Prefer a **unit test** on `ControllerSession` (`process_pv` → `on_dt_skipped` → `process_pv`) with a noop `CvSink` and `kd > 0` so the second tick has non-zero `d` and the third has `d == 0` — no subprocess or wall clock.
- **Pipe `dt`:** `pipe` uses the configured fixed `--dt` (default `1.0`) per line, not wall-clock spacing between stdin lines (buffered stdin can make inter-line gaps &lt; `--min-dt`). Subprocess tests assume fixed-dt semantics. This differs from `pid-ctl_plan.md` §`dt` handling (Instant between lines for `pipe`); `--dt-clamp` / `--min-dt` / `--max-dt` apply to **`loop`** measured `dt`, not to **`pipe`** until measured-dt `pipe` exists.
- **`loop` interval slip:** Emitted when measured `raw_dt` **strictly exceeds** `--interval` (seconds), matching Reliability §10 (`interval_slip` NDJSON includes `interval_ms` / `actual_ms`).
- **`--tune` CLI validation:** Subprocess tests assert **incompatible combinations before the TTY check** (`parse_loop` order: `--format json` → `--quiet` → `--pv-stdin` → non-TTY), so CI without a TTY still sees the specific plan error for json/quiet/stdin. A separate case asserts `--tune-history` parses and then hits the TTY error when the dashboard would start.
- **`once` / `pipe` + `--tune`:** `parse_common_args` rejects any `--tune*` flag for non-`loop` modes with the plan-exact strings (`--tune requires loop` / pipe transformer message). Covered in `req_tune.rs`.
- **Export line (`c` / quit):** Unit-test `build_export_line_values` in `tune.rs` for deduping `--setpoint`/`--kp`/…/`--interval` and stripping `--tune*`. No PTY required.
- **`loop --tune` PTY smoke (`portable-pty`):** Integration tests allocate a PTY, spawn `pid-ctl loop … --dry-run --tune`, drain the master reader in a background thread (avoid filling the pty buffer), send `q` or `kill -INT` after a short startup delay, then assert exit code 0 and that the combined PTY capture includes the stderr sentinel `Tuned gains only:`. `SIGINT` test is `#[cfg(unix)]` (`kill -INT`). Use a bounded wait loop on `Child::try_wait` instead of unbounded `wait`.
- **Sparkline `|` markers:** `spark_marker_row` is unit-tested against a synthetic serial window + `GainAnnotation` (column alignment).
- **PV trend label:** `pv_history_trend` (first vs last sample in the deque) is unit-tested for the “trending ▲” case.
- **`once --log`:** Successful ticks append one iteration NDJSON line (same shape as `loop`); `state_write_failed` also emits a structured `state_write_failed` event before exit 4 when `--state` is set.
- **Clippy `redundant_clone` vs two `..base` updates:** When a test builds two `PidConfig { .. }` values from the same `base` without `Copy`, exactly one `.clone()` is required. `clippy::redundant_clone` may still fire on that clone; suppress with a targeted `#[allow(clippy::redundant_clone)]` on the test and a one-line rationale (prefer this over `PidConfig: Copy`, which would force broad `clone_on_copy` cleanups across tests).
- **`pid-ctl` + `pid-ctl-sim` subprocess smoke:** `req_sim_loop.rs` resolves `target/debug/pid-ctl-sim` via `std::env::current_exe()` (strip `deps/` then `debug/`) so it matches the same `CARGO_TARGET_DIR` as the test binary; `CARGO_MANIFEST_DIR/../../target/debug` breaks under custom target dirs. Build the sim binary first (`cargo build -p pid-ctl-sim` or `cargo test --workspace`) so the artifact exists beside `pid-ctl`.
