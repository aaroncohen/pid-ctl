use crate::{CliError, LoopArgs, LoopRuntimeConfig, OutputFormat, print_iteration_json};
use pid_ctl::adapters::{CvSink, DryRunCvSink};
use pid_ctl::app::adapters_build::{build_cv_sink, build_pv_source};
use pid_ctl::app::logger::Logger;
use pid_ctl::app::loop_runtime::{
    LoopControls, MeasuredDt, apply_measured_dt, flush_state_at_shutdown,
    handle_dt_skip_state_write, millis_round_u64, write_safe_cv,
};
use pid_ctl::app::ticker::{self, TickContext, TickObserver, TickStepResult};
use pid_ctl::app::{ControllerSession, TickError, TickOutcome};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use pid_ctl::app::socket_dispatch::{SocketSideEffect, handle_socket_request};

#[allow(clippy::too_many_lines)]
pub(crate) fn run_loop(args: &mut LoopArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut pv_source = build_pv_source(
        &args.pv_source,
        args.pv_cmd_timeout,
        args.runtime.pv_stdin_timeout(),
    );
    let mut cv_sink: Box<dyn CvSink> = if args.dry_run {
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

    // Bind socket listener when --socket is set.
    #[cfg(unix)]
    let socket_listener = if let Some(ref path) = args.socket_path {
        let listener =
            pid_ctl::socket::SocketListener::bind(path, args.socket_mode).map_err(|e| match e {
                pid_ctl::socket::SocketError::AlreadyRunning => CliError::new(
                    3,
                    format!("socket {}: another instance is running", path.display()),
                ),
                other => CliError::new(1, format!("socket: {other}")),
            })?;
        json_events::emit_socket_ready(&mut logger, path.clone());
        Some(listener)
    } else {
        None
    };

    // Set up SIGTERM/SIGINT shutdown flag.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::Relaxed);
    })
    .expect("signal handler");

    let mut next_deadline = Instant::now() + args.runtime.interval;
    let mut last_tick = Instant::now();
    let mut cv_fail_count: u32 = 0;
    let mut pv_fail_count: u32 = 0;
    let mut hold = false;

    loop {
        // Check shutdown flag at top of each iteration.
        if shutdown.load(Ordering::Relaxed) {
            write_safe_cv(args.safe_cv, cv_sink.as_mut(), &mut session);
            flush_state_at_shutdown(&mut session, args.state_path.as_deref(), &mut logger);
            break;
        }

        // Sleep until next deadline, servicing socket if active.
        let now = Instant::now();
        if now < next_deadline {
            #[cfg(unix)]
            if let Some(ref listener) = socket_listener {
                sleep_with_socket(
                    next_deadline,
                    listener,
                    &mut session,
                    &mut args.runtime,
                    &mut hold,
                    &shutdown,
                    &mut logger,
                );
            } else {
                std::thread::sleep(next_deadline - now);
            }
            #[cfg(not(unix))]
            std::thread::sleep(next_deadline - now);
        }

        // Check shutdown again after sleeping (signal may have arrived during sleep).
        if shutdown.load(Ordering::Relaxed) {
            write_safe_cv(args.safe_cv, cv_sink.as_mut(), &mut session);
            flush_state_at_shutdown(&mut session, args.state_path.as_deref(), &mut logger);
            break;
        }

        let now = Instant::now();
        let tick_deadline = next_deadline;
        next_deadline = next_deadline_after_tick(tick_deadline, args.runtime.interval, now);

        // Hold mode: skip PID computation, keep servicing socket.
        if hold {
            continue;
        }

        // Measure dt — wall-clock elapsed since last PID step (actual time between ticks).
        let raw_dt = now.duration_since(last_tick).as_secs_f64();
        last_tick = now;

        // Interval slip (plan: Reliability §10 — tick longer than configured `--interval`).
        let interval_secs = args.runtime.interval.as_secs_f64();
        if raw_dt > interval_secs {
            if !args.quiet {
                eprintln!(
                    "interval slip: tick took {:.0}ms (interval: {:.0}ms)",
                    raw_dt * 1000.0,
                    interval_secs * 1000.0
                );
            }
            let interval_ms = millis_round_u64(interval_secs * 1000.0);
            let actual_ms = millis_round_u64(raw_dt * 1000.0);
            json_events::emit_interval_slip(&mut logger, interval_ms, actual_ms);
        }

        let dt = match apply_measured_dt(
            raw_dt,
            *args.min_dt.value(),
            args.runtime.max_dt(),
            args.dt_clamp,
            args.quiet,
            &mut logger,
        ) {
            MeasuredDt::Skip => {
                handle_dt_skip_state_write(
                    session.on_dt_skipped(),
                    &session,
                    args.state_path.as_deref(),
                    &mut logger,
                    args.quiet,
                );
                continue;
            }
            MeasuredDt::Use(dt) => dt,
        };

        // Read PV.
        let raw_pv = match pv_source.read_pv() {
            Ok(pv) => {
                pv_fail_count = 0;
                pv
            }
            Err(error) => {
                pv_fail_count += 1;
                eprintln!("PV read failed: {error}");
                json_events::emit_pv_read_failure(&mut logger, error.to_string(), args.safe_cv);
                write_safe_cv(args.safe_cv, cv_sink.as_mut(), &mut session);
                if let Some(limit) = args.fail_after
                    && pv_fail_count >= limit
                {
                    json_events::emit_pv_fail_after_reached(&mut logger, pv_fail_count, limit);
                    return Err(CliError::new(
                        2,
                        format!("exiting after {pv_fail_count} consecutive PV read failures"),
                    ));
                }
                continue;
            }
        };

        let mut observer = LoopObserver {
            output_format: args.output_format,
            verbose: args.verbose,
            cv_fail_after: args.cv_fail_after,
        };
        let ctx = TickContext {
            scaled_pv: raw_pv * args.scale,
            dt,
            session: &mut session,
            cv_sink: cv_sink.as_mut(),
            logger: &mut logger,
            state_path: args.state_path.as_deref(),
            cv_fail_after: args.cv_fail_after,
            safe_cv: args.safe_cv,
            quiet: args.quiet,
        };
        if let TickStepResult::CvFailExhausted(msg) =
            ticker::step(ctx, &mut cv_fail_count, &mut observer)
        {
            return Err(CliError::new(2, msg));
        }
    }

    Ok(())
}

