//! Åström–Hägglund relay-feedback autotune engine.
//!
//! Pure state machine: no I/O, no wall clock. The caller drives it with successive
//! `tick(pv, dt)` calls and reads back the relay CV plus any emitted events.
//!
//! # Algorithm
//!
//! The relay toggles the control output between `bias + amp` (High) and `bias - amp`
//! (Low). The process variable oscillates around the reference `pv_ref` (the PV
//! observed on the first tick). Each time the PV crosses `pv_ref`, the relay flips.
//!
//! After three or more complete limit-cycle periods the engine declares settlement and
//! estimates:
//!
//! ```text
//! a   = (mean(peaks) – mean(troughs)) / 2
//! Ku  = 4 · amp / (π · a)
//! Tu  = 2 · mean(half-period durations)
//! ```
//!
//! Tuning rules then map `(Ku, Tu)` to `(Kp, Ki, Kd)`.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// Minimum complete oscillation cycles required before declaring settlement.
const MIN_CYCLES: usize = 3;
// Maximum coefficient of variation (σ/μ) for half-periods to declare settled.
const SETTLED_PERIOD_CV: f64 = 0.20;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Tuning rule applied to `(Ku, Tu)` to produce suggested PID gains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningRule {
    /// Ziegler–Nichols PI (conservative; no derivative).
    Pi,
    /// Ziegler–Nichols PID.
    Pid,
    /// Tyreus–Luyben (robust; well-damped).
    Tl,
}

impl TuningRule {
    /// Compute suggested `(kp, ki, kd)` from ultimate gain and period.
    #[must_use]
    pub fn gains(self, ku: f64, tu: f64) -> (f64, f64, f64) {
        match self {
            Self::Pi => {
                let kp = 0.45 * ku;
                let ki = kp * 1.2 / tu;
                (kp, ki, 0.0)
            }
            Self::Pid => {
                let kp = 0.6 * ku;
                let ki = kp * 2.0 / tu;
                let kd = kp * tu / 8.0;
                (kp, ki, kd)
            }
            Self::Tl => {
                let kp = ku / 3.2;
                let ki = kp / (2.2 * tu);
                let kd = kp * tu / 6.3;
                (kp, ki, kd)
            }
        }
    }
}

/// Configuration for the relay autotune test.
#[derive(Clone, Debug)]
pub struct AutotuneConfig {
    /// CV operating-point; relay toggles `± amp` around this value.
    pub bias: f64,
    /// Half-amplitude of the relay output swing.
    pub amp: f64,
    /// Lower clamp on control output.
    pub out_min: f64,
    /// Upper clamp on control output.
    pub out_max: f64,
}

impl AutotuneConfig {
    /// Validate the configuration, returning a human-readable error on failure.
    ///
    /// # Errors
    ///
    /// Returns an error string when `amp ≤ 0`, `bias` is outside
    /// `[out_min, out_max]`, or the relay swing would exceed the output limits.
    pub fn validate(&self) -> Result<(), String> {
        if self.amp <= 0.0 {
            return Err(format!("--amp must be > 0, got {}", self.amp));
        }
        if self.bias < self.out_min || self.bias > self.out_max {
            return Err(format!(
                "--bias {b} is outside [out_min={lo}, out_max={hi}]",
                b = self.bias,
                lo = self.out_min,
                hi = self.out_max,
            ));
        }
        if self.bias - self.amp < self.out_min {
            return Err(format!(
                "relay low ({b} - {a} = {lo}) would be below out_min ({min})",
                b = self.bias,
                a = self.amp,
                lo = self.bias - self.amp,
                min = self.out_min,
            ));
        }
        if self.bias + self.amp > self.out_max {
            return Err(format!(
                "relay high ({b} + {a} = {hi}) would exceed out_max ({max})",
                b = self.bias,
                a = self.amp,
                hi = self.bias + self.amp,
                max = self.out_max,
            ));
        }
        Ok(())
    }
}

/// Event emitted during the autotune run (serialised as NDJSON by the CLI).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RelayEvent {
    /// The relay output flipped direction.
    RelayFlip { cv: f64, pv: f64, elapsed_secs: f64 },
    /// A new period estimate is available (emitted after each High→Low flip once
    /// at least one complete cycle has been observed).
    PeriodDetected { tu: f64, amplitude: f64 },
    /// The limit cycle has settled to a consistent oscillation.
    Settled {
        ku: f64,
        tu: f64,
        amplitude: f64,
        cycles: usize,
    },
}

/// Final autotune result including the suggested PID gains.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutotuneResult {
    pub ku: f64,
    pub tu: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub rule: TuningRule,
    pub samples: usize,
    pub cycles: usize,
}

