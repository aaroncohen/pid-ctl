//! Feed-forward path — `pid-ctl_plan.md` (issue #26).
//!
//! Tests observe the public `StepResult` API (`cv`, `ff_term`, `i_acc`) rather than
//! internal controller state — "social" style that validates integration points.

use crate::support::{assert_close, controller};
use pid_ctl_core::{AntiWindupStrategy, PidConfig, StepInput};

fn step_ff(ctrl: &mut pid_ctl_core::PidController, pv: f64, ff: f64) -> pid_ctl_core::StepResult {
    ctrl.step(StepInput {
        pv,
        dt: 1.0,
        prev_applied_cv: 0.0,
        ff,
    })
}

#[test]
fn ff_zero_gain_has_no_effect() {
    let mut ctrl = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        feedforward_gain: 0.0,
        ..PidConfig::default()
    });

    let result = step_ff(&mut ctrl, 7.0, 100.0);

    // ff_term should be zero; cv = kp * (sp - pv) = 3.0
    assert_close(result.ff_term, 0.0);
    assert_close(result.cv, 3.0);
}

#[test]
fn ff_term_equals_gain_times_raw_value() {
    let mut ctrl = controller(PidConfig {
        setpoint: 0.0,
        kp: 0.0,
        ki: 0.0,
        kd: 0.0,
        feedforward_gain: 0.5,
        ..PidConfig::default()
    });

    let result = step_ff(&mut ctrl, 0.0, 10.0);

    // P=I=D=0; cv = ff_gain * ff = 0.5 * 10 = 5
    assert_close(result.ff_term, 5.0);
    assert_close(result.cv, 5.0);
}

#[test]
fn ff_adds_to_pid_output() {
    let mut ctrl = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ki: 0.0,
        kd: 0.0,
        feedforward_gain: 1.0,
        ..PidConfig::default()
    });

    let result = step_ff(&mut ctrl, 7.0, 2.0);

    // p_term = kp * (sp - pv) = 1*3 = 3; ff_term = 1*2 = 2; cv = 5
    assert_close(result.p_term, 3.0);
    assert_close(result.ff_term, 2.0);
    assert_close(result.cv, 5.0);
}

#[test]
fn ff_respects_output_limits() {
    let mut ctrl = controller(PidConfig {
        setpoint: 10.0,
        kp: 1.0,
        ki: 0.0,
        kd: 0.0,
        out_min: 0.0,
        out_max: 4.0,
        feedforward_gain: 1.0,
        ..PidConfig::default()
    });

    // p=3, ff=2 → unclamped=5, clamped to 4
    let result = step_ff(&mut ctrl, 7.0, 2.0);
    assert_close(result.cv, 4.0);
    assert!(result.saturated);
}

#[test]
fn ff_alone_exceeds_out_max_anti_windup_clamp_prevents_integral_runaway() {
    // When FF alone saturates the output, anti-windup (clamp) should prevent
    // integral build-up regardless of the residual error.
    let mut ctrl = controller(PidConfig {
        setpoint: 10.0,
        kp: 0.1,
        ki: 1.0,
        kd: 0.0,
        out_min: 0.0,
        out_max: 5.0,
        feedforward_gain: 1.0,
        anti_windup: AntiWindupStrategy::Clamp,
        ..PidConfig::default()
    });

    // ff_term = 1*10 = 10 → already saturates to out_max=5 on its own.
    // Run several ticks; i_acc must stay bounded rather than growing unboundedly.
    let mut prev_cv = 0.0;
    let mut last_i_acc = 0.0;
    for _ in 0..20 {
        let result = ctrl.step(StepInput {
            pv: 0.0,
            dt: 1.0,
            prev_applied_cv: prev_cv,
            ff: 10.0,
        });
        prev_cv = result.cv;
        last_i_acc = result.i_acc;
    }

    // With clamp anti-windup and FF dominating, i_acc should be ≤ out_max/ki.
    assert!(
        last_i_acc <= 5.0 / 1.0 + 1e-6,
        "integral windup detected: i_acc={last_i_acc}"
    );
}

#[test]
fn ff_integral_winds_down_when_ff_closes_the_gap() {
    // With FF supplying most of the control effort, integral should not
    // accumulate large values — the back-calculation anti-windup should act
    // on the post-FF saturation gap.
    let mut ctrl = controller(PidConfig {
        setpoint: 10.0,
        kp: 0.2,
        ki: 0.5,
        kd: 0.0,
        out_min: 0.0,
        out_max: 100.0,
        feedforward_gain: 1.0,
        anti_windup: AntiWindupStrategy::BackCalculation,
        ..PidConfig::default()
    });

    // Simulate: PV tracks the setpoint closely; FF provides most of the output.
    // After several ticks, the i_acc should stay moderate (not blow up).
    let mut prev_cv = 0.0;
    for _ in 0..30 {
        let result = ctrl.step(StepInput {
            pv: 9.8, // close to SP=10
            dt: 0.1,
            prev_applied_cv: prev_cv,
            ff: 9.0, // FF provides ~90% of the needed output
        });
        prev_cv = result.cv;
    }

    // i_acc should remain small because FF covers most of the load.
    assert!(
        ctrl.i_acc().abs() < 10.0,
        "i_acc unexpectedly large: {}",
        ctrl.i_acc()
    );
}
