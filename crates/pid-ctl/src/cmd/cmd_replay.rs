//! `pid-ctl replay` — re-run the PID core against a recorded NDJSON log with
//! counterfactual gains.
//!
//! Reads iteration records from a log produced by `loop --log`, steps the
//! PID controller with the new gains and the recorded `(pv, dt)` from each
//! line, and emits a replayed NDJSON stream. Non-iteration event lines (those
//! with an `"event"` field) are silently skipped.
//!
//! With `--diff`, no log is written; instead a JSON summary of the CV
//! difference between the original and replayed streams is printed to stdout.

use crate::{CliError, ReplayArgs};
use pid_ctl::app::{IterationRecord, STATE_SCHEMA_VERSION, now_iso8601};
use pid_ctl_core::{PidConfig, PidController, StepInput};
use serde::Serialize;
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

// ---------------------------------------------------------------------------
// Intermediate representation of a parsed log line
// ---------------------------------------------------------------------------

struct LogRecord {
    iter: u64,
    ts: String,
    name: Option<String>,
    pv: f64,
    sp: f64,
    dt: f64,
    ff: f64,
    original_cv: f64,
}

/// Returns `Some(record)` for iteration lines, `None` for skippable lines
/// (event records, empty lines, unrecognised JSON objects).
///
/// # Errors
///
/// Returns an error for non-JSON text or for lines that look like iteration
/// records (have `"iter"`) but are missing required fields (`pv`, `dt`, `sp`).
fn parse_log_line(line: &str, line_num: usize) -> Result<Option<LogRecord>, CliError> {
    let v: Value = serde_json::from_str(line).map_err(|e| {
        CliError::new(
            1,
            format!("line {line_num}: not valid JSON: {e}\n  → {line:?}"),
        )
    })?;

    // Lines with an "event" field are non-iteration NDJSON events — skip them.
    if v.get("event").is_some() {
        return Ok(None);
    }

    // Lines with "iter" are iteration records — require the full set of fields.
    if v.get("iter").is_some() {
        let pv = v["pv"].as_f64().ok_or_else(|| {
            CliError::new(
                1,
                format!("line {line_num}: iteration record missing `pv`\n  → {line:?}"),
            )
        })?;
        let dt = v["dt"].as_f64().ok_or_else(|| {
            CliError::new(
                1,
                format!("line {line_num}: iteration record missing `dt`\n  → {line:?}"),
            )
        })?;
        let sp = v["sp"].as_f64().ok_or_else(|| {
            CliError::new(
                1,
                format!("line {line_num}: iteration record missing `sp`\n  → {line:?}"),
            )
        })?;
        let original_cv = v["cv"].as_f64().ok_or_else(|| {
            CliError::new(
                1,
                format!("line {line_num}: iteration record missing `cv`\n  → {line:?}"),
            )
        })?;
        let iter = v["iter"].as_u64().ok_or_else(|| {
            CliError::new(
                1,
                format!("line {line_num}: iteration record has invalid `iter`\n  → {line:?}"),
            )
        })?;
        let ts = v["ts"].as_str().unwrap_or("").to_owned();
        let name = v["name"].as_str().map(str::to_owned);
        let ff = v["ff"].as_f64().unwrap_or(0.0);

        return Ok(Some(LogRecord {
            iter,
            ts,
            name,
            pv,
            sp,
            dt,
            ff,
            original_cv,
        }));
    }

    // Unknown JSON object — skip silently.
    Ok(None)
}

// ---------------------------------------------------------------------------
// Diff statistics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DiffAccumulator {
    n: u64,
    sum_sq: f64,
    max: f64,
}

impl DiffAccumulator {
    fn push(&mut self, original_cv: f64, replayed_cv: f64) {
        let diff = (replayed_cv - original_cv).abs();
        self.n += 1;
        self.sum_sq += diff * diff;
        if diff > self.max {
            self.max = diff;
        }
    }

    fn rms(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            (self.sum_sq / self.n as f64).sqrt()
        }
    }
}

#[derive(Serialize)]
struct DiffResult {
    n: u64,
    max_cv_diff: f64,
    rms_cv_diff: f64,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub(crate) fn run_replay(args: &ReplayArgs) -> Result<(), CliError> {
    let pid_config = PidConfig {
        kp: args.kp,
        ki: args.ki,
        kd: args.kd,
        out_min: args.out_min,
        out_max: args.out_max,
        ..PidConfig::default()
    };
    pid_config
        .validate()
        .map_err(|e| CliError::config(e.to_string()))?;

    let mut controller =
        PidController::new(pid_config).map_err(|e| CliError::config(e.to_string()))?;

    let log_file = File::open(&args.log)
        .map_err(|e| CliError::new(1, format!("cannot open log {}: {e}", args.log.display())))?;
    let reader = BufReader::new(log_file);

    let mut output: Option<Box<dyn Write>> = if args.diff {
        None
    } else if let Some(ref path) = args.output_log {
        let f = File::create(path).map_err(|e| {
            CliError::new(
                1,
                format!("cannot create output log {}: {e}", path.display()),
            )
        })?;
        Some(Box::new(BufWriter::new(f)))
    } else {
        Some(Box::new(BufWriter::new(io::stdout())))
    };

    let mut prev_applied_cv = 0.0_f64;
    let mut diff_acc = DiffAccumulator::default();

    for (idx, line_result) in reader.lines().enumerate() {
        let line = line_result
            .map_err(|e| CliError::new(1, format!("read error at line {}: {e}", idx + 1)))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some(record) = parse_log_line(line, idx + 1)? else {
            continue;
        };

        controller.set_setpoint(record.sp);
        let step = controller.step(StepInput {
            pv: record.pv,
            dt: record.dt,
            prev_applied_cv,
            ff: record.ff,
        });
        prev_applied_cv = step.cv;

        if args.diff {
            diff_acc.push(record.original_cv, step.cv);
        } else {
            let replayed = IterationRecord {
                schema_version: STATE_SCHEMA_VERSION,
                ts: if record.ts.is_empty() {
                    now_iso8601()
                } else {
                    record.ts
                },
                name: record.name,
                iter: record.iter,
                pv: record.pv,
                sp: record.sp,
                effective_sp: None,
                err: controller.last_error().unwrap_or(0.0),
                p: step.p_term,
                i: step.i_term,
                d: step.d_term,
                ff: step.ff_term,
                cv: step.cv,
                i_acc: step.i_acc,
                dt: record.dt,
            };
            let json = serde_json::to_string(&replayed)
                .map_err(|e| CliError::new(1, format!("serialisation error: {e}")))?;
            if let Some(ref mut w) = output {
                writeln!(w, "{json}").map_err(|e| CliError::new(1, format!("write error: {e}")))?;
            }
        }
    }

    if args.diff {
        let result = DiffResult {
            n: diff_acc.n,
            max_cv_diff: diff_acc.max,
            rms_cv_diff: diff_acc.rms(),
        };
        let json = serde_json::to_string(&result)
            .map_err(|e| CliError::new(1, format!("serialisation error: {e}")))?;
        println!("{json}");
    }

    Ok(())
}
