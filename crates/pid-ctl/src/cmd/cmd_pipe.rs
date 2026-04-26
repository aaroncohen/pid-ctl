use crate::{CliError, PipeArgs, parse_f64_value};
use pid_ctl::adapters::StdoutCvSink;
use pid_ctl::app::ControllerSession;
use pid_ctl::app::loop_runtime::emit_state_write_failure;
use pid_ctl::json_events;
use std::io::{self, BufRead};
use std::time::Instant;

pub(crate) fn run_pipe(args: &PipeArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = StdoutCvSink {
        precision: args.cv_precision,
    };
    let mut logger = super::open_log(args.log_path.as_deref())?;

    // Monotonic clock for dt: use elapsed time between lines (plan §dt handling).
    // First line uses args.dt (no prior tick to measure from).
    let mut last_tick: Option<Instant> = None;

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| CliError::new(1, format!("stdin read failed: {error}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let now = Instant::now();
        let dt = last_tick.map_or(args.dt, |prev| now.duration_since(prev).as_secs_f64());
        last_tick = Some(now);

        let pv = parse_f64_value("--stdin", trimmed)? * args.scale;
        let outcome = session
            .process_pv(pv, dt, &mut sink)
            .map_err(|error| CliError::new(1, error.to_string()))?;

        if let Some(reason) = outcome.d_term_skipped {
            json_events::emit_d_term_skipped(&mut logger, reason, outcome.record.iter);
        }

        logger.write_iteration_line(&outcome.record);

        if let Some(error) = outcome.state_write_failed {
            emit_state_write_failure(
                &session,
                args.state_path.as_deref(),
                &mut logger,
                &error,
                false,
            );
        }
    }

    Ok(())
}
