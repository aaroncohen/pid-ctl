//! Shared tick-pipeline helpers: dt validation, safe-CV writes, log helpers, state-write failure
//! reporting, and the [`LoopControls`] abstraction used by both the main loop and socket dispatch.

use crate::adapters::CvSink;
use crate::app::logger::Logger;
use crate::app::{ControllerSession, StateStoreError};
use crate::json_events;
use std::path::Path;
use std::time::Duration;

pub enum MeasuredDt {
    Skip,
    Use(f64),
}

/// Trait over the mutable loop-configuration state that socket commands and the interval command
/// need to inspect and update without depending on the concrete [`super::super::cli::types::LoopArgs`] type.
pub trait LoopControls {
    fn interval(&self) -> Duration;
    fn set_interval(&mut self, d: Duration);
    fn max_dt(&self) -> f64;
    fn set_max_dt_unless_explicit(&mut self, v: f64);
    fn pv_stdin_timeout(&self) -> Duration;
    fn set_pv_stdin_timeout_unless_explicit(&mut self, d: Duration);
    fn state_write_interval(&self) -> Option<Duration>;
    fn set_state_write_interval_unless_explicit(&mut self, d: Option<Duration>);
}

/// Applies a new loop interval at runtime, updating derived defaults (`max_dt`,
/// `pv_stdin_timeout`, `state_write_interval`) unless the user set them explicitly.
pub fn apply_runtime_interval(
    session: &mut ControllerSession,
    controls: &mut dyn LoopControls,
    new_interval: Duration,
) {
    controls.set_interval(new_interval);
    let s = new_interval.as_secs_f64();
    controls.set_max_dt_unless_explicit((s * 3.0_f64).clamp(0.01, 60.0));
    controls.set_pv_stdin_timeout_unless_explicit(new_interval);
    let min_flush = Duration::from_millis(100);
    controls.set_state_write_interval_unless_explicit(Some(new_interval.max(min_flush)));
    session.set_flush_interval(controls.state_write_interval());
}

#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn millis_round_u64(ms: f64) -> u64 {
    let v = ms.round();
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    if v >= u64::MAX as f64 {
        return u64::MAX;
    }
    v as u64
}

/// Applies `--min-dt` / `--max-dt` for measured `dt` in `loop`: skip (default) or clamp (`--dt-clamp`).
pub fn apply_measured_dt(
    raw_dt: f64,
    min_dt: f64,
    max_dt: f64,
    dt_clamp: bool,
    quiet: bool,
    logger: &mut Logger,
) -> MeasuredDt {
    if raw_dt >= min_dt && raw_dt <= max_dt {
        return MeasuredDt::Use(raw_dt);
    }

    if dt_clamp {
        let clamped = raw_dt.clamp(min_dt, max_dt);
        if !quiet {
            if raw_dt < min_dt {
                eprintln!("dt {raw_dt:.6}s below min_dt {min_dt:.6}s — clamping to min_dt");
            } else {
                eprintln!("dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — clamping to max_dt");
            }
        }
        json_events::emit_dt_clamped(logger, raw_dt, clamped);
        MeasuredDt::Use(clamped)
    } else {
        if !quiet {
            if raw_dt < min_dt {
                eprintln!("dt {raw_dt:.6}s below min_dt {min_dt:.6}s — skipping tick");
            } else {
                eprintln!("dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — skipping tick");
            }
        }
        json_events::emit_dt_skipped(logger, raw_dt, min_dt, max_dt);
        MeasuredDt::Skip
    }
}

/// Writes the safe CV when configured; on success, records it as the last confirmed-applied CV.
pub fn write_safe_cv(
    safe_cv: Option<f64>,
    cv_sink: &mut dyn CvSink,
    session: &mut ControllerSession,
) {
    if let Some(cv) = safe_cv
        && cv_sink.write_cv(cv).is_ok()
        && let Some(err) = session.record_confirmed_cv(cv)
    {
        eprintln!("state write failed: {err}");
    }
}

/// Handles a state write failure that occurred during a dt-skip, applying escalation logic.
pub fn handle_dt_skip_state_write(
    err: Option<StateStoreError>,
    session: &ControllerSession,
    state_path: Option<&Path>,
    logger: &mut Logger,
    quiet: bool,
) {
    let Some(err) = err else {
        return;
    };
    emit_state_write_failure(session, state_path, logger, &err, quiet);
}

/// Emits a state write failure — escalated warning if threshold reached, plain log otherwise.
pub fn emit_state_write_failure(
    session: &ControllerSession,
    state_path: Option<&Path>,
    logger: &mut Logger,
    err: &StateStoreError,
    quiet: bool,
) {
    if let Some(path) = state_path {
        if session.state_fail_escalated() {
            let count = session.state_fail_count();
            if !quiet {
                eprintln!("WARNING: state write failing persistently ({count} consecutive): {err}");
            }
            json_events::emit_state_write_escalated(
                logger,
                path.to_path_buf(),
                err.to_string(),
                count,
            );
        } else {
            if !quiet {
                eprintln!("state write failed: {err}");
            }
            json_events::emit_state_write_failed(logger, path.to_path_buf(), err.to_string());
        }
    } else if !quiet {
        eprintln!("state write failed: {err}");
    }
}

/// Forces a final state flush at loop shutdown, logging any failure.
pub fn flush_state_at_shutdown(
    session: &mut ControllerSession,
    state_path: Option<&Path>,
    logger: &mut Logger,
) {
    if let Some(err) = session.force_flush() {
        eprintln!("state write failed at shutdown: {err}");
        if let Some(path) = state_path {
            json_events::emit_state_write_failed(logger, path.to_path_buf(), err.to_string());
        }
    }
}
