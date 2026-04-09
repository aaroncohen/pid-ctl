//! Continuous-time dynamics integrated with explicit Euler steps (`x += dt * dx_dt`).

use crate::SimError;
use serde::{Deserialize, Serialize};

/// First-order lag toward `gain * cv`: \( \dot{x} = (K u - x) / \tau \).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FirstOrderParams {
    pub tau: f64,
    pub gain: f64,
}

impl FirstOrderParams {
    /// # Errors
    ///
    /// Returns [`SimError::Validation`] if `tau` is not finite and positive.
    pub fn validate(&self) -> Result<(), SimError> {
        if !self.tau.is_finite() || self.tau <= 0.0 {
            return Err(SimError::Validation(format!(
                "first-order tau must be finite and > 0, got {}",
                self.tau
            )));
        }
        if !self.gain.is_finite() {
            return Err(SimError::Validation(format!(
                "first-order gain must be finite, got {}",
                self.gain
            )));
        }
        Ok(())
    }
}

/// Advances `x` one step; returns the new PV (`x`).
#[must_use]
pub fn step_first_order(x: f64, cv: f64, dt: f64, p: &FirstOrderParams) -> f64 {
    let dx_dt = (p.gain * cv - x) / p.tau;
    x + dt * dx_dt
}

/// Thermal mass: ambient relaxation plus heater proportional to `cv`.
///
/// \( \dot{T} = (T_{amb} - T)/\tau + k_{heat} \cdot u \).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalParams {
    pub tau: f64,
    pub t_ambient: f64,
    /// Heater effectiveness (units of PV rate per unit CV).
    pub k_heat: f64,
}

impl ThermalParams {
    /// # Errors
    ///
    /// Returns [`SimError::Validation`] if `tau` is not finite and positive.
    pub fn validate(&self) -> Result<(), SimError> {
        if !self.tau.is_finite() || self.tau <= 0.0 {
            return Err(SimError::Validation(format!(
                "thermal tau must be finite and > 0, got {}",
                self.tau
            )));
        }
        if !self.t_ambient.is_finite() {
            return Err(SimError::Validation(format!(
                "thermal t_ambient must be finite, got {}",
                self.t_ambient
            )));
        }
        if !self.k_heat.is_finite() {
            return Err(SimError::Validation(format!(
                "thermal k_heat must be finite, got {}",
                self.k_heat
            )));
        }
        Ok(())
    }
}

/// Advances temperature one step; returns new `T`.
#[must_use]
pub fn step_thermal(t: f64, cv: f64, dt: f64, p: &ThermalParams) -> f64 {
    let d_dt = (p.t_ambient - t) / p.tau + p.k_heat * cv;
    t + dt * d_dt
}

/// Fan / airflow: first-order lag toward a **nonlinear** steady-state flow vs command.
///
/// Let `u = clamp(cv / cv_max, 0, 1)`. Target flow is `max_flow * u^exponent` (e.g. exponent 1.5
/// for a stylized fan curve). PV reported is the **lagged internal speed** (0…`max_flow`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FanParams {
    pub tau: f64,
    pub max_flow: f64,
    /// Normalization: command is scaled by this before clamping to `[0, 1]`.
    pub cv_max: f64,
    /// Steady-state map: `flow_ss = max_flow * u^exponent`.
    pub exponent: f64,
}

impl FanParams {
    /// # Errors
    ///
    /// Returns [`SimError::Validation`] if parameters are inconsistent.
    pub fn validate(&self) -> Result<(), SimError> {
        if !self.tau.is_finite() || self.tau <= 0.0 {
            return Err(SimError::Validation(format!(
                "fan tau must be finite and > 0, got {}",
                self.tau
            )));
        }
        if !self.max_flow.is_finite() || self.max_flow <= 0.0 {
            return Err(SimError::Validation(format!(
                "fan max_flow must be finite and > 0, got {}",
                self.max_flow
            )));
        }
        if !self.cv_max.is_finite() || self.cv_max <= 0.0 {
            return Err(SimError::Validation(format!(
                "fan cv_max must be finite and > 0, got {}",
                self.cv_max
            )));
        }
        if !self.exponent.is_finite() || self.exponent <= 0.0 {
            return Err(SimError::Validation(format!(
                "fan exponent must be finite and > 0, got {}",
                self.exponent
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn target_flow(&self, cv: f64) -> f64 {
        let u = (cv / self.cv_max).clamp(0.0, 1.0);
        self.max_flow * u.powf(self.exponent)
    }
}

/// Advances lag state toward `target_flow(cv)`; returns new internal speed (PV).
#[must_use]
pub fn step_fan(speed: f64, cv: f64, dt: f64, p: &FanParams) -> f64 {
    let target = p.target_flow(cv);
    let ds_dt = (target - speed) / p.tau;
    speed + dt * ds_dt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_order_converges_to_gain_times_cv() {
        let p = FirstOrderParams { tau: 1.0, gain: 2.0 };
        let mut x = 0.0;
        let dt = 0.1;
        for _ in 0..200 {
            x = step_first_order(x, 1.0, dt, &p);
        }
        assert!((x - 2.0).abs() < 0.02, "x={x}");
    }

    #[test]
    fn thermal_heats_toward_setpoint_when_ambient_below() {
        let p = ThermalParams {
            tau: 10.0,
            t_ambient: 20.0,
            k_heat: 0.5,
        };
        let mut t = 20.0;
        let dt = 0.5;
        for _ in 0..100 {
            t = step_thermal(t, 1.0, dt, &p);
        }
        // Steady state: (T_amb - T)/tau + k_heat*u = 0 => T = T_amb + tau*k_heat*u = 25
        assert!((t - 25.0).abs() < 0.05, "t={t}");
    }

    #[test]
    fn fan_reaches_nonlinear_steady_state() {
        let fp = FanParams {
            tau: 0.5,
            max_flow: 100.0,
            cv_max: 100.0,
            exponent: 1.5,
        };
        let target = fp.target_flow(100.0);
        let mut s = 0.0;
        let dt = 0.05;
        for _ in 0..500 {
            s = step_fan(s, 100.0, dt, &fp);
        }
        assert!((s - target).abs() < 0.5, "s={s} target={target}");
    }
}
