use crate::{CliError, LoopArgs, LoopRuntimeConfig, OutputFormat, print_iteration_json};
use pid_ctl::adapters::{CvSink, PvSource};
use pid_ctl::app::adapters_build::{build_cv_mode_sink, build_loop_pv_source};
use pid_ctl::app::logger::Logger;
use pid_ctl::app::loop_runtime::{
    LoopControls, MeasuredDt, apply_measured_dt, flush_state_at_shutdown,
    handle_dt_skip_state_write, millis_round_u64, write_safe_cv,
};
use pid_ctl::app::ticker::{self, TickContext, TickObserver, TickStepResult};
use pid_ctl::app::{self, ControllerSession, TickError, TickOutcome};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(unix)]
use pid_ctl::app::socket_dispatch::{SocketSideEffect, handle_socket_request};
#[cfg(unix)]
use pid_ctl::socket::{SocketError, SocketListener};

pub(crate) fn run_loop(args: &mut LoopArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut pv_source = build_loop_pv_source(
        &args.pv_source,
        args.pv_cmd_timeout,
        args.runtime.pv_stdin_timeout(),
    );
    let mut cv_sink: Box<dyn CvSink> =
        build_cv_mode_sink(&args.cv_mode(), args.cv_precision, args.cmd_timeout);

    let mut logger = super::open_log(args.log_path.as_deref())?;

    #[cfg(unix)]
    let socket_listener = bind_socket_listener(args, &mut logger)?;

    let shutdown = install_shutdown_handler();

    let mut next_deadline = Instant::now() + args.runtime.interval;
    let mut last_tick = Instant::now();
    let mut cv_fail_count: u32 = 0;
    let mut pv_fail_count: u32 = 0;
    let mut hold = false;

    loop {
        // Check shutdown flag at top of each iteration.
        if shutdown.load(Ordering::Relaxed) {
            finalize_shutdown(args, cv_sink.as_mut(), &mut session, &mut logger);
            break;
        }

        sleep_until_next_deadline(
            next_deadline,
            #[cfg(unix)]
            socket_listener.as_ref(),
            &mut session,
            &mut args.runtime,
            &mut hold,
            &shutdown,
            &mut logger,
        );

        // Check shutdown again after sleeping (signal may have arrived during sleep).
        if shutdown.load(Ordering::Relaxed) {
            finalize_shutdown(args, cv_sink.as_mut(), &mut session, &mut logger);
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

        let interval_secs = args.runtime.interval.as_secs_f64();
        log_interval_slip_if_needed(raw_dt, interval_secs, args.quiet, &mut logger);

        let Some(dt) = resolve_tick_dt(raw_dt, args, &mut session, &mut logger) else {
            continue;
        };

        let Some(raw_pv) = read_pv_with_escalation(
            pv_source.as_mut(),
            &mut pv_fail_count,
            args.fail_after,
            args.safe_cv,
            cv_sink.as_mut(),
            &mut session,
            &mut logger,
        )?
        else {
            continue;
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

        // Test hook: exit after N successful ticks so tests can synchronize on a
        // deterministic signal instead of racing a wall-clock kill.
        if let Some(limit) = args.max_iterations
            && session.iteration() >= limit
        {
            finalize_shutdown(args, cv_sink.as_mut(), &mut session, &mut logger);
            break;
        }
    }

    Ok(())
}

/// Binds the optional Unix-socket listener for operator commands.
#[cfg(unix)]
fn bind_socket_listener(
    args: &LoopArgs,
    logger: &mut Logger,
) -> Result<Option<SocketListener>, CliError> {
    let Some(ref path) = args.socket_path else {
        return Ok(None);
    };
    let listener = SocketListener::bind(path, args.socket_mode).map_err(|e| match e {
        SocketError::AlreadyRunning => CliError::new(
            3,
            format!("socket {}: another instance is running", path.display()),
        ),
        other => CliError::new(1, format!("socket: {other}")),
    })?;
    json_events::emit_socket_ready(logger, path.clone());
    Ok(Some(listener))
}

/// Installs the SIGTERM/SIGINT handler and returns the shared shutdown flag.
fn install_shutdown_handler() -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::Relaxed);
    })
    .expect("signal handler");
    shutdown
}

