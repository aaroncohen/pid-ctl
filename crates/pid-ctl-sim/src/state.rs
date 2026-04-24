use crate::SimError;
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
    /// Returns [`SimError::Validation`] when a parameter is out of range.
    pub fn validate_params(&self) -> Result<(), SimError> {
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
    /// Returns [`SimError::Validation`] if `dt` or `cv` are not finite and positive.
    pub fn apply_cv(&mut self, cv: f64, dt: f64) -> Result<(), SimError> {
        if !dt.is_finite() || dt <= 0.0 {
            return Err(SimError::Validation(format!(
                "dt must be finite and > 0, got {dt}"
            )));
        }
        if !cv.is_finite() {
            return Err(SimError::Validation(format!("cv must be finite, got {cv}")));
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
    /// Returns [`SimError::Validation`] if the schema version is unsupported or plant parameters are invalid.
    pub fn validate(&self) -> Result<(), SimError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SimError::Validation(format!(
                "unsupported schema_version {} (this binary understands {})",
                self.schema_version, SCHEMA_VERSION
            )));
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
                    params: FirstOrderParams {
                        tau: 2.0,
                        gain: 3.0,
                    },
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

    #[test]
    fn validate_returns_sim_error_validation_on_bad_params() {
        use crate::SimError;
        let bad = SimState {
            schema_version: SCHEMA_VERSION,
            plant: Plant::FirstOrder {
                params: FirstOrderParams {
                    tau: -1.0,
                    gain: 1.0,
                },
                x: 0.0,
            },
        };
        let err = bad.validate().expect_err("should fail for tau <= 0");
        assert!(
            matches!(err, SimError::Validation(_)),
            "expected SimError::Validation, got {err:?}"
        );
        assert!(
            err.to_string().contains("tau"),
            "error message should mention 'tau': {err}"
        );
    }

    #[test]
    fn validate_returns_sim_error_validation_on_wrong_schema_version() {
        use crate::SimError;
        let bad = SimState {
            schema_version: 999,
            plant: Plant::FirstOrder {
                params: FirstOrderParams {
                    tau: 1.0,
                    gain: 1.0,
                },
                x: 0.0,
            },
        };
        let err = bad
            .validate()
            .expect_err("should fail for wrong schema version");
        assert!(matches!(err, SimError::Validation(_)));
        assert!(
            err.to_string().contains("schema_version"),
            "error should mention schema_version: {err}"
        );
    }

    #[test]
    fn apply_cv_returns_sim_error_for_bad_dt() {
        use crate::SimError;
        let mut plant = Plant::Thermal {
            params: ThermalParams {
                tau: 10.0,
                t_ambient: 20.0,
                k_heat: 0.01,
            },
            t: 20.0,
        };
        let err = plant
            .apply_cv(1.0, -0.1)
            .expect_err("negative dt should fail");
        assert!(matches!(err, SimError::Validation(_)));
        assert!(
            err.to_string().contains("dt"),
            "error should mention dt: {err}"
        );
    }

    #[test]
    fn sim_error_implements_std_error() {
        use std::error::Error;
        let e = SimError::Validation(String::from("test"));
        assert!(e.source().is_none());
        assert_eq!(e.to_string(), "test");
    }
}