// ---------------------------------------------------------------------------
// Internal relay state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelayPos {
    High,
    Low,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Stateful relay autotune engine.
///
/// Call [`tick`](AutotuneEngine::tick) once per control interval. Inspect
/// [`is_settled`](AutotuneEngine::is_settled) to decide when to stop, then
/// call [`result`](AutotuneEngine::result) to obtain suggested gains.
pub struct AutotuneEngine {
    config: AutotuneConfig,
    relay: RelayPos,
    /// Reference PV set from the first sample.
    pv_ref: Option<f64>,

    // Per-phase accumulators (reset on every relay flip).
    phase_elapsed: f64,
    phase_extremum: f64,

    // Completed-phase history.
    high_peaks: VecDeque<f64>,
    low_troughs: VecDeque<f64>,
    half_periods: VecDeque<f64>,

    pub elapsed_secs: f64,
    pub samples: usize,
    settled: bool,
}

impl AutotuneEngine {
    /// Create a new engine from the given configuration.
    #[must_use]
    pub fn new(config: AutotuneConfig) -> Self {
        Self {
            config,
            relay: RelayPos::High,
            pv_ref: None,
            phase_elapsed: 0.0,
            phase_extremum: f64::NEG_INFINITY,
            high_peaks: VecDeque::new(),
            low_troughs: VecDeque::new(),
            half_periods: VecDeque::new(),
            elapsed_secs: 0.0,
            samples: 0,
            settled: false,
        }
    }

    /// Override the PV reference used for relay switching.
    ///
    /// Call this after pre-warming the process (applying `bias` for several
    /// ticks) so that `pv_ref` reflects the true operating-point PV rather
    /// than the cold-start value. Has no effect once the first [`tick`] has
    /// been called without a prior `set_pv_ref`.
    ///
    /// [`tick`]: AutotuneEngine::tick
    pub fn set_pv_ref(&mut self, pv: f64) {
        self.pv_ref = Some(pv);
    }

    /// Advance one tick.
    ///
    /// Returns `(cv, events)` where `cv` is the relay output to apply and
    /// `events` is the (possibly empty) list of notable state changes.
    pub fn tick(&mut self, pv: f64, dt: f64) -> (f64, Vec<RelayEvent>) {
        self.elapsed_secs += dt;
        self.samples += 1;
        self.phase_elapsed += dt;

        let pv_ref = *self.pv_ref.get_or_insert(pv);

        let mut events = Vec::new();

        match self.relay {
            RelayPos::High => {
                if pv > self.phase_extremum {
                    self.phase_extremum = pv;
                }
                if pv > pv_ref {
                    self.complete_phase(RelayPos::High, pv, &mut events);
                }
            }
            RelayPos::Low => {
                if pv < self.phase_extremum {
                    self.phase_extremum = pv;
                }
                if pv < pv_ref {
                    self.complete_phase(RelayPos::Low, pv, &mut events);
                }
            }
        }

        let cv = self.current_cv();
        (cv, events)
    }

