//! Multi-step continuity — `pid-ctl_plan.md` (Core state, Controller form, D-on-measurement).
//!
//! The core must carry state between sequential `step()` calls: integral accumulates,
//! D-term uses prior tick's PV, filter state persists. All observable through `StepResult`.

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn integral_accumulates_across_multiple_ticks() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        ki: 1.0,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 8.0, 1.0, 0.0);
    let second = step(&mut controller, 8.0, 1.0, first.cv);

    assert_close(first.i_acc, 2.0);
    assert_close(second.i_acc, 4.0);
}

#[test]
fn d_term_uses_previous_ticks_pv_not_default() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kd: 2.0,
        ..PidConfig::default()
    });

    step(&mut controller, 10.0, 0.5, 0.0);
    let second = step(&mut controller, 11.0, 0.5, 0.0);

    assert_close(second.d_term, -4.0);
}

#[test]
fn filter_state_carries_between_steps() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        pv_filter_alpha: 0.25,
        ..PidConfig::default()
    });

    step(&mut controller, 8.0, 1.0, 0.0);
    let _ = step(&mut controller, 12.0, 1.0, 0.0);

    assert_close(controller.last_pv().expect("seeded"), 11.0);
}

#[test]
fn multi_tick_step_response_converges_toward_setpoint() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 0.8,
        ki: 0.15,
        out_min: 0.0,
        out_max: 20.0,
        ..PidConfig::default()
    });

    let mut pv = 0.0_f64;
    let initial_error = (10.0 - pv).abs();
    let mut prev_applied_cv = 0.0;

    for _ in 0..20 {
        let result = step(&mut controller, pv, 1.0, prev_applied_cv);
        prev_applied_cv = result.cv;
        pv += 0.25 * (result.cv - pv);
    }

    let final_error = (10.0 - pv).abs();
    assert!(
        final_error < initial_error,
        "expected {final_error} < {initial_error}"
    );
}
