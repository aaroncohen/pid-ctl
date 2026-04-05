# Test patterns — pid-ctl

## Goals

- Requirements live in `pid-ctl_plan.md`; **tests name behaviors**, not internal helpers.
- **Social / behavioral tests:** Prefer **API interaction points** and **output-for-input correctness** — what callers observe when they supply inputs (CLI flags, `StepInput`, files, socket payloads). Assertions should survive refactors that preserve behavior: avoid coupling tests to private fields, internal function call order, or “how” the code is structured unless the plan documents that as part of the contract. Table and property tests still target **observable** outcomes (e.g. `StepResult.cv`, exit codes, NDJSON fields), not implementation trivia.
- **Core (`pid-ctl-core`):** tests inject `dt` as data — no wall-clock coupling, no `sleep` for correctness. See plan *Architecture & Code Structure* → principle **Core tests are not tied to the wall clock**.
- `pid-ctl-core`: table-driven and **proptest** checks live in `src/` next to the module under test; integration harness is `tests/requirements.rs`.
- `pid-ctl`: orchestration, state JSON, and CLI contracts use `tests/requirements.rs` modules; use **`assert_cmd`** + **`tempfile`** for subprocess/FS cases once the binary exists.

## Layout

| Location | Role |
|----------|------|
| `crates/pid-ctl-core/tests/requirements.rs` | Integration harness + smoke test |
| `crates/pid-ctl-core/tests/req/*.rs` | Core behavior vs plan (error convention, controller form, D-on-measurement, anti-windup, setpoint ramp, deadband, filter, output/slew, step I/O, dt, cross-cutting) — included via `#[path = ...]` (**not** top-level `tests/*.rs`, or Cargo builds one binary per file) |
| `crates/pid-ctl/tests/requirements.rs` | App/CLI harness + smoke test |
| `crates/pid-ctl/tests/req/*.rs` | Reliability, schema, CLI |

## Ignored tests

- `#[ignore = "..."]` marks behavior **not implemented yet**.
- Run: `cargo test -p pid-ctl-core --test requirements -- --ignored` (expect failures until `todo!` is replaced with real assertions).
- Remove `#[ignore]` when the test passes on CI.

## Dependencies (workspace-pinned)

Versions are centralized in the root `Cargo.toml` `[workspace.dependencies]`; bump there and inherit with `dep.workspace = true` in crates.

## Learnings

- **Cargo integration tests:** Only `crates/<pkg>/tests/*.rs` (one level) become test binaries. Putting every requirement file at `tests/req_foo.rs` creates **one binary per file** and duplicates runs. Keep a single `tests/requirements.rs` and pull modules from `tests/req/*.rs` via `#[path = "req/....rs"]` (see both crates).
- **`loop` deadline scheduling:** Pure `Instant` math for `next_deadline_after_tick` lives in `pid_ctl::schedule` with unit tests in `src/schedule.rs` — no wall-clock sleeps or subprocesses needed to prove deadline-based vs `now + interval` drift behavior.
- **Clippy `-D warnings`:** Avoid `assert!(true, "...")` smoke tests — use an empty `#[test] fn harness_smoke() {}` or a non-tautological assert.
- **`pid-ctl loop` subprocess tests:** For PV read failure paths, `--pv-cmd false` (non-zero exit) forces `read_pv` errors deterministically; pair with `Command::timeout` and assert on `--cv-file` contents. First tick waits one `--interval` before the first PV read, so timeouts should exceed that interval.
- **Subprocess + temp files:** `tempfile::tempdir()` is the default (child writes to the same OS temp as normal `pid-ctl` runs). Some sandboxed test runners only allow writes under a fixed workspace root; if subprocess FS assertions fail with missing files, re-run tests outside that restriction.
