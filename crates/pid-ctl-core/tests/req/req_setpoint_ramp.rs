//! Setpoint ramping — `pid-ctl_plan.md` (Setpoint ramping).

use crate::support::{assert_close, controller, restored_controller, step};
use pid_ctl_core::{PidConfig, PidRuntimeState};

#[test]
fn effective_sp_moves_toward_target_at_ramp_rate_times_dt() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(2.0),
        ..PidConfig::default()
    });

    step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(10.0);
    let result = step(&mut controller, 0.0, 1.5, 0.0);

    assert_close(result.effective_sp, 3.0);
}

#[test]
fn when_ramp_disabled_effective_equals_setpoint_field() {
    let mut controller = controller(PidConfig {
        setpoint: 7.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.effective_sp, 7.0);
}

#[test]
fn first_tick_no_ramp_when_no_prior_state() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        setpoint_ramp: Some(1.0),
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.effective_sp, 10.0);
}

#[test]
fn mid_ramp_target_change_updates_target_but_effective_continues_from_current() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(2.0),
        ..PidConfig::default()
    });

    let _ = step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(10.0);
    let first = step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(-10.0);
    let second = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(first.effective_sp, 2.0);
    assert_close(second.effective_sp, 0.0);
}

#[test]
fn when_effective_reaches_target_ramp_has_no_further_effect() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(5.0),
        ..PidConfig::default()
    });

    step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(4.0);
    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.effective_sp, 4.0);
}

#[test]
fn step_result_includes_effective_sp_for_diagnostics_when_ramping() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(1.0),
        ..PidConfig::default()
    });

    step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(10.0);
    let result = step(&mut controller, 0.0, 2.0, 0.0);

    assert_close(result.effective_sp, 2.0);
}

#[test]
fn preloaded_effective_sp_resumes_ramp_from_that_value() {
    let mut controller = restored_controller(
        PidConfig {
            setpoint: 10.0,
            setpoint_ramp: Some(2.0),
            ..PidConfig::default()
        },
        &PidRuntimeState {
            effective_sp: Some(3.0),
            ..PidRuntimeState::default()
        },
    );

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.effective_sp, 5.0);
}
