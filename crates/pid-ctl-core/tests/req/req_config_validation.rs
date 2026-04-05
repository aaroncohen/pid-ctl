//! `PidConfig` validation — `pid-ctl_plan.md` (Reliability item 5, Scaling & Filtering, Safety ranges).
//!
//! The core should reject invalid configurations before any computation occurs.

use pid_ctl_core::{ConfigError, PidConfig};

#[test]
fn rejects_pv_filter_alpha_at_or_above_one() {
    let config = PidConfig {
        pv_filter_alpha: 1.0,
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidPvFilterAlpha { alpha: 1.0 })
    );
}

#[test]
fn rejects_pv_filter_alpha_below_zero() {
    let config = PidConfig {
        pv_filter_alpha: -0.01,
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidPvFilterAlpha { alpha: -0.01 })
    );
}

#[test]
fn rejects_out_min_greater_than_out_max() {
    let config = PidConfig {
        out_min: 10.0,
        out_max: 5.0,
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidOutputLimits {
            min: 10.0,
            max: 5.0,
        })
    );
}

#[test]
fn rejects_negative_setpoint_ramp_rate() {
    let config = PidConfig {
        setpoint_ramp: Some(-0.5),
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::NegativeSetpointRampRate { rate: -0.5 })
    );
}

#[test]
fn rejects_negative_slew_rate() {
    let config = PidConfig {
        slew_rate: Some(-0.5),
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::NegativeSlewRate { rate: -0.5 })
    );
}

#[test]
fn rejects_negative_deadband() {
    let config = PidConfig {
        deadband: -0.001,
        ..PidConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(ConfigError::NegativeDeadband { deadband: -0.001 })
    );
}

#[test]
fn accepts_default_infinite_limits() {
    let config = PidConfig::default();

    assert!(config.out_min.is_infinite() && config.out_min.is_sign_negative());
    assert!(config.out_max.is_infinite() && config.out_max.is_sign_positive());
    assert_eq!(config.validate(), Ok(()));
}