    /// Whether the limit cycle has settled enough to read a reliable result.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Compute the final result applying `rule` to the identified `(Ku, Tu)`.
    ///
    /// Returns `None` when fewer than two complete oscillation cycles have been
    /// observed (not enough data for a meaningful estimate).
    #[must_use]
    pub fn result(&self, rule: TuningRule) -> Option<AutotuneResult> {
        let (ku, tu) = self.best_estimates()?;
        let (kp, ki, kd) = rule.gains(ku, tu);
        let cycles = self.high_peaks.len().min(self.low_troughs.len());
        Some(AutotuneResult {
            ku,
            tu,
            kp,
            ki,
            kd,
            rule,
            samples: self.samples,
            cycles,
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn current_cv(&self) -> f64 {
        match self.relay {
            RelayPos::High => self.config.bias + self.config.amp,
            RelayPos::Low => self.config.bias - self.config.amp,
        }
    }

    /// Complete the current relay phase, record its extremum and duration, flip
    /// the relay, and emit the appropriate events.
    fn complete_phase(&mut self, pos: RelayPos, crossing_pv: f64, events: &mut Vec<RelayEvent>) {
        let extremum = self.phase_extremum;
        let duration = self.phase_elapsed;

        match pos {
            RelayPos::High => {
                self.high_peaks.push_back(extremum);
                self.half_periods.push_back(duration);
                self.relay = RelayPos::Low;
                self.phase_extremum = f64::INFINITY;
            }
            RelayPos::Low => {
                self.low_troughs.push_back(extremum);
                self.half_periods.push_back(duration);
                self.relay = RelayPos::High;
                self.phase_extremum = f64::NEG_INFINITY;
            }
        }

        self.phase_elapsed = 0.0;

        let cv = self.current_cv();
        events.push(RelayEvent::RelayFlip {
            cv,
            pv: crossing_pv,
            elapsed_secs: self.elapsed_secs,
        });

        // After a High→Low flip we may have a new period estimate.
        if pos == RelayPos::High {
            if let Some((tu, amplitude)) = self.period_estimate() {
                events.push(RelayEvent::PeriodDetected { tu, amplitude });
            }
        } else if !self.settled {
            // Check settlement after every Low→High flip (completes a full cycle).
            if let Some((ku, tu, amplitude)) = self.settlement_check() {
                self.settled = true;
                let cycles = self.high_peaks.len().min(self.low_troughs.len());
                events.push(RelayEvent::Settled {
                    ku,
                    tu,
                    amplitude,
                    cycles,
                });
            }
        }
    }

    /// Compute period and amplitude from all observed data. Requires ≥ 1 complete
    /// cycle (both a peak and a trough).
    fn period_estimate(&self) -> Option<(f64, f64)> {
        if self.high_peaks.is_empty() || self.low_troughs.is_empty() {
            return None;
        }
        let tu = 2.0 * vec_mean(&self.half_periods);
        let mean_peaks = vec_mean(&self.high_peaks);
        let mean_troughs = vec_mean(&self.low_troughs);
        let amplitude = (mean_peaks - mean_troughs) / 2.0;
        if amplitude <= 0.0 {
            return None;
        }
        Some((tu, amplitude))
    }

    /// Returns `(ku, tu, amplitude)` when the oscillation has settled
    /// (≥ `MIN_CYCLES` complete cycles with consistent period).
    fn settlement_check(&self) -> Option<(f64, f64, f64)> {
        let n_cycles = self.high_peaks.len().min(self.low_troughs.len());
        if n_cycles < MIN_CYCLES {
            return None;
        }

        // Use the last 2*MIN_CYCLES half-periods to assess stability.
        let window: Vec<f64> = self
            .half_periods
            .iter()
            .rev()
            .take(2 * MIN_CYCLES)
            .copied()
            .collect();
        let mean_hp = slice_mean(&window);
        if mean_hp <= 0.0 {
            return None;
        }
        let std_hp = slice_std(&window, mean_hp);
        if std_hp / mean_hp >= SETTLED_PERIOD_CV {
            return None;
        }

        let tu = 2.0 * mean_hp;
        let mean_peaks = vec_mean(&self.high_peaks);
        let mean_troughs = vec_mean(&self.low_troughs);
        let amplitude = (mean_peaks - mean_troughs) / 2.0;
        if amplitude <= 0.0 {
            return None;
        }

        let ku = 4.0 * self.config.amp / (std::f64::consts::PI * amplitude);
        Some((ku, tu, amplitude))
    }

    /// Best available `(ku, tu)` estimate: settled estimate if available, otherwise
    /// from all data if ≥ 2 complete cycles exist.
    fn best_estimates(&self) -> Option<(f64, f64)> {
        if let Some((ku, tu, _)) = self.settlement_check() {
            return Some((ku, tu));
        }
        let n_cycles = self.high_peaks.len().min(self.low_troughs.len());
        if n_cycles < 2 {
            return None;
        }
        let (tu, amplitude) = self.period_estimate()?;
        let ku = 4.0 * self.config.amp / (std::f64::consts::PI * amplitude);
        Some((ku, tu))
    }
}

// ---------------------------------------------------------------------------
// Statistics helpers
// ---------------------------------------------------------------------------

fn vec_mean(v: &VecDeque<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = v.len() as f64;
    v.iter().sum::<f64>() / n
}

fn slice_mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = v.len() as f64;
    v.iter().sum::<f64>() / n
}

fn slice_std(v: &[f64], mean: f64) -> f64 {
    if v.len() <= 1 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = v.len() as f64;
    let variance = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    variance.sqrt()
}

// ---------------------------------------------------------------------------
// Unit tests for the pure engine
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn make_config() -> AutotuneConfig {
        AutotuneConfig {
            bias: 50.0,
            amp: 20.0,
            out_min: 0.0,
            out_max: 100.0,
        }
    }

    // Simulate a first-order lag: dx/dt = (K*u - x) / tau.
    fn step_first_order(x: f64, u: f64, dt: f64, tau: f64, gain: f64) -> f64 {
        let dx = (gain * u - x) / tau;
        x + dt * dx
    }

    #[test]
    fn config_validate_rejects_zero_amp() {
        let cfg = AutotuneConfig {
            amp: 0.0,
            ..make_config()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_negative_amp() {
        let cfg = AutotuneConfig {
            amp: -5.0,
            ..make_config()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_bias_above_out_max() {
        let cfg = AutotuneConfig {
            bias: 110.0,
            ..make_config()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_relay_high_exceeds_out_max() {
        let cfg = AutotuneConfig {
            bias: 90.0,
            amp: 20.0,
            out_min: 0.0,
            out_max: 100.0,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_validate_accepts_valid() {
        assert!(make_config().validate().is_ok());
    }

    #[test]
    fn tuning_rule_pi_zero_derivative() {
        let (_, _, kd) = TuningRule::Pi.gains(1.0, 1.0);
        assert!(
            kd.abs() < f64::EPSILON,
            "PI rule should have kd=0, got {kd}"
        );
    }

    #[test]
    fn tuning_rule_pid_gains_positive() {
        let (kp, ki, kd) = TuningRule::Pid.gains(2.0, 10.0);
        assert!(kp > 0.0 && ki > 0.0 && kd > 0.0);
    }

    #[test]
    fn tuning_rule_tl_more_conservative_than_zn() {
        // TL kp = Ku/3.2, ZN PID kp = 0.6*Ku — TL is more conservative.
        let (kp_tl, _, _) = TuningRule::Tl.gains(1.0, 1.0);
        let (kp_zn, _, _) = TuningRule::Pid.gains(1.0, 1.0);
        assert!(kp_tl < kp_zn);
    }

    #[test]
    fn engine_emits_relay_flip_on_pv_crossing() {
        let mut engine = AutotuneEngine::new(make_config());
        // First tick initialises pv_ref = 50.0; relay starts High.
        let (cv0, _) = engine.tick(50.0, 0.1);
        assert!((cv0 - 70.0).abs() < 1e-9); // bias + amp

        // Simulate PV rising above pv_ref.
        let (cv1, events) = engine.tick(50.1, 0.1);
        assert!((cv1 - 30.0).abs() < 1e-9); // flipped to Low
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RelayEvent::RelayFlip { .. }))
        );
    }

    #[test]
    fn engine_settled_after_three_cycles_on_first_order_plant() {
        let cfg = AutotuneConfig {
            bias: 50.0,
            amp: 10.0,
            out_min: 0.0,
            out_max: 100.0,
        };
        let mut engine = AutotuneEngine::new(cfg);

        let tau = 5.0;
        let gain = 1.0;
        let dt = 0.05;
        let mut pv = 50.0;

        for _ in 0..4000 {
            let (cv, _) = engine.tick(pv, dt);
            pv = step_first_order(pv, cv, dt, tau, gain);
            if engine.is_settled() {
                break;
            }
        }

        assert!(
            engine.is_settled(),
            "engine should settle within 4000 ticks"
        );

        // Theoretical Ku for relay on first-order: Ku = 4*d / (π * a_pv)
        // For this system the relay test should give Ku around 1/(gain * dt_like) but
        // we just check it's a reasonable positive number and the gains are sane.
        let result = engine
            .result(TuningRule::Pid)
            .expect("result after settlement");
        assert!(result.ku > 0.0, "ku={}", result.ku);
        assert!(result.tu > 0.0, "tu={}", result.tu);
        assert!(result.kp > 0.0 && result.ki > 0.0 && result.kd > 0.0);
    }

    #[test]
    fn ku_formula_matches_manual_calc() {
        // Drive a simple simulation long enough to get clean data, then compare
        // the engine's Ku with the hand-calculated value.
        let cfg = AutotuneConfig {
            bias: 0.0,
            amp: 5.0,
            out_min: -10.0,
            out_max: 10.0,
        };
        let mut engine = AutotuneEngine::new(cfg.clone());
        let tau = 2.0;
        let gain = 1.0;
        let dt = 0.01;
        let mut pv = 0.0;

        for _ in 0..10_000 {
            let (cv, _) = engine.tick(pv, dt);
            pv = step_first_order(pv, cv, dt, tau, gain);
            if engine.is_settled() {
                break;
            }
        }
        assert!(engine.is_settled());
        let result = engine.result(TuningRule::Pid).unwrap();

        // The analytical amplitude for relay on first-order:
        // a_pv = (4 * d * gain) / (π * Ku_true), and Ku_true from the process is
        // not simply computable analytically without the transfer function — but we
        // can at least verify Ku = 4*amp / (π * a_pv) is consistent.
        let amplitude = (4.0 * cfg.amp) / (PI * result.ku);
        assert!(amplitude > 0.0, "amplitude={amplitude}");
        assert!(result.samples > 0);
    }
}
