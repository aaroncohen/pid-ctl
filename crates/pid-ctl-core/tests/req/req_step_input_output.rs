//! Public step API contract — `pid-ctl_plan.md` (Core API, Verification strategy → behavioral tests).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn step_accepts_scaled_pv_dt_and_prev_applied_cv() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 7.0, 0.25, 3.0);

    assert_close(result.cv, 3.0);
}

#[test]
fn step_returns_cv_and_unclamped_and_terms_and_i_acc() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ki: 1.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 0.5, 0.0);

    assert_close(result.cv, 5.0);
    assert_close(result.u_unclamped, 5.0);
    assert_close(result.p_term, 4.0);
    assert_close(result.i_term, 1.0);
    assert_close(result.d_term, 0.0);
    assert_close(result.i_acc, 1.0);
    assert_close(result.effective_sp, 10.0);
    assert!(!result.saturated);
}

#[test]
fn reset_integral_clears_accumulator_for_subsequent_steps() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 2.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 8.0, 1.0, 0.0);
    assert_close(first.i_acc, 2.0);
    assert_close(first.cv, 4.0);

    controller.reset_integral();

    let second = step(&mut controller, 8.0, 1.0, first.cv);
    assert_close(second.i_acc, 2.0);
    assert_close(second.cv, 4.0);
}

#[test]
fn set_gains_updates_behavior_without_global_singleton() {
    let config = PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    };
    let mut tuned = controller(config.clone());
    let mut baseline = controller(config);

    tuned.set_gains(2.0, 0.0, 0.0);

    let tuned_result = step(&mut tuned, 8.0, 1.0, 0.0);
    let baseline_result = step(&mut baseline, 8.0, 1.0, 0.0);

    assert_close(tuned_result.cv, 4.0);
    assert_close(baseline_result.cv, 2.0);
}

#[test]
fn same_inputs_produce_same_outputs_deterministically() {
    let config = PidConfig {
        setpoint: 10.0,
        kp: 1.5,
        ki: 0.5,
        ..PidConfig::default()
    };
    let mut left = controller(config.clone());
    let mut right = controller(config);

    let left_result = step(&mut left, 8.0, 0.5, 1.0);
    let right_result = step(&mut right, 8.0, 0.5, 1.0);

    assert_eq!(left_result, right_result);
}
