//! Requirement-driven tests for **`pid-ctl`** (orchestration, CLI, persistence). See `pid-ctl_plan.md`.
//!
//! # Running
//! - `cargo test -p pid-ctl --test requirements`
//! - Pending: `cargo test -p pid-ctl --test requirements -- --ignored`
//!
//! Module files live under `tests/req/` so Cargo does not treat each file as a separate integration-test binary.
//!
//! Tests should verify **CLI / persistence / IPC contracts** (exit codes, streams, files, JSON) — behavioral “social” tests, not private app internals.

#[path = "req/helpers.rs"]
mod helpers;
#[path = "req/req_cv_sink.rs"]
mod req_cv_sink;
#[path = "req/req_cv_write_policy.rs"]
mod req_cv_write_policy;
#[path = "req/req_dry_run.rs"]
mod req_dry_run;
#[path = "req/req_fail_after.rs"]
mod req_fail_after;
#[path = "req/req_flag_precedence.rs"]
mod req_flag_precedence;
#[path = "req/req_flag_validation.rs"]
mod req_flag_validation;
#[path = "req/req_locking.rs"]
mod req_locking;
#[path = "req/req_loop.rs"]
mod req_loop;
#[path = "req/req_once_pipe.rs"]
mod req_once_pipe;
#[path = "req/req_pv_source.rs"]
mod req_pv_source;
#[path = "req/req_pv_stdin_verify_cv.rs"]
mod req_pv_stdin_verify_cv;
#[path = "req/req_reliability.rs"]
mod req_reliability;
#[path = "req/req_replay.rs"]
mod req_replay;
#[path = "req/req_sim_loop.rs"]
mod req_sim_loop;
#[path = "req/req_socket.rs"]
mod req_socket;
#[path = "req/req_state_commands.rs"]
mod req_state_commands;
#[path = "req/req_state_schema.rs"]
mod req_state_schema;
#[path = "req/req_state_write_interval.rs"]
mod req_state_write_interval;
#[path = "req/req_stdout_contract.rs"]
mod req_stdout_contract;
#[path = "req/req_tune.rs"]
mod req_tune;
#[path = "req/req_tune_pty.rs"]
mod req_tune_pty;

#[test]
fn harness_smoke() {
    // Keeps a fast non-ignored test in the suite; expand with CLI/state checks as behavior lands.
}
