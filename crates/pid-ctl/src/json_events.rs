//! Structured NDJSON event lines (stderr + optional `--log` file).
//!
//! [`suppress_structured_json_stderr`] disables stderr for `emit_*` only (not other `eprintln!`),
//! so `loop --tune` can use an alternate-screen TUI on stdout without JSON lines corrupting the
//! display — events still append to `--log` when set.

use crate::app::{STATE_SCHEMA_VERSION, now_iso8601};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

static SUPPRESS_STRUCTURED_JSON_STDERR: AtomicBool = AtomicBool::new(false);

fn emit_line(log: &mut Option<std::fs::File>, record: &impl Serialize) {
    let Ok(json) = serde_json::to_string(record) else {
        return;
    };
    if !SUPPRESS_STRUCTURED_JSON_STDERR.load(Ordering::Relaxed) {
        eprintln!("{json}");
    }
    if let Some(f) = log {
        let _ = writeln!(f, "{json}");
    }
}

/// While held, structured NDJSON events are written to `--log` only, not stderr.
///
/// Used by `loop --tune` so `emit_*` lines do not interleave with the ratatui display (stderr is not
/// in the alternate screen buffer).
#[must_use]
pub fn suppress_structured_json_stderr() -> StructuredJsonStderrGuard {
    SUPPRESS_STRUCTURED_JSON_STDERR.store(true, Ordering::Relaxed);
    StructuredJsonStderrGuard
}

pub struct StructuredJsonStderrGuard;

impl Drop for StructuredJsonStderrGuard {
    fn drop(&mut self) {
        SUPPRESS_STRUCTURED_JSON_STDERR.store(false, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
pub struct DtSkippedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub raw_dt: f64,
    pub min_dt: f64,
    pub max_dt: f64,
}

impl DtSkippedEvent {
    #[must_use]
    pub fn new(raw_dt: f64, min_dt: f64, max_dt: f64) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "dt_skipped",
            raw_dt,
            min_dt,
            max_dt,
        }
    }
}

#[derive(Serialize)]
pub struct DtClampedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub raw_dt: f64,
    pub clamped_dt: f64,
}

impl DtClampedEvent {
    #[must_use]
    pub fn new(raw_dt: f64, clamped_dt: f64) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "dt_clamped",
            raw_dt,
            clamped_dt,
        }
    }
}

#[derive(Serialize)]
pub struct IntervalSlipEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub interval_ms: u64,
    pub actual_ms: u64,
}

impl IntervalSlipEvent {
    #[must_use]
    pub fn new(interval_ms: u64, actual_ms: u64) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "interval_slip",
            interval_ms,
            actual_ms,
        }
    }
}

#[derive(Serialize)]
pub struct PvReadFailureEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_cv: Option<f64>,
}

impl PvReadFailureEvent {
    #[must_use]
    pub fn new(error: impl Into<String>, safe_cv: Option<f64>) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "pv_read_failure",
            error: error.into(),
            safe_cv,
        }
    }
}

#[derive(Serialize)]
pub struct CvWriteFailedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub error: String,
    pub consecutive_failures: u32,
}

impl CvWriteFailedEvent {
    #[must_use]
    pub fn new(error: impl Into<String>, consecutive_failures: u32) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "cv_write_failed",
            error: error.into(),
            consecutive_failures,
        }
    }
}

#[derive(Serialize)]
pub struct StateWriteFailedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub path: PathBuf,
    pub error: String,
}

impl StateWriteFailedEvent {
    #[must_use]
    pub fn new(path: PathBuf, error: impl Into<String>) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "state_write_failed",
            path,
            error: error.into(),
        }
    }
}

pub fn emit_dt_skipped(log: &mut Option<std::fs::File>, raw_dt: f64, min_dt: f64, max_dt: f64) {
    emit_line(log, &DtSkippedEvent::new(raw_dt, min_dt, max_dt));
}

pub fn emit_dt_clamped(log: &mut Option<std::fs::File>, raw_dt: f64, clamped_dt: f64) {
    emit_line(log, &DtClampedEvent::new(raw_dt, clamped_dt));
}

pub fn emit_interval_slip(log: &mut Option<std::fs::File>, interval_ms: u64, actual_ms: u64) {
    emit_line(log, &IntervalSlipEvent::new(interval_ms, actual_ms));
}

pub fn emit_pv_read_failure(
    log: &mut Option<std::fs::File>,
    error: impl Into<String>,
    safe_cv: Option<f64>,
) {
    emit_line(log, &PvReadFailureEvent::new(error, safe_cv));
}

pub fn emit_cv_write_failed(
    log: &mut Option<std::fs::File>,
    error: impl Into<String>,
    consecutive_failures: u32,
) {
    emit_line(log, &CvWriteFailedEvent::new(error, consecutive_failures));
}

pub fn emit_state_write_failed(
    log: &mut Option<std::fs::File>,
    path: PathBuf,
    error: impl Into<String>,
) {
    emit_line(log, &StateWriteFailedEvent::new(path, error));
}

#[derive(Serialize)]
pub struct DTermSkippedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub reason: &'static str,
    pub iter: u64,
}

