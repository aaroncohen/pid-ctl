//! Anti-windup — `pid-ctl_plan.md` (Anti-windup, Reliability item 9, CLI `--anti-windup*`).

use crate::support::{assert_close, controller, restored_controller, step};
use pid_ctl_core::{AntiWindupStrategy, PidConfig, PidRuntimeState};

#[test]
fn default_back_calculation_reduces_integral_windup_when_saturated() {
    let config = PidConfig {
        setpoint: 10.0,
        kp: 4.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 2.0,
        ..PidConfig::default()
    };
    let mut back_calc = controller(config.clone());
    let mut none = controller(PidConfig {
        anti_windup: AntiWindupStrategy::None,
        ..config
    });

    let back_calc_result = step(&mut back_calc, 0.0, 1.0, 2.0);
    let none_result = step(&mut none, 0.0, 1.0, 2.0);

    assert!(back_calc_result.i_acc < none_result.i_acc);
    assert_close(back_calc_result.cv, 2.0);
    assert_close(none_result.cv, 2.0);
}

#[test]
fn back_calculation_uses_prev_applied_cv_from_step_input() {
    let config = PidConfig {
        setpoint: 10.0,
        kp: 4.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 2.0,
        ..PidConfig::default()
    };
    let mut low_prev = controller(config.clone());
    let mut high_prev = controller(config);

    let low_prev_result = step(&mut low_prev, 0.0, 1.0, 0.0);
    let high_prev_result = step(&mut high_prev, 0.0, 1.0, 2.0);

    assert_close(low_prev_result.i_acc, -2.5);
    assert_close(high_prev_result.i_acc, -2.0);
}

#[test]
fn tt_when_kd_positive_is_sqrt_kd_over_ki() {
    let mut controller = controller(PidConfig {
        setpoint: 1.0,
        kp: 0.0,
        ki: 1.0,
        kd: 9.0,
        out_min: f64::NEG_INFINITY,
        out_max: 0.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.i_acc, 2.0 / 3.0);
}

#[test]
fn tt_when_kd_zero_is_kp_over_ki() {
    let mut controller = controller(PidConfig {
        setpoint: 5.0,
        kp: 4.0,
        ki: 2.0,
        out_min: f64::NEG_INFINITY,
        out_max: 0.0,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.i_acc, -2.5);
}

#[test]
fn anti_windup_inactive_when_ki_zero() {
    let mut controller = restored_controller(
        PidConfig {
            setpoint: 10.0,
            kp: 4.0,
            ki: 0.0,
            out_min: f64::NEG_INFINITY,
            out_max: 0.0,
            ..PidConfig::default()
        },
        &PidRuntimeState {
            i_acc: 7.0,
            ..PidRuntimeState::default()
        },
    );

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.i_acc, 7.0);
    assert_close(result.i_term, 0.0);
}

#[test]
fn auto_tt_clamped_to_dt_and_upper_bound() {
    let mut lower_clamped = controller(PidConfig {
        setpoint: 1.0,
        kp: 0.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 0.0,
        ..PidConfig::default()
    });
    let lower = step(&mut lower_clamped, 0.0, 2.0, 0.0);
    assert_close(lower.i_acc, 0.0);

    let mut upper_clamped = controller(PidConfig {
        setpoint: 1.0,
        kp: 1000.0,
        ki: 0.001,
        out_min: f64::NEG_INFINITY,
        out_max: 0.0,
        ..PidConfig::default()
    });
    let upper = step(&mut upper_clamped, 0.0, 1.0, 0.0);
    assert_close(upper.i_term, -9.99901);
}

#[test]
fn integral_contribution_not_promised_to_match_raw_i_acc_units() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 0.0,
        ki: 0.5,
        out_min: f64::NEG_INFINITY,
        out_max: 4.0,
        anti_windup: AntiWindupStrategy::Clamp,
        ..PidConfig::default()
    });

    let result = step(&mut controller, 0.0, 1.0, 0.0);

    assert_close(result.i_term, 4.0);
    assert_close(result.i_acc, 8.0);
    assert!(result.i_acc > result.cv);
}

#[test]
fn anti_windup_strategy_clamp_behaves_differently_from_back_calc() {
    let base = PidConfig {
        setpoint: 10.0,
        kp: 4.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 2.0,
        ..PidConfig::default()
    };
    let mut clamp_strategy = controller(PidConfig {
        anti_windup: AntiWindupStrategy::Clamp,
        ..base.clone()
    });
    let mut back_calc_strategy = controller(PidConfig {
        anti_windup: AntiWindupStrategy::BackCalculation,
        ..base
    });

    let clamp_result = step(&mut clamp_strategy, 0.0, 1.0, 2.0);
    let back_calc_result = step(&mut back_calc_strategy, 0.0, 1.0, 2.0);

    assert_close(clamp_result.cv, 2.0);
    assert_close(back_calc_result.cv, 2.0);
    assert!((clamp_result.i_acc - back_calc_result.i_acc).abs() > 1e-9);
}

#[test]
fn anti_windup_strategy_none_allows_unconstrained_integral_accumulation() {
    let mut controller = controller(PidConfig {
        setpoint: 10.0,
        kp: 0.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 1.0,
        anti_windup: AntiWindupStrategy::None,
        ..PidConfig::default()
    });

    let first = step(&mut controller, 0.0, 1.0, 0.0);
    let second = step(&mut controller, 0.0, 1.0, first.cv);

    assert_close(first.i_acc, 10.0);
    assert_close(second.i_acc, 20.0);
    assert_close(second.cv, 1.0);
}

#[test]
fn manual_anti_windup_tt_override_bypasses_auto_tt_clamp() {
    let base = PidConfig {
        setpoint: 1.0,
        kp: 0.0,
        ki: 1.0,
        out_min: f64::NEG_INFINITY,
        out_max: 0.0,
        ..PidConfig::default()
    };
    let mut auto_tt = controller(base.clone());
    let mut manual_tt = controller(PidConfig {
        anti_windup_tt: Some(0.5),
        ..base
    });

    let auto_result = step(&mut auto_tt, 0.0, 1.0, 0.0);
    let manual_result = step(&mut manual_tt, 0.0, 1.0, 0.0);

    assert_close(auto_result.i_acc, 0.0);
    assert_close(manual_result.i_acc, -1.0);
}
