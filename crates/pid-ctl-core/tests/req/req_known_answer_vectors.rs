//! Hand-worked known-answer vectors for `pid-ctl-core`.
//!
//! These tests are meant to be audit-friendly: the expected values come from the
//! documented controller equations in `pid-ctl_plan.md`, worked out step by step,
//! not copied from internal helpers.

use crate::support::{assert_close, controller, step};
use pid_ctl_core::PidConfig;

#[test]
fn hand_worked_position_form_pid_sequence_matches_exact_outputs() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.2,
        ki: 0.5,
        kd: 0.25,
        ..PidConfig::default()
    });

    // Worked values:
    // step 1: error = 2.0
    //   p = 1.2 * 2.0 = 2.4
    //   i_acc = 0.0 + 2.0 * 0.5 = 1.0, i = 0.5 * 1.0 = 0.5
    //   d = 0.0 (first tick, no prior PV)
    //   cv = 2.4 + 0.5 + 0.0 = 2.9
    let first = step(&mut controller, 8.0, 0.5, 0.0);
    assert_close(first.p_term, 2.4);
    assert_close(first.i_acc, 1.0);
    assert_close(first.i_term, 0.5);
    assert_close(first.d_term, 0.0);
    assert_close(first.cv, 2.9);

    // step 2: error = 1.0
    //   p = 1.2
    //   i_acc = 1.0 + 1.0 * 0.5 = 1.5, i = 0.75
    //   d = -0.25 * ((9.0 - 8.0) / 0.5) = -0.5
    //   cv = 1.2 + 0.75 - 0.5 = 1.45
    let second = step(&mut controller, 9.0, 0.5, first.cv);
    assert_close(second.p_term, 1.2);
    assert_close(second.i_acc, 1.5);
    assert_close(second.i_term, 0.75);
    assert_close(second.d_term, -0.5);
    assert_close(second.cv, 1.45);

    // step 3: error = 0.5
    //   p = 0.6
    //   i_acc = 1.5 + 0.5 * 0.5 = 1.75, i = 0.875
    //   d = -0.25 * ((9.5 - 9.0) / 0.5) = -0.25
    //   cv = 0.6 + 0.875 - 0.25 = 1.225
    let third = step(&mut controller, 9.5, 0.5, second.cv);
    assert_close(third.p_term, 0.6);
    assert_close(third.i_acc, 1.75);
    assert_close(third.i_term, 0.875);
    assert_close(third.d_term, -0.25);
    assert_close(third.cv, 1.225);
}

#[test]
fn hand_worked_deadband_ramp_and_filter_sequence_matches_exact_outputs() {
    let mut controller = controller(PidConfig {
        setpoint: 0.0,
        setpoint_ramp: Some(1.0),
        pv_filter_alpha: 0.5,
        deadband: 0.25,
        kp: 2.0,
        ki: 1.0,
        ..PidConfig::default()
    });

    let seeded = step(&mut controller, 0.0, 1.0, 0.0);
    assert_close(seeded.effective_sp, 0.0);
    assert_close(seeded.cv, 0.0);

    controller.set_setpoint(1.0);

    // step 2:
    //   filtered PV = 0.5 * 0.6 + 0.5 * 0.0 = 0.3
    //   effective_sp = 0.0 + 1.0 * 0.5 = 0.5
    //   error = 0.5 - 0.3 = 0.2, which is inside the 0.25 deadband
    //   effective_error = 0.0, so P and I stay at zero
    let inside_deadband = step(&mut controller, 0.6, 0.5, seeded.cv);
    assert_close(inside_deadband.effective_sp, 0.5);
    assert_close(inside_deadband.p_term, 0.0);
    assert_close(inside_deadband.i_acc, 0.0);
    assert_close(inside_deadband.i_term, 0.0);
    assert_close(inside_deadband.cv, 0.0);

    // step 3:
    //   filtered PV = 0.5 * 0.2 + 0.5 * 0.3 = 0.25
    //   effective_sp = 0.5 + 1.0 * 0.5 = 1.0
    //   error = 1.0 - 0.25 = 0.75, outside deadband
    //   p = 2.0 * 0.75 = 1.5
    //   i_acc = 0.0 + 0.75 * 0.5 = 0.375, i = 0.375
    //   cv = 1.5 + 0.375 = 1.875
    let outside_deadband = step(&mut controller, 0.2, 0.5, inside_deadband.cv);
    assert_close(outside_deadband.effective_sp, 1.0);
    assert_close(outside_deadband.p_term, 1.5);
    assert_close(outside_deadband.i_acc, 0.375);
    assert_close(outside_deadband.i_term, 0.375);
    assert_close(outside_deadband.cv, 1.875);
}

#[test]
fn hand_worked_output_clamp_and_slew_sequence_matches_exact_outputs() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 2.0,
        ki: 1.0,
        out_min: 0.0,
        out_max: 4.0,
        slew_rate: Some(1.5),
        ..PidConfig::default()
    });

    // step 1:
    //   error = 2.0, p = 4.0
    //   candidate i_acc = 2.0, candidate i = 2.0
    //   back-calculation anti-windup sees candidate_u = 6.0 > 4.0 and Tt = kp / ki = 2.0
    //   corrected i = 2.0 + ((0.0 - 6.0) * (1.0 / 2.0)) = -1.0
    //   u = 4.0 - 1.0 = 3.0
    //   clamp keeps 3.0, then slew limits 0.0 -> 1.5
    let first = step(&mut controller, 8.0, 1.0, 0.0);
    assert_close(first.p_term, 4.0);
    assert_close(first.i_acc, -1.0);
    assert_close(first.i_term, -1.0);
    assert_close(first.u_unclamped, 3.0);
    assert_close(first.cv, 1.5);

    // step 2:
    //   error = 1.0, p = 2.0
    //   candidate i_acc = -1.0 + 1.0 = 0.0, i = 0.0
    //   u = 2.0, already in range
    //   slew allows 1.5 -> 2.0 because max delta is 1.5
    let second = step(&mut controller, 9.0, 1.0, first.cv);
    assert_close(second.p_term, 2.0);
    assert_close(second.i_acc, 0.0);
    assert_close(second.i_term, 0.0);
    assert_close(second.u_unclamped, 2.0);
    assert_close(second.cv, 2.0);

    // step 3:
    //   error = 0.5, p = 1.0
    //   candidate i_acc = 0.0 + 0.5 = 0.5, i = 0.5
    //   u = 1.5, in range and within slew limit from 2.0
    let third = step(&mut controller, 9.5, 1.0, second.cv);
    assert_close(third.p_term, 1.0);
    assert_close(third.i_acc, 0.5);
    assert_close(third.i_term, 0.5);
    assert_close(third.u_unclamped, 1.5);
    assert_close(third.cv, 1.5);
}
