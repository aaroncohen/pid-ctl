//! Requirement-driven tests for **`pid-ctl-core`**. Sections refer to `pid-ctl_plan.md` at the repo root.
//!
//! # Running
//! - Default: `cargo test -p pid-ctl-core --test requirements` runs only non-ignored tests (smoke / already implemented behavior).
//! - Pending behaviors: `cargo test -p pid-ctl-core --test requirements -- --ignored`
//!
//! # Conventions
//! - Assert **public API / observable outputs** (e.g. `StepResult`, documented invariants) — output-for-input correctness, not internal structure.
//! - Keep each `#[ignore]` reason string actionable for implementers.
//!
//! Module files live under `tests/req/` so Cargo does not treat each file as a separate integration-test binary.

#[path = "req/req_anti_windup.rs"]
mod req_anti_windup;
#[path = "req/req_config_validation.rs"]
mod req_config_validation;
#[path = "req/req_controller_form.rs"]
mod req_controller_form;
#[path = "req/req_core_step.rs"]
mod req_core_step;
#[path = "req/req_d_on_measurement.rs"]
mod req_d_on_measurement;
#[path = "req/req_deadband.rs"]
mod req_deadband;
#[path = "req/req_dt.rs"]
mod req_dt;
#[path = "req/req_error_convention.rs"]
mod req_error_convention;
#[path = "req/req_filter.rs"]
mod req_filter;
#[path = "req/req_gains_runtime.rs"]
mod req_gains_runtime;
#[path = "req/req_known_answer_vectors.rs"]
mod req_known_answer_vectors;
#[path = "req/req_multi_step.rs"]
mod req_multi_step;
#[path = "req/req_output_and_limits.rs"]
mod req_output_and_limits;
#[path = "req/req_setpoint_ramp.rs"]
mod req_setpoint_ramp;
#[path = "req/req_step_input_output.rs"]
mod req_step_input_output;
#[path = "req/support.rs"]
mod support;

#[test]
fn harness_smoke() {
    // Keeps a fast non-ignored test in the suite; expand with real checks as behavior lands.
}
