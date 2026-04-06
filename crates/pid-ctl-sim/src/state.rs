use crate::plant::{FanParams, FirstOrderParams, ThermalParams};
use serde::{Deserialize, Serialize};

/// Bump when the JSON shape changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level persisted simulation state (`--state` file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimState {
    pub schema_version: u32,
    pub plant: Plant,
}

/// Plant kind with parameters and scalar state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Plant {
    FirstOrder {
        #[serde(flatten)]
        params: FirstOrderParams,
        x: f64,
    },
    Thermal {
        #[serde(flatten)]
        params: ThermalParams,
        t: f64,
    },
    Fan {
        #[serde(flatten)]
        params: FanParams,
        /// Lagged flow (PV).
        speed: f64,
    },
}

impl Plant {
    /// Validates parameters for the selected kind.
    ///
    /// # Errors
    ///
    /// Returns a human-readable validation error string.
    pub fn validate_params(&self) -> Result<(), String> {
        match self {
            Plant::FirstOrder { params, .. } => params.validate(),
            Plant::Thermal { params, .. } => params.validate(),
            Plant::Fan { params, .. } => params.validate(),
        }
    }

    /// Current process variable (measurement).
    #[must_use]
    pub fn pv(&self) -> f64 {
        match self {
            Plant::FirstOrder { x, .. } => *x,
            Plant::Thermal { t, .. } => *t,
            Plant::Fan { speed, .. } => *speed,
        }
    }

    /// Integrate one controller tick: apply `cv` for `dt` seconds.
    ///
    /// # Errors
    ///
    /// Returns an error if `dt` is not finite and positive.
    pub fn apply_cv(&mut self, cv: f64, dt: f64) -> Result<(), String> {
        if !dt.is_finite() || dt <= 0.0 {
            return Err(format!("dt must be finite and > 0, got {dt}"));
        }
        if !cv.is_finite() {
            return Err(format!("cv must be finite, got {cv}"));
        }
        match self {
            Plant::FirstOrder { params, x } => {
                *x = crate::plant::step_first_order(*x, cv, dt, params);
            }
            Plant::Thermal { params, t } => {
                *t = crate::plant::step_thermal(*t, cv, dt, params);
            }
            Plant::Fan { params, speed } => {
                *speed = crate::plant::step_fan(*speed, cv, dt, params);
            }
        }
        Ok(())
    }
}

impl SimState {
    /// # Errors
    ///
    /// Returns an error if the schema version is unsupported or plant parameters are invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported schema_version {} (this binary understands {})",
                self.schema_version, SCHEMA_VERSION
            ));
        }
        self.plant.validate_params()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::{FanParams, FirstOrderParams, ThermalParams};

    #[test]
    fn serde_roundtrip_each_plant_kind() {
        let cases = [
            SimState {
                schema_version: SCHEMA_VERSION,
                plant: Plant::FirstOrder {
                    params: FirstOrderParams { tau: 2.0, gain: 3.0 },
                    x: 1.5,
                },
            },
            SimState {
                schema_version: SCHEMA_VERSION,
                plant: Plant::Thermal {
                    params: ThermalParams {
                        tau: 30.0,
                        t_ambient: 21.0,
                        k_heat: 0.02,
                    },
                    t: 22.0,
                },
            },
            SimState {
                schema_version: SCHEMA_VERSION,
                plant: Plant::Fan {
                    params: FanParams {
                        tau: 1.0,
                        max_flow: 80.0,
                        cv_max: 50.0,
                        exponent: 1.2,
                    },
                    speed: 10.0,
                },
            },
        ];
        for original in cases {
            let json = serde_json::to_string(&original).expect("serialize");
            let back: SimState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, original);
            back.validate().expect("valid");
        }
    }
}
