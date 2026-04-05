//! Cross-cutting single-step behavior — `pid-ctl_plan.md` (Core API, full tick semantics).

use crate::support::{assert_close, restored_controller, step};
use pid_ctl_core::{AntiWindupStrategy, PidConfig, PidRuntimeState};
use proptest::prelude::*;

#[test]
fn one_step_integrates_filter_pid_clamp_slew_and_windup() {
    let mut controller = restored_controller(
        PidConfig {
            setpoint: 10.0,
            kp: 2.0,
            ki: 1.0,
            kd: 0.5,
            out_min: 0.0,
            out_max: 4.0,
            setpoint_ramp: Some(2.0),
            slew_rate: Some(0.5),
            pv_filter_alpha: 0.25,
            anti_windup: AntiWindupStrategy::Clamp,
            ..PidConfig::default()
        },
        &PidRuntimeState {
            i_acc: 10.0,
            last_pv: Some(8.0),
            effective_sp: Some(9.0),
            ..PidRuntimeState::default()
        },
    );

    let result = step(&mut controller, 12.0, 1.0, 1.0);

    assert_close(result.p_term, -2.0);
    assert_close(result.d_term, -1.5);
    assert_close(result.i_acc, 7.5);
    assert_close(result.i_term, 7.5);
    assert_close(result.u_unclamped, 4.0);
    assert_close(result.cv, 1.5);
    assert_close(result.effective_sp, 10.0);
    assert!(!result.saturated);
}

proptest! {
    #[test]
    fn property_output_bounded_when_gains_and_limits_favor_bounded_cv(
        kp in 0.0f64..20.0,
        setpoint in -50.0f64..50.0,
        pv in -50.0f64..50.0,
        out_min in -100.0f64..0.0,
        out_max in 0.0f64..100.0,
    ) {
        let mut controller = restored_controller(
            PidConfig {
                setpoint,
                kp,
                ki: 0.0,
                kd: 0.0,
                out_min,
                out_max,
                ..PidConfig::default()
            },
            &PidRuntimeState::default(),
        );

        let result = step(&mut controller, pv, 1.0, 0.0);

        prop_assert!(result.cv >= out_min);
        prop_assert!(result.cv <= out_max);
    }
}
