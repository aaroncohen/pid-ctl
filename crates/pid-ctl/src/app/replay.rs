//! Pure log-driven replay: re-runs `PidController` against recorded PV/dt values
//! from an NDJSON log, producing a counterfactual CV stream.

use crate::app::{STATE_SCHEMA_VERSION, now_iso8601};
use pid_ctl_core::{ConfigError, PidConfig, PidController, StepInput};
use serde::Serialize;
use std::fmt;
use std::io::{self, BufRead, Write};

/// Errors that can arise during a replay run.
#[derive(Debug)]
pub enum ReplayError {
    Io(io::Error),
    Json {
        line: u64,
        source: serde_json::Error,
    },
    MissingField {
        line: u64,
        field: &'static str,
    },
    Config(ConfigError),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Json { line, source } => write!(f, "line {line}: invalid JSON: {source}"),
            Self::MissingField { line, field } => {
                write!(
                    f,
                    "line {line}: incomplete log record — missing required field `{field}`"
                )
            }
            Self::Config(e) => write!(f, "invalid PID configuration: {e}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<io::Error> for ReplayError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ConfigError> for ReplayError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}

/// A single replayed iteration record written to the output NDJSON log.
#[derive(Serialize)]
pub struct ReplayRecord {
    pub schema_version: u64,
    pub ts: String,
    pub iter: u64,
    pub pv: f64,
    pub sp: f64,
    pub err: f64,
    pub p: f64,
    pub i: f64,
    pub d: f64,
    pub cv: f64,
    pub i_acc: f64,
    pub dt: f64,
}

/// CV delta statistics comparing replayed output against the original log.
pub struct DiffSummary {
    pub max_diff: f64,
    pub rms_diff: f64,
    /// Number of iteration records processed.
    pub n: u64,
}

/// Re-runs the PID controller against every iteration record in `reader`.
///
/// `pid_config.setpoint` is used as-is when `setpoint_from_cli` is `true`; otherwise
/// the setpoint is taken from the first iteration record's `sp` field.
///
/// Replayed records are written as NDJSON to `output` when provided.
/// Returns [`DiffSummary`] comparing replayed CV values against the original log's `cv`.
///
/// # Errors
///
/// Returns [`ReplayError`] on I/O failure, malformed JSON, a missing required field
/// in an iteration record, or an invalid PID configuration.
///
/// # Panics
///
/// Cannot panic: the `unwrap()` on the controller is guarded by the `is_none()` check
/// that initialises it on the same path.
#[allow(clippy::too_many_lines)]
pub fn replay(
    reader: impl BufRead,
    mut pid_config: PidConfig,
    setpoint_from_cli: bool,
    mut output: Option<&mut dyn Write>,
) -> Result<DiffSummary, ReplayError> {
    let mut controller: Option<PidController> = None;
    let mut prev_applied_cv = 0.0_f64;
    let mut iter = 0_u64;

    let mut sum_sq_diff = 0.0_f64;
    let mut max_diff = 0.0_f64;
    let mut n = 0_u64;

    for (line_idx, raw_line) in reader.lines().enumerate() {
        let line_num = (line_idx + 1) as u64;
        let raw = raw_line?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let v: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|source| ReplayError::Json {
                line: line_num,
                source,
            })?;

        // Skip control-event records — they carry an "event" string field.
        if v.get("event").is_some() {
            continue;
        }

        let has_pv = v.get("pv").is_some();
        let has_dt = v.get("dt").is_some();

        // Skip lines that appear to be neither events nor iteration records.
        if !has_pv && !has_dt {
            continue;
        }

        if !has_pv {
            return Err(ReplayError::MissingField {
                line: line_num,
                field: "pv",
            });
        }
        if !has_dt {
            return Err(ReplayError::MissingField {
                line: line_num,
                field: "dt",
            });
        }

        let pv = v["pv"].as_f64().ok_or(ReplayError::MissingField {
            line: line_num,
            field: "pv",
        })?;
        let dt = v["dt"].as_f64().ok_or(ReplayError::MissingField {
            line: line_num,
            field: "dt",
        })?;
        let orig_cv = v.get("cv").and_then(serde_json::Value::as_f64);

        // Initialise the controller on the first iteration record.
        if controller.is_none() {
            if !setpoint_from_cli {
                let sp = v["sp"].as_f64().ok_or(ReplayError::MissingField {
                    line: line_num,
                    field: "sp",
                })?;
                pid_config.setpoint = sp;
            }
            controller = Some(PidController::new(pid_config.clone())?);
        }
        let ctl = controller.as_mut().unwrap();

        let step = ctl.step(StepInput {
            pv,
            dt,
            prev_applied_cv,
        });
        prev_applied_cv = step.cv;
        iter += 1;

        // Accumulate diff stats.
        if let Some(ocv) = orig_cv {
            let diff = (step.cv - ocv).abs();
            if diff > max_diff {
                max_diff = diff;
            }
            sum_sq_diff += diff * diff;
            n += 1;
        }

        // Write replayed record to output log.
        if let Some(ref mut w) = output {
            let record = ReplayRecord {
                schema_version: STATE_SCHEMA_VERSION,
                ts: now_iso8601(),
                iter,
                pv,
                sp: pid_config.setpoint,
                err: ctl.last_error().unwrap_or(0.0),
                p: step.p_term,
                i: step.i_term,
                d: step.d_term,
                cv: step.cv,
                i_acc: step.i_acc,
                dt,
            };
            let json = serde_json::to_string(&record).map_err(io::Error::other)?;
            writeln!(w, "{json}")?;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let rms_diff = if n == 0 {
        0.0
    } else {
        (sum_sq_diff / n as f64).sqrt()
    };

    Ok(DiffSummary {
        max_diff,
        rms_diff,
        n,
    })
}
