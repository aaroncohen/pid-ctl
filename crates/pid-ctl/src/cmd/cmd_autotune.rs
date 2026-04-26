//! `pid-ctl autotune` — Åström–Hägglund relay feedback autotune.
//!
//! Drives the output as a relay (`bias ± amp`) and observes the resulting
//! limit-cycle oscillation. Estimates the ultimate gain `Ku` and period `Tu`,
//! then applies the requested tuning rule to suggest `(Kp, Ki, Kd)`.
//!
//! # Warmup phase
//!
//! Before starting the relay, the command applies `bias` for
//! `WARMUP_TICKS` ticks so the process variable can settle near the
//! operating point. The last PV sample is used as the relay reference
//! (`pv_ref`). This avoids the degenerate case where a cold-started plant
//! has PV=0 and the relay would latch in one position forever.
//!
//! # Output
//!
//! Progress events are emitted as NDJSON to stdout (`relay_flip`,
//! `period_detected`, `settled`). On completion the final result is
//! printed as a JSON object and, if `--state` was given, the suggested
//! gains are written to the state file.

use crate::{AutotuneArgs, CliError};
use pid_ctl::adapters::{CmdCvSink, CmdPvSource, CvSink, PvSource};
use pid_ctl::app::state_store::StateStore;
use pid_ctl::autotune::{AutotuneConfig, AutotuneEngine, RelayEvent};
use std::io::{self, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;

/// Fraction of the total test duration spent in warmup (applying `bias`).
const WARMUP_FRACTION: f64 = 0.25;
/// Minimum warmup ticks regardless of duration.
const WARMUP_TICKS_MIN: usize = 10;

pub(crate) fn run_autotune(args: &AutotuneArgs) -> Result<(), CliError> {
    let config = AutotuneConfig {
        bias: args.bias,
        amp: args.amp,
        out_min: args.out_min,
        out_max: args.out_max,
    };

    let mut pv_source: Box<dyn PvSource> =
        Box::new(CmdPvSource::new(args.pv_cmd.clone(), args.cmd_timeout));
    let mut cv_sink: Box<dyn CvSink> = Box::new(CmdCvSink::new(
        args.cv_cmd.clone(),
        args.cmd_timeout,
        args.cv_precision,
    ));

    let mut engine = AutotuneEngine::new(config);

    let shutdown = install_shutdown_handler();

    // ------------------------------------------------------------------
    // Warmup: apply bias so the PV settles near the operating point.
    // Uses 25% of the test duration (minimum WARMUP_TICKS_MIN ticks).
    // ------------------------------------------------------------------
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let warmup_ticks = {
        let frac =
            (args.duration.as_secs_f64() / args.interval.as_secs_f64() * WARMUP_FRACTION) as usize;
        frac.max(WARMUP_TICKS_MIN)
    };
    let mut warmup_pv = 0.0_f64;
    let mut warmup_last = Instant::now();
    for _ in 0..warmup_ticks {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        let now = Instant::now();
        let remaining = args
            .interval
            .saturating_sub(now.duration_since(warmup_last));
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
        warmup_last = Instant::now();

        if let Ok(pv) = pv_source.read_pv() {
            warmup_pv = pv;
        }
        let _ = cv_sink.write_cv(args.bias);
    }
    engine.set_pv_ref(warmup_pv);

    // ------------------------------------------------------------------
    // Relay loop
    // ------------------------------------------------------------------
    let deadline_start = Instant::now();
    let mut next_deadline = deadline_start + args.interval;
    let mut last_tick = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            eprintln!("autotune: interrupted");
            break;
        }

        // Sleep until next tick deadline.
        let now = Instant::now();
        if now < next_deadline {
            thread::sleep(next_deadline - now);
        }
        let now = Instant::now();
        next_deadline += args.interval;

        let elapsed = now.duration_since(deadline_start);
        if elapsed >= args.duration {
            break;
        }

        let dt = now.duration_since(last_tick).as_secs_f64();
        last_tick = now;

        // Read PV.
        let pv = match pv_source.read_pv() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("autotune: PV read failed: {e}");
                continue;
            }
        };

        // Advance engine.
        let (cv, events) = engine.tick(pv, dt);

        // Emit NDJSON events.
        for event in &events {
            emit_event(event);
        }

        // Write CV.
        if let Err(e) = cv_sink.write_cv(cv) {
            eprintln!("autotune: CV write failed: {e}");
        }

        // Stop early once settled.
        if engine.is_settled() {
            break;
        }
    }

    // ------------------------------------------------------------------
    // Emit final result
    // ------------------------------------------------------------------
    let result = engine.result(args.rule).ok_or_else(|| {
        CliError::new(
            2,
            "autotune did not observe enough oscillation cycles to estimate \
             Ku/Tu — try a longer --duration or larger --amp",
        )
    })?;

    let json = serde_json::to_string(&result).map_err(|e| CliError::new(1, e.to_string()))?;
    println!("{json}");

    // Optionally persist suggested gains to state file.
    if let Some(ref state_path) = args.state {
        let store = StateStore::new(state_path);
        let existing = store.load().map_err(|e| CliError::new(1, e.to_string()))?;
        let mut snapshot = existing.unwrap_or_default();
        snapshot.kp = Some(result.kp);
        snapshot.ki = Some(result.ki);
        snapshot.kd = Some(result.kd);
        store
            .save(&snapshot)
            .map_err(|e| CliError::new(1, e.to_string()))?;
    }

    Ok(())
}

fn emit_event(event: &RelayEvent) {
    match serde_json::to_string(event) {
        Ok(json) => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            let _ = writeln!(handle, "{json}");
        }
        Err(e) => eprintln!("autotune: failed to serialise event: {e}"),
    }
}

fn install_shutdown_handler() -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    // Ignore errors: a second ctrlc handler registration panics on some platforms.
    let _ = ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::Relaxed);
    });
    shutdown
}
