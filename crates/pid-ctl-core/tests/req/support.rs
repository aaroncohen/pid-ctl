use pid_ctl_core::{PidConfig, PidController, PidRuntimeState, StepInput, StepResult};

pub(crate) fn controller(config: PidConfig) -> PidController {
    PidController::new(config).expect("valid test config")
}

pub(crate) fn step(
    controller: &mut PidController,
    pv: f64,
    dt: f64,
    prev_applied_cv: f64,
) -> StepResult {
    controller.step(StepInput {
        pv,
        dt,
        prev_applied_cv,
    })
}

pub(crate) fn restored_controller(config: PidConfig, state: &PidRuntimeState) -> PidController {
    let mut controller = controller(config);
    controller.restore_state(state);
    controller
}

pub(crate) fn assert_close(actual: f64, expected: f64) {
    let delta = (actual - expected).abs();
    assert!(
        delta < 1e-9,
        "expected {expected}, got {actual} (|delta| = {delta})"
    );
}