impl DTermSkippedEvent {
    #[must_use]
    pub fn new(reason: &'static str, iter: u64) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "d_term_skipped",
            reason,
            iter,
        }
    }
}

/// Converts a `DTermSkipReason` to its plan-specified string representation.
#[must_use]
pub const fn reason_str(reason: pid_ctl_core::DTermSkipReason) -> &'static str {
    match reason {
        pid_ctl_core::DTermSkipReason::NoPvPrev => "no_pv_prev",
        pid_ctl_core::DTermSkipReason::PostDtSkip => "post_dt_skip",
        pid_ctl_core::DTermSkipReason::PostReset => "post_reset",
    }
}

pub fn emit_d_term_skipped(
    log: &mut Option<std::fs::File>,
    reason: pid_ctl_core::DTermSkipReason,
    iter: u64,
) {
    emit_line(log, &DTermSkippedEvent::new(reason_str(reason), iter));
}

#[derive(Serialize)]
pub struct PvFailAfterReachedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub consecutive_failures: u32,
    pub limit: u32,
}

impl PvFailAfterReachedEvent {
    #[must_use]
    pub fn new(consecutive_failures: u32, limit: u32) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "pv_fail_after_reached",
            consecutive_failures,
            limit,
        }
    }
}

pub fn emit_pv_fail_after_reached(
    log: &mut Option<std::fs::File>,
    consecutive_failures: u32,
    limit: u32,
) {
    emit_line(
        log,
        &PvFailAfterReachedEvent::new(consecutive_failures, limit),
    );
}

#[derive(Serialize)]
pub struct StateWriteEscalatedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub path: PathBuf,
    pub error: String,
    pub consecutive_failures: u32,
}

impl StateWriteEscalatedEvent {
    #[must_use]
    pub fn new(path: PathBuf, error: impl Into<String>, consecutive_failures: u32) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "state_write_escalated",
            path,
            error: error.into(),
            consecutive_failures,
        }
    }
}

pub fn emit_state_write_escalated(
    log: &mut Option<std::fs::File>,
    path: PathBuf,
    error: impl Into<String>,
    consecutive_failures: u32,
) {
    emit_line(
        log,
        &StateWriteEscalatedEvent::new(path, error, consecutive_failures),
    );
}

#[derive(Serialize)]
pub struct GainsChangedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub sp: f64,
    pub iter: u64,
    pub source: &'static str,
}

impl GainsChangedEvent {
    #[must_use]
    pub fn new(kp: f64, ki: f64, kd: f64, sp: f64, iter: u64, source: &'static str) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "gains_changed",
            kp,
            ki,
            kd,
            sp,
            iter,
            source,
        }
    }
}

pub fn emit_gains_changed(
    log: &mut Option<std::fs::File>,
    kp: f64,
    ki: f64,
    kd: f64,
    sp: f64,
    iter: u64,
    source: &'static str,
) {
    emit_line(log, &GainsChangedEvent::new(kp, ki, kd, sp, iter, source));
}

#[derive(Serialize)]
pub struct GainsSavedEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub iter: u64,
}

impl GainsSavedEvent {
    #[must_use]
    pub fn new(iter: u64) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "gains_saved",
            iter,
        }
    }
}

pub fn emit_gains_saved(log: &mut Option<std::fs::File>, iter: u64) {
    emit_line(log, &GainsSavedEvent::new(iter));
}

#[derive(Serialize)]
pub struct IntegralResetEvent {
    pub schema_version: u64,
    pub ts: String,
    pub event: &'static str,
    pub i_acc_before: f64,
    pub iter: u64,
    pub source: &'static str,
}

impl IntegralResetEvent {
    #[must_use]
    pub fn new(i_acc_before: f64, iter: u64, source: &'static str) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            ts: now_iso8601(),
            event: "integral_reset",
            i_acc_before,
            iter,
            source,
        }
    }
}

pub fn emit_integral_reset(
    log: &mut Option<std::fs::File>,
    i_acc_before: f64,
    iter: u64,
    source: &'static str,
) {
    emit_line(log, &IntegralResetEvent::new(i_acc_before, iter, source));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn suppress_guard_still_appends_to_log() {
        let mut log = Some(tempfile::tempfile().unwrap());
        let _guard = suppress_structured_json_stderr();
        emit_gains_changed(&mut log, 1.0, 0.0, 0.0, 1.0, 1, "tui");
        let mut f = log.unwrap();
        let mut s = String::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_string(&mut s).unwrap();
        assert!(s.contains("\"event\":\"gains_changed\""), "{s:?}");
    }

    #[test]
    fn integral_reset_event_fields() {
        let mut log = Some(tempfile::tempfile().unwrap());
        emit_integral_reset(&mut log, 42.5, 7, "socket");
        let mut f = log.unwrap();
        let mut s = String::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_string(&mut s).unwrap();
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(v["event"], "integral_reset");
        assert_eq!(v["i_acc_before"], 42.5);
        assert_eq!(v["iter"], 7);
        assert_eq!(v["source"], "socket");
        // Must NOT contain gains_changed fields
        assert!(v.get("kp").is_none(), "kp should not be in integral_reset event");
    }
}
