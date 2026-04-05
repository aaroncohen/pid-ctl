//! Runtime gain changes and Ki=0 freeze — `pid-ctl_plan.md` (Anti-windup, Core API `set_gains`).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn ki_set_to_zero_freezes_i_acc_at_current_value() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 8.0, 1.0, 0.0);
    controller.set_gains(0.0, 0.0, 0.0);
    let second = step(&mut controller, 8.0, 1.0, first.cv);

    assert_close(first.i_acc, 2.0);
    assert_close(second.i_acc, 2.0);
    assert_close(second.i_term, 0.0);
}

#[test]
fn frozen_i_acc_only_cleared_by_explicit_reset_not_by_ki_zero() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 8.0, 1.0, 0.0);
    controller.set_gains(0.0, 0.0, 0.0);
    let frozen = step(&mut controller, 8.0, 1.0, first.cv);
    controller.reset_integral();
    let cleared = step(&mut controller, 8.0, 1.0, frozen.cv);

    assert_close(frozen.i_acc, 2.0);
    assert_close(cleared.i_acc, 0.0);
}

#[test]
fn two_independent_controllers_do_not_share_state() {
    let mut left = controller(PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        kp: 0.0,
        ..PidConfig::default()
    });
    let mut right = controller(PidConfig {
        setpoint: 10.0,
        ki: 0.5,
        kp: 0.0,
        ..PidConfig::default()
    });

    let left_result = step(&mut left, 8.0, 1.0, 0.0);
    let right_result = step(&mut right, 8.0, 1.0, 0.0);

    assert_close(left_result.i_acc, 2.0);
    assert_close(right_result.i_acc, 2.0);
    assert_close(left_result.i_term, 2.0);
    assert_close(right_result.i_term, 1.0);
}

#[test]
fn gain_change_mid_run_affects_next_step_output() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 8.0, 1.0, 0.0);
    controller.set_gains(2.0, 0.0, 0.0);
    let second = step(&mut controller, 8.0, 1.0, first.cv);

    assert_close(first.cv, 2.0);
    assert_close(second.cv, 4.0);
}