/// Writes the safe CV and flushes state on shutdown.
fn finalize_shutdown(
    args: &LoopArgs,
    cv_sink: &mut dyn CvSink,
    session: &mut ControllerSession,
    logger: &mut Logger,
) {
    write_safe_cv(args.safe_cv, cv_sink, session);
    flush_state_at_shutdown(session, args.state_path.as_deref(), logger);
}

/// Resolves the tick's effective dt — applying clamp/skip semantics. Returns `None` when
/// the tick should be skipped (and emits the associated state-write side effects).
fn resolve_tick_dt(
    raw_dt: f64,
    args: &LoopArgs,
    session: &mut ControllerSession,
    logger: &mut Logger,
) -> Option<f64> {
    match apply_measured_dt(
        raw_dt,
        *args.min_dt.value(),
        args.runtime.max_dt(),
        args.dt_clamp,
        args.quiet,
        logger,
    ) {
        MeasuredDt::Skip => {
            handle_dt_skip_state_write(
                session.on_dt_skipped(),
                session,
                args.state_path.as_deref(),
                logger,
                args.quiet,
            );
            None
        }
        MeasuredDt::Use(dt) => Some(dt),
    }
}

/// Sleeps until `next_deadline`, servicing the optional socket listener in small
/// chunks so operator commands remain responsive while the loop waits for its tick.
fn sleep_until_next_deadline(
    next_deadline: Instant,
    #[cfg(unix)] socket_listener: Option<&SocketListener>,
    session: &mut ControllerSession,
    runtime: &mut LoopRuntimeConfig,
    hold: &mut bool,
    shutdown: &AtomicBool,
    logger: &mut Logger,
) {
    let now = Instant::now();
    if now >= next_deadline {
        return;
    }

    #[cfg(unix)]
    if let Some(listener) = socket_listener {
        sleep_with_socket(
            next_deadline,
            listener,
            session,
            runtime,
            hold,
            shutdown,
            logger,
        );
        return;
    }

    // Fallback for non-unix or when no socket is bound — unused locals suppressed.
    let _ = (session, runtime, hold, shutdown, logger);
    std::thread::sleep(next_deadline - now);
}

/// Reads a PV sample, applying the safe-CV and escalation policy on failure.
///
/// Returns `Ok(Some(pv))` when a PV was read, `Ok(None)` when the caller should
/// `continue` to the next tick, or `Err` when the failure limit was reached.
fn read_pv_with_escalation(
    pv_source: &mut dyn PvSource,
    pv_fail_count: &mut u32,
    fail_after: Option<u32>,
    safe_cv: Option<f64>,
    cv_sink: &mut dyn CvSink,
    session: &mut ControllerSession,
    logger: &mut Logger,
) -> Result<Option<f64>, CliError> {
    match pv_source.read_pv() {
        Ok(pv) => {
            *pv_fail_count = 0;
            Ok(Some(pv))
        }
        Err(error) => {
            *pv_fail_count += 1;
            eprintln!("PV read failed: {error}");
            json_events::emit_pv_read_failure(logger, error.to_string(), safe_cv);
            write_safe_cv(safe_cv, cv_sink, session);
            if let Some(limit) = fail_after
                && *pv_fail_count >= limit
            {
                json_events::emit_pv_fail_after_reached(logger, *pv_fail_count, limit);
                return Err(CliError::new(
                    2,
                    format!(
                        "exiting after {count} consecutive PV read failures",
                        count = *pv_fail_count
                    ),
                ));
            }
            Ok(None)
        }
    }
}

/// Emits the interval-slip event when a tick took longer than the configured interval
/// (plan: Reliability §10).
fn log_interval_slip_if_needed(raw_dt: f64, interval_secs: f64, quiet: bool, logger: &mut Logger) {
    if raw_dt <= interval_secs {
        return;
    }
    if !quiet {
        eprintln!(
            "interval slip: tick took {:.0}ms (interval: {:.0}ms)",
            raw_dt * 1000.0,
            interval_secs * 1000.0
        );
    }
    let interval_ms = millis_round_u64(interval_secs * 1000.0);
    let actual_ms = millis_round_u64(raw_dt * 1000.0);
    json_events::emit_interval_slip(logger, interval_ms, actual_ms);
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
/// Sleeps until `until` in 50ms chunks, servicing socket connections between chunks.
fn sleep_with_socket(
    until: Instant,
    listener: &SocketListener,
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
        let chunk = (until - now).min(app::defaults::SOCKET_SLEEP_CHUNK);
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
