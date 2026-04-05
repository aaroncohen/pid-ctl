//! `dt` as input to core math — `pid-ctl_plan.md` (dt handling math callsites; Architecture → core tests not wall-clock).

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn step_uses_explicit_dt_for_all_per_second_scaling() {
    let config = PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        ..PidConfig::default()
    };
    let mut short_dt = controller(config.clone());
    let mut long_dt = controller(config);

    let short = step(&mut short_dt, 8.0, 0.5, 0.0);
    let long = step(&mut long_dt, 8.0, 2.0, 0.0);

    assert_close(short.i_acc, 1.0);
    assert_close(long.i_acc, 4.0);
}

#[test]
fn tests_can_run_arbitrary_many_steps_without_sleep() {
    let mut controller = controller(PidConfig {
        setpoint: 1.0,
        ki: 1.0,
        ..PidConfig::default()
    });

    for _ in 0..1_000 {
        let _ = step(&mut controller, 0.0, 0.01, 0.0);
    }

    assert_close(controller.i_acc(), 10.0);
}

#[test]
fn fixed_dt_override_semantics_match_scalar_input() {
    let config = PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ki: 0.5,
        ..PidConfig::default()
    };
    let mut left = controller(config.clone());
    let mut right = controller(config);

    let left_first = step(&mut left, 8.0, 0.25, 0.0);
    let right_first = step(&mut right, 8.0, 0.25, 0.0);
    let left_second = step(&mut left, 9.0, 0.25, left_first.cv);
    let right_second = step(&mut right, 9.0, 0.25, right_first.cv);

    assert_eq!(left_first, right_first);
    assert_eq!(left_second, right_second);
}
