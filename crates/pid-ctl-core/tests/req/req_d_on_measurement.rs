//! D-on-measurement and unreliable derivative — `pid-ctl_plan.md` (D-on-measurement, Reliability item 15).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::{DTermSkipReason, PidConfig};

#[test]
fn derivative_uses_pv_rate_not_error_rate() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 2.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    step(&mut controller, 10.0, 1.0, 0.0);
    controller.set_setpoint(20.0);
    let result = step(&mut controller, 10.0, 1.0, 0.0);

    assert_close(result.d_term, 0.0);
}

#[test]
fn setpoint_step_without_pv_step_does_not_spike_d_via_error() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 3.0,
        kp: 0.0,
        ..PidConfig::default()
    });

    step(&mut controller, 10.0, 1.0, 0.0);
    controller.set_setpoint(12.0);
    let setpoint_only = step(&mut controller, 10.0, 1.0, 0.0);
    let pv_change = step(&mut controller, 11.0, 1.0, 0.0);

    assert_close(setpoint_only.d_term, 0.0);
    assert_close(pv_change.d_term, -3.0);
}

#[test]
fn first_tick_or_no_prior_pv_zeroes_d_and_seeds_last_pv() {
    let mut controller = controller(PidConfig {
        setpoint: 12.0,
        kp: 1.0,
        ki: 1.0,
        kd: 5.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 10.0, 0.5, 0.0);

    assert_close(result.d_term, 0.0);
    assert_close(result.p_term, 2.0);
    assert_close(result.i_term, 1.0);
    assert_eq!(controller.last_pv(), Some(10.0));
}

#[test]
fn after_live_integrator_reset_d_term_zeroed_and_pv_seeded() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 2.0,
        ..PidConfig::default()
    });

    step(&mut controller, 9.0, 1.0, 0.0);
    controller.reset_integral();
    let result = step(&mut controller, 11.0, 1.0, 0.0);

    assert_close(result.d_term, 0.0);
    assert_eq!(result.d_term_skipped, Some(DTermSkipReason::PostReset));
    assert_eq!(controller.last_pv(), Some(11.0));
}

#[test]
fn after_simulated_dt_gap_d_term_zeroes_and_pv_seeds_like_post_dt_skip() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 2.0,
        ..PidConfig::default()
    });

    step(&mut controller, 9.0, 1.0, 0.0);
    controller.mark_dt_skipped();
    let result = step(&mut controller, 11.0, 1.0, 0.0);

    assert_close(result.d_term, 0.0);
    assert_eq!(result.d_term_skipped, Some(DTermSkipReason::PostDtSkip));
    assert_eq!(controller.last_pv(), Some(11.0));
}

#[test]
fn diagnostic_or_flag_reflects_d_term_skipped_reason_when_applicable() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 1.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 9.0, 1.0, 0.0);
    assert_eq!(first.d_term_skipped, Some(DTermSkipReason::NoPvPrev));

    controller.reset_integral();
    let post_reset = step(&mut controller, 10.0, 1.0, 0.0);
    assert_eq!(post_reset.d_term_skipped, Some(DTermSkipReason::PostReset));

    controller.mark_dt_skipped();
    let post_dt_skip = step(&mut controller, 11.0, 1.0, 0.0);
    assert_eq!(
        post_dt_skip.d_term_skipped,
        Some(DTermSkipReason::PostDtSkip)
    );
}

#[test]
fn p_and_i_still_compute_normally_when_d_is_zeroed() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ki: 1.0,
        kd: 5.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 0.5, 0.0);

    assert_close(result.d_term, 0.0);
    assert_eq!(result.d_term_skipped, Some(DTermSkipReason::NoPvPrev));
    assert_close(result.p_term, 4.0);
    assert_close(result.i_term, 1.0);
}

#[test]
fn kd_zero_always_yields_zero_d_term() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 0.0,
        ..PidConfig::default()
    });

    step(&mut controller, 8.0, 1.0, 0.0);
    let result = step(&mut controller, 12.0, 1.0, 0.0);

    assert_close(result.d_term, 0.0);
}
