//! Position form, term structure — `pid-ctl_plan.md` (PID Implementation → Controller form).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn v1_is_position_form_absolute_cv_per_tick() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 9.0, 1.0, 0.0);
    let second = step(&mut controller, 9.0, 1.0, first.cv);

    assert_close(first.cv, 2.0);
    assert_close(second.cv, 2.0);
}

#[test]
fn p_term_matches_kp_times_error() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.5,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 1.0, 0.0);

    assert_close(result.p_term, 3.0);
}

#[test]
fn d_term_matches_negative_kd_times_pv_rate() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 2.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 10.0, 0.5, 0.0);
    let second = step(&mut controller, 11.0, 0.5, first.cv);

    assert_close(second.d_term, -4.0);
}

#[test]
fn integral_path_uses_i_acc_and_error_dt() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 3.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 0.5, 0.0);

    assert_close(result.i_acc, 1.0);
    assert_close(result.i_term, 3.0);
}

#[test]
fn u_unclamped_is_sum_of_p_i_d_contributions() {
    let mut controller = controller(PidConfig {
        setpoint: 12.0,
        kp: 2.0,
        ki: 1.0,
        kd: 4.0,
        ..PidConfig::default()
    });

    step(&mut controller, 10.0, 0.5, 0.0);
    let result = step(&mut controller, 9.0, 0.5, 0.0);

    assert_close(
        result.u_unclamped,
        result.p_term + result.i_term + result.d_term,
    );
}

#[test]
fn step_result_exposes_term_diagnostics_matching_definitions() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ki: 2.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 0.5, 0.0);

    assert_close(result.p_term, 2.0);
    assert_close(result.i_term, 2.0);
    assert_close(result.d_term, 0.0);
    assert_close(result.u_unclamped, 4.0);
    assert_close(result.cv, 4.0);
}
