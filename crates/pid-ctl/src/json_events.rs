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

/// Generates a serialisable event struct with the common envelope fields
/// (`schema_version`, `ts`, `event`) prepended, plus caller-supplied payload fields.
///
/// Also generates a free `emit_*` function that fills the envelope and calls `emit_line`.
///
/// Syntax:
/// ```text
/// event_struct! {
///     StructName "event_name" emit_fn_name {
///         field: Type,           // payload fields
///         field: Type => expr,   // payload field with default from extra arg
///     }
/// }
/// ```
macro_rules! event_struct {
    // Base case: struct with zero payload fields, emit fn with no extra args.
    ($name:ident $event:literal $emit:ident {}) => {
        #[derive(Serialize)]
        pub struct $name {
            pub schema_version: u64,
            pub ts: String,
            pub event: &'static str,
        }

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self {
                    schema_version: STATE_SCHEMA_VERSION,
                    ts: now_iso8601(),
                    event: $event,
                }
            }
        }

        pub fn $emit(log: &mut Option<std::fs::File>) {
            emit_line(log, &$name::new());
        }
    };

    // General case: struct with payload fields. The emit fn takes the same fields as args.
    ($name:ident $event:literal $emit:ident {
        $( $field:ident : $ty:ty ),+ $(,)?
    }) => {
        #[derive(Serialize)]
        pub struct $name {
            pub schema_version: u64,
            pub ts: String,
            pub event: &'static str,
            $( pub $field: $ty, )+
        }

        impl $name {
            #[must_use]
            pub fn new($( $field: $ty ),+) -> Self {
                Self {
                    schema_version: STATE_SCHEMA_VERSION,
                    ts: now_iso8601(),
                    event: $event,
                    $( $field, )+
                }
            }
        }

        pub fn $emit(log: &mut Option<std::fs::File>, $( $field: $ty ),+) {
            emit_line(log, &$name::new($( $field ),+));
        }
    };
}

// ---------------------------------------------------------------------------
// Event definitions
// ---------------------------------------------------------------------------

event_struct! { DtSkippedEvent "dt_skipped" emit_dt_skipped {
    raw_dt: f64,
    min_dt: f64,
    max_dt: f64,
}}

event_struct! { DtClampedEvent "dt_clamped" emit_dt_clamped {
    raw_dt: f64,
    clamped_dt: f64,
}}

event_struct! { IntervalSlipEvent "interval_slip" emit_interval_slip {
    interval_ms: u64,
    actual_ms: u64,
}}

event_struct! { CvWriteFailedEvent "cv_write_failed" emit_cv_write_failed {
    error: String,
    consecutive_failures: u32,
}}

event_struct! { StateWriteFailedEvent "state_write_failed" emit_state_write_failed {
    path: PathBuf,
    error: String,
}}

event_struct! { StateWriteEscalatedEvent "state_write_escalated" emit_state_write_escalated {
    path: PathBuf,
    error: String,
    consecutive_failures: u32,
}}

event_struct! { PvFailAfterReachedEvent "pv_fail_after_reached" emit_pv_fail_after_reached {
    consecutive_failures: u32,
    limit: u32,
}}

event_struct! { GainsChangedEvent "gains_changed" emit_gains_changed {
    kp: f64,
    ki: f64,
    kd: f64,
    sp: f64,
    iter: u64,
    source: &'static str,
}}

event_struct! { GainsSavedEvent "gains_saved" emit_gains_saved {
    iter: u64,
}}

event_struct! { IntegralResetEvent "integral_reset" emit_integral_reset {
    i_acc_before: f64,
    iter: u64,
    source: &'static str,
}}

event_struct! { SocketReadyEvent "socket_ready" emit_socket_ready {
    path: PathBuf,
}}

// ---------------------------------------------------------------------------
// Events with non-standard fields (not a clean fit for the macro)
// ---------------------------------------------------------------------------

/// PV read failure — has an optional `safe_cv` field (skipped when `None`).
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

pub fn emit_pv_read_failure(
    log: &mut Option<std::fs::File>,
    error: impl Into<String>,
    safe_cv: Option<f64>,
) {
    emit_line(log, &PvReadFailureEvent::new(error, safe_cv));
}

/// D-term skipped event — payload uses `&'static str` reason derived from enum variant.
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
    fn socket_ready_event_fields() {
        let mut log = Some(tempfile::tempfile().unwrap());
        emit_socket_ready(&mut log, std::path::PathBuf::from("/tmp/ctl.sock"));
        let mut f = log.unwrap();
        let mut s = String::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_string(&mut s).unwrap();
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(v["event"], "socket_ready");
        assert_eq!(v["path"], "/tmp/ctl.sock");
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

    #[test]
    fn macro_generated_events_have_correct_envelope() {
        let mut log = Some(tempfile::tempfile().unwrap());
        emit_dt_skipped(&mut log, 0.6, 0.1, 0.5);
        let mut f = log.unwrap();
        let mut s = String::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_string(&mut s).unwrap();
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(v["event"], "dt_skipped");
        assert!(v["schema_version"].is_number());
        assert!(v["ts"].is_string());
        assert_eq!(v["raw_dt"], 0.6);
        assert_eq!(v["min_dt"], 0.1);
        assert_eq!(v["max_dt"], 0.5);
    }

    #[test]
    fn pv_read_failure_safe_cv_skipped_when_none() {
        let mut log = Some(tempfile::tempfile().unwrap());
        emit_pv_read_failure(&mut log, "timeout", None);
        let mut f = log.unwrap();
        let mut s = String::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_string(&mut s).unwrap();
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(v["event"], "pv_read_failure");
        assert!(v.get("safe_cv").is_none(), "safe_cv should be absent when None");
    }
}
