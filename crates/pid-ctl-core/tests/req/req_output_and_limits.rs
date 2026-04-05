//! Output clamp and slew — `pid-ctl_plan.md` (Safety → `--out-min`/`--out-max`, `--ramp-rate`; Core API → `StepResult`).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn final_cv_clamped_to_out_min_out_max() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        out_min: -1.0,
        out_max: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.cv, 1.0);
}

#[test]
fn saturated_true_when_u_unclamped_outside_limits() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        out_min: -1.0,
        out_max: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert!(result.saturated);
}

#[test]
fn u_unclamped_pre_clamp_value_exposed_for_diagnostics() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        out_min: -1.0,
        out_max: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.u_unclamped, 20.0);
    assert_close(result.cv, 1.0);
}

#[test]
fn slew_rate_limits_per_tick_change_using_dt() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 10.0,
        slew_rate: Some(1.5),
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 2.0, 2.0);

    assert_close(result.cv, 5.0);
}

#[test]
fn slew_applies_after_or_alongside_clamp_per_plan_order() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 10.0,
        out_min: 0.0,
        out_max: 5.0,
        slew_rate: Some(1.0),
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 1.0);

    assert!(result.cv <= 5.0);
    assert_close(result.cv, 2.0);
}

#[test]
fn infinite_limits_mean_cv_equals_u_unclamped_and_not_saturated() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.cv, result.u_unclamped);
    assert!(!result.saturated);
}
