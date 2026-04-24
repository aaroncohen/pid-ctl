use crate::{CliError, OnceArgs, OutputFormat, print_iteration_json, resolve_pv};
use pid_ctl::adapters::{CvSink, DryRunCvSink};
use pid_ctl::app::ControllerSession;
use pid_ctl::app::adapters_build::build_cv_sink;
use pid_ctl::app::logger::Logger;
use pid_ctl::json_events;

pub(crate) fn run_once(args: &OnceArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink: Box<dyn CvSink> = if args.dry_run {
        Box::new(DryRunCvSink)
    } else {
        build_cv_sink(
            args.cv_sink
                .as_ref()
                .expect("cv_sink required when not dry_run"),
            args.cv_precision,
            args.cmd_timeout,
        )
    };
    let mut logger = super::open_log(args.log_path.as_deref())?;

    let dt = resolve_once_dt(&session, args, &mut logger);

    let raw_pv = resolve_pv(&args.pv_source, args.pv_cmd_timeout)
        .map_err(|error| CliError::new(1, format!("failed to read PV: {error}")))?;
    let scaled_pv = raw_pv * args.scale;
    match session.process_pv(scaled_pv, dt, sink.as_mut()) {
        Ok(outcome) => {
            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(&mut logger, reason, outcome.record.iter);
            }

            if matches!(args.output_format, OutputFormat::Json) {
                print_iteration_json(&outcome.record)?;
            }

            logger.write_iteration_line(&outcome.record);

            if let Some(error) = outcome.state_write_failed {
                if let Some(path) = &args.state_path {
                    json_events::emit_state_write_failed(
                        &mut logger,
                        path.clone(),
                        error.to_string(),
                    );
                }
                return Err(CliError::new(
                    4,
                    format!("state persistence failed after CV was emitted: {error}"),
                ));
            }

            Ok(())
        }
        Err(error) => {
            json_events::emit_cv_write_failed(&mut logger, error.to_string(), 1);
            Err(CliError::new(5, error.to_string()))
        }
    }
}

fn resolve_once_dt(session: &ControllerSession, args: &OnceArgs, logger: &mut Logger) -> f64 {
    if args.dt_explicit {
        return args.dt;
    }
    if args.state_path.is_none() {
        return args.dt;
    }
    session
        .wall_clock_dt_since_state_update()
        .map_or(args.dt, |raw| {
            clamp_once_wall_clock_dt(raw, args.min_dt, args.max_dt, logger)
        })
}

fn clamp_once_wall_clock_dt(raw: f64, min_dt: f64, max_dt: f64, logger: &mut Logger) -> f64 {
    let raw = raw.max(0.0);
    if raw < min_dt {
        eprintln!("once: wall-clock dt {raw:.6}s below --min-dt {min_dt:.6}s — clamping to min_dt");
        json_events::emit_dt_clamped(logger, raw, min_dt);
        return min_dt;
    }
    if raw > max_dt {
        eprintln!(
            "once: wall-clock dt {raw:.6}s exceeds --max-dt {max_dt:.6}s — clamping to max_dt"
        );
        json_events::emit_dt_clamped(logger, raw, max_dt);
        return max_dt;
    }
    raw
}
