//! Deadband — `pid-ctl_plan.md` (Safety & Anti-Windup → `--deadband`, Setpoint ramping cross-reference).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn when_error_within_deadband_p_and_i_use_zero_effective_error() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ki: 1.0,
        deadband: 0.5,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 9.8, 1.0, 0.0);

    assert_close(result.p_term, 0.0);
    assert_close(result.i_term, 0.0);
}

#[test]
fn deadband_applies_to_error_from_effective_sp() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(1.0),
        deadband: 0.25,
        kp: 2.0,
        ..PidConfig::default()
    });

    let _ = step(&mut controller, 0.0, 1.0, 0.0);
    controller.set_setpoint(2.0);
    let result = step(&mut controller, 0.5, 0.5, 0.0);

    assert_close(result.effective_sp, 0.5);
    assert_close(result.p_term, 0.0);
}

#[test]
fn d_term_remains_measurement_based_under_deadband() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        deadband: 5.0,
        kd: 2.0,
        ..PidConfig::default()
    });

    let _ = step(&mut controller, 10.0, 1.0, 0.0);
    let result = step(&mut controller, 11.0, 1.0, 0.0);

    assert_close(result.d_term, -2.0);
    assert_eq!(controller.last_pv(), Some(11.0));
}

#[test]
fn small_error_suppresses_p_i_actuation_without_stale_state_hazards() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        deadband: 5.0,
        kp: 2.0,
        ki: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 11.0, 1.0, 0.0);

    assert_close(result.p_term, 0.0);
    assert_close(result.i_term, 0.0);
    assert_close(result.effective_sp, 10.0);
    assert_eq!(controller.last_pv(), Some(11.0));
    assert_eq!(controller.last_error(), Some(-1.0));
}

#[test]
fn integral_does_not_accumulate_when_effective_error_is_zero_due_to_deadband() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        deadband: 0.5,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 9.8, 1.0, 0.0);

    assert_close(result.i_acc, 0.0);
}
