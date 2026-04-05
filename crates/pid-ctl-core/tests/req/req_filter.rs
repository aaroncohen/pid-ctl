//! PV EMA filter — `pid-ctl_plan.md` (Scaling & Filtering → `--pv-filter`, State `last_pv`).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn alpha_zero_is_no_filtering() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        pv_filter_alpha: 0.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 1.0, 0.0);

    assert_close(result.p_term, 2.0);
    assert_eq!(controller.last_pv(), Some(8.0));
}

#[test]
fn ema_matches_plan_recurrence() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        pv_filter_alpha: 0.25,
        ..PidConfig::default()
    });

    step(&mut controller, 8.0, 1.0, 0.0);
    let result = step(&mut controller, 12.0, 1.0, 0.0);

    assert_close(controller.last_pv().expect("seeded"), 11.0);
    assert_close(result.p_term, -1.0);
}

#[test]
fn first_tick_without_prior_last_pv_seeds_filter() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        pv_filter_alpha: 0.9,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 8.0, 1.0, 0.0);

    assert_close(result.p_term, 2.0);
    assert_eq!(controller.last_pv(), Some(8.0));
}

#[test]
fn filtered_pv_used_for_pid_and_recorded_for_next_tick() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        pv_filter_alpha: 0.5,
        ..PidConfig::default()
    });

    step(&mut controller, 8.0, 1.0, 0.0);
    let result = step(&mut controller, 12.0, 1.0, 0.0);

    assert_close(controller.last_pv().expect("seeded"), 10.0);
    assert_close(result.p_term, 0.0);
}