struct LoopObserver {
    output_format: OutputFormat,
    verbose: bool,
    cv_fail_after: u32,
}

impl TickObserver for LoopObserver {
    fn on_success(&mut self, outcome: &TickOutcome) {
        if matches!(self.output_format, OutputFormat::Json)
            && let Err(error) = print_iteration_json(&outcome.record)
        {
            eprintln!("output write failed: {error}");
        }
        if self.verbose {
            let r = &outcome.record;
            eprintln!(
                "iter={} pv={:.4} sp={:.4} err={:.4} cv={:.4} p={:.4} i={:.4} d={:.4}",
                r.iter, r.pv, r.sp, r.err, r.cv, r.p, r.i, r.d
            );
        }
    }

    fn on_cv_fail(&mut self, error: &TickError, consecutive: u32) {
        eprintln!(
            "CV write failed ({consecutive}/{}): {error}",
            self.cv_fail_after
        );
    }
}

#[cfg(unix)]
fn sleep_with_socket(
    until: Instant,
    listener: &pid_ctl::socket::SocketListener,
    session: &mut ControllerSession,
    runtime: &mut LoopRuntimeConfig,
    hold: &mut bool,
    shutdown: &AtomicBool,
    logger: &mut Logger,
) {
    loop {
        let now = Instant::now();
        if now >= until || shutdown.load(Ordering::Relaxed) {
            break;
        }
        let chunk = (until - now).min(Duration::from_millis(50));
        std::thread::sleep(chunk);

        for _ in 0..10 {
            match listener.try_service_one(|req| {
                let (resp, effect) = handle_socket_request(&req, session, runtime, logger);
                match effect {
                    SocketSideEffect::Hold => *hold = true,
                    SocketSideEffect::Resume => *hold = false,
                    SocketSideEffect::IntervalChanged | SocketSideEffect::None => {}
                }
                resp
            }) {
                Ok(Some(())) => {}
                _ => break,
            }
        }
    }
}
