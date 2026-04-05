//! Error convention — `pid-ctl_plan.md` (PID Implementation → Error convention, Setpoint ramping when ramp active).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn error_is_setpoint_minus_pv_when_no_ramp() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 7.5, 1.0, 0.0);

    assert_close(result.p_term, 2.5);
}

#[test]
fn positive_error_implies_pv_below_target() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 1.0, 0.0);

    assert!(result.cv > 0.0);
}

#[test]
fn negative_error_implies_pv_above_target() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 12.0, 1.0, 0.0);

    assert!(result.cv < 0.0);
}

#[test]
fn error_uses_effective_sp_when_setpoint_ramp_active() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(2.0),
        kp: 1.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 0.0, 1.0, 0.0);
    assert_close(first.effective_sp, 0.0);

    controller.set_setpoint(10.0);

    let second = step(&mut controller, 0.0, 1.0, 0.0);
    assert_close(second.effective_sp, 2.0);
    assert_close(second.p_term, 2.0);
}
