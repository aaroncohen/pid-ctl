//! Deterministic control semantics for **pid-ctl**: PV filtering, deadband, setpoint ramping,
//! PID computation (position form, D-on-measurement), output clamping, slew-rate limiting,
//! and anti-windup. Pure in-memory state; no I/O.
//!
//! Specification: repository root `pid-ctl_plan.md` (Architecture & Code Structure, PID Implementation).

#![forbid(unsafe_code)]

// TODO: filter module is currently empty — either move `filtered_pv` logic here
// or remove this module declaration to avoid dead-weight confusion.
pub mod filter;

use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AntiWindupStrategy {
    BackCalculation,
    Clamp,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DTermSkipReason {
    NoPvPrev,
    PostDtSkip,
    PostReset,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PidConfig {
    pub setpoint: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub out_min: f64,
    pub out_max: f64,
    pub deadband: f64,
    pub setpoint_ramp: Option<f64>,
    pub slew_rate: Option<f64>,
    pub pv_filter_alpha: f64,
    pub anti_windup: AntiWindupStrategy,
    pub anti_windup_tt: Option<f64>,
    pub tt_upper_bound: Option<f64>,
}

impl Default for PidConfig {
    fn default() -> Self {
        Self {
            setpoint: 0.0,
            kp: 1.0,
            ki: 0.0,
            kd: 0.0,
            out_min: f64::NEG_INFINITY,
            out_max: f64::INFINITY,
            deadband: 0.0,
            setpoint_ramp: None,
            slew_rate: None,
            pv_filter_alpha: 0.0,
            anti_windup: AntiWindupStrategy::BackCalculation,
            anti_windup_tt: None,
            tt_upper_bound: None,
        }
    }
}

impl PidConfig {
    /// Validates that configuration values are internally consistent before the
    /// controller performs any PID computation.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a field violates the documented CLI and core
    /// contract, such as invalid filter ranges or impossible output limits.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(0.0..1.0).contains(&self.pv_filter_alpha) {
            return Err(ConfigError::InvalidPvFilterAlpha {
                alpha: self.pv_filter_alpha,
            });
        }

        if self.out_min > self.out_max {
            return Err(ConfigError::InvalidOutputLimits {
                min: self.out_min,
                max: self.out_max,
            });
        }

        if let Some(rate) = self.setpoint_ramp
            && rate < 0.0
        {
            return Err(ConfigError::NegativeSetpointRampRate { rate });
        }

        if let Some(rate) = self.slew_rate
            && rate < 0.0
        {
            return Err(ConfigError::NegativeSlewRate { rate });
        }

        if self.deadband < 0.0 {
            return Err(ConfigError::NegativeDeadband {
                deadband: self.deadband,
            });
        }

        if let Some(bound) = self.tt_upper_bound
            && bound <= 0.0
        {
            return Err(ConfigError::NonPositiveTtUpperBound { bound });
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepInput {
    pub pv: f64,
    pub dt: f64,
    pub prev_applied_cv: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    pub cv: f64,
    pub u_unclamped: f64,
    pub p_term: f64,
    pub i_term: f64,
    pub d_term: f64,
    pub i_acc: f64,
    pub effective_sp: f64,
    pub saturated: bool,
    pub d_term_skipped: Option<DTermSkipReason>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PidRuntimeState {
    pub i_acc: f64,
    pub last_pv: Option<f64>,
    pub last_error: Option<f64>,
    pub last_cv: Option<f64>,
    pub effective_sp: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PidController {
    config: PidConfig,
    i_acc: f64,
    last_pv: Option<f64>,
    last_error: Option<f64>,
    last_cv: Option<f64>,
    effective_sp: Option<f64>,
    next_d_term_skip_reason: Option<DTermSkipReason>,
}

impl PidController {
    /// Constructs a controller with validated configuration and empty runtime
    /// state.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when `config` fails validation.
    pub fn new(config: PidConfig) -> Result<Self, ConfigError> {
        config.validate()?;

        Ok(Self {
            config,
            i_acc: 0.0,
            last_pv: None,
            last_error: None,
            last_cv: None,
            effective_sp: None,
            next_d_term_skip_reason: None,
        })
    }

    #[must_use]
    pub fn config(&self) -> &PidConfig {
        &self.config
    }

    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) {
        self.config.kp = kp;
        self.config.ki = ki;
        self.config.kd = kd;
    }

    pub fn set_setpoint(&mut self, setpoint: f64) {
        self.config.setpoint = setpoint;
    }

    pub fn restore_state(&mut self, state: &PidRuntimeState) {
        self.i_acc = state.i_acc;
        self.last_pv = state.last_pv;
        self.last_error = state.last_error;
        self.last_cv = state.last_cv;
        self.effective_sp = state.effective_sp;
    }

    pub fn mark_dt_skipped(&mut self) {
        self.next_d_term_skip_reason = Some(DTermSkipReason::PostDtSkip);
    }

    pub fn reset_integral(&mut self) {
        self.i_acc = 0.0;
        self.next_d_term_skip_reason = Some(DTermSkipReason::PostReset);
    }

    pub fn step(&mut self, input: StepInput) -> StepResult {
        let filtered_pv = self.filtered_pv(input.pv);
        let effective_sp = self.next_effective_sp(input.dt);
        let error = effective_sp - filtered_pv;
        let effective_error = if error.abs() < self.config.deadband {
            0.0
        } else {
            error
        };

        let p_term = self.config.kp * effective_error;
        let (d_term, d_term_skipped) = self.d_term(filtered_pv, input.dt);
        let candidate_i_acc = if self.config.ki == 0.0 {
            self.i_acc
        } else {
            self.i_acc + (effective_error * input.dt)
        };

        let (i_acc, i_term) = self.apply_anti_windup(
            candidate_i_acc,
            p_term,
            d_term,
            input.prev_applied_cv,
            input.dt,
        );
        let u_unclamped = p_term + i_term + d_term;
        let saturated = u_unclamped < self.config.out_min || u_unclamped > self.config.out_max;
        let clamped_cv = clamp(u_unclamped, self.config.out_min, self.config.out_max);
        let cv = self.apply_slew_rate(clamped_cv, input.prev_applied_cv, input.dt);

        self.i_acc = i_acc;
        self.last_pv = Some(filtered_pv);
        self.last_error = Some(error);
        self.last_cv = Some(cv);
        self.effective_sp = Some(effective_sp);
        self.next_d_term_skip_reason = None;

        StepResult {
            cv,
            u_unclamped,
            p_term,
            i_term,
            d_term,
            i_acc,
            effective_sp,
            saturated,
            d_term_skipped,
        }
    }

    #[must_use]
    pub fn i_acc(&self) -> f64 {
        self.i_acc
    }

    #[must_use]
    pub fn last_pv(&self) -> Option<f64> {
        self.last_pv
    }

    #[must_use]
    pub fn last_error(&self) -> Option<f64> {
        self.last_error
    }

    #[must_use]
    pub fn last_cv(&self) -> Option<f64> {
        self.last_cv
    }

    #[must_use]
    pub fn effective_sp(&self) -> Option<f64> {
        self.effective_sp
    }

    fn filtered_pv(&self, raw_pv: f64) -> f64 {
        if self.config.pv_filter_alpha == 0.0 {
            raw_pv
        } else if let Some(prev_pv) = self.last_pv {
            ((1.0 - self.config.pv_filter_alpha) * raw_pv) + (self.config.pv_filter_alpha * prev_pv)
        } else {
            raw_pv
        }
    }

    fn next_effective_sp(&self, dt: f64) -> f64 {
        match (self.config.setpoint_ramp, self.effective_sp) {
            (None, _) | (Some(_), None) => self.config.setpoint,
            (Some(rate), Some(current)) => ramp_toward(current, self.config.setpoint, rate * dt),
        }
    }

    fn d_term(&self, filtered_pv: f64, dt: f64) -> (f64, Option<DTermSkipReason>) {
        if self.config.kd == 0.0 {
            return (0.0, None);
        }

        if let Some(reason) = self.next_d_term_skip_reason {
            return (0.0, Some(reason));
        }

        let Some(previous_pv) = self.last_pv else {
            return (0.0, Some(DTermSkipReason::NoPvPrev));
        };

        (-self.config.kd * ((filtered_pv - previous_pv) / dt), None)
    }

    fn apply_anti_windup(
        &self,
        candidate_i_acc: f64,
        p_term: f64,
        d_term: f64,
        prev_applied_cv: f64,
        dt: f64,
    ) -> (f64, f64) {
        if self.config.ki == 0.0 {
            return (self.i_acc, 0.0);
        }

        let candidate_i_term = self.config.ki * candidate_i_acc;
        let candidate_u_unclamped = p_term + candidate_i_term + d_term;

        match self.config.anti_windup {
            AntiWindupStrategy::None => (candidate_i_acc, candidate_i_term),
            AntiWindupStrategy::Clamp => {
                let pd_sum = p_term + d_term;
                let min_i_term = self.config.out_min - pd_sum;
                let max_i_term = self.config.out_max - pd_sum;
                let clamped_i_term = clamp(candidate_i_term, min_i_term, max_i_term);

                (clamped_i_term / self.config.ki, clamped_i_term)
            }
            AntiWindupStrategy::BackCalculation => {
                if candidate_u_unclamped >= self.config.out_min
                    && candidate_u_unclamped <= self.config.out_max
                {
                    return (candidate_i_acc, candidate_i_term);
                }

                let tt = self.anti_windup_tt(dt);
                let corrected_i_term =
                    candidate_i_term + ((prev_applied_cv - candidate_u_unclamped) * (dt / tt));

                (corrected_i_term / self.config.ki, corrected_i_term)
            }
        }
    }

    fn anti_windup_tt(&self, dt: f64) -> f64 {
        if let Some(tt) = self.config.anti_windup_tt {
            return tt;
        }

        let auto_tt = if self.config.ki == 0.0 {
            dt
        } else if self.config.kd > 0.0 {
            (self.config.kd / self.config.ki).sqrt()
        } else {
            self.config.kp / self.config.ki
        };

        auto_tt.clamp(dt, self.config.tt_upper_bound.unwrap_or(100.0))
    }

    fn apply_slew_rate(&self, clamped_cv: f64, prev_applied_cv: f64, dt: f64) -> f64 {
        let Some(rate) = self.config.slew_rate else {
            return clamped_cv;
        };

        let max_delta = rate * dt;
        let min_cv = prev_applied_cv - max_delta;
        let max_cv = prev_applied_cv + max_delta;

        clamp(clamped_cv, min_cv, max_cv)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfigError {
    InvalidPvFilterAlpha { alpha: f64 },
    InvalidOutputLimits { min: f64, max: f64 },
    NegativeSetpointRampRate { rate: f64 },
    NegativeSlewRate { rate: f64 },
    NegativeDeadband { deadband: f64 },
    NonPositiveTtUpperBound { bound: f64 },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPvFilterAlpha { alpha } => {
                write!(f, "pv filter alpha must be in [0.0, 1.0), got {alpha}")
            }
            Self::InvalidOutputLimits { min, max } => {
                write!(
                    f,
                    "output limits must satisfy out_min <= out_max, got {min} > {max}"
                )
            }
            Self::NegativeSetpointRampRate { rate } => {
                write!(f, "setpoint ramp rate must be non-negative, got {rate}")
            }
            Self::NegativeSlewRate { rate } => {
                write!(f, "slew rate must be non-negative, got {rate}")
            }
            Self::NegativeDeadband { deadband } => {
                write!(f, "deadband must be non-negative, got {deadband}")
            }
            Self::NonPositiveTtUpperBound { bound } => {
                write!(f, "tt_upper_bound must be positive, got {bound}")
            }
        }
    }
}

impl Error for ConfigError {}

// TODO: consider replacing with f64::clamp — functionally equivalent here since
// inputs are validated, but std's version is more idiomatic.
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn ramp_toward(current: f64, target: f64, max_delta: f64) -> f64 {
    if (target - current).abs() <= max_delta {
        target
    } else if target > current {
        current + max_delta
    } else {
        current - max_delta
    }
}

#[cfg(test)]
mod proptest_smoke {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_toolchain_smoke(x in 0i32..10) {
            prop_assert!((0..10).contains(&x));
        }
    }
}
