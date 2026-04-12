mod cli;
#[cfg(feature = "tui")]
mod tune;
#[allow(clippy::wildcard_imports)]
pub(crate) use cli::*;

use clap::Parser;
use pid_ctl::adapters::{
    CmdCvSink, CmdPvSource, CvSink, DryRunCvSink, FileCvSink, FilePvSource, PvSource,
    StdinPvSource, StdoutCvSink,
};
use pid_ctl::app::{self, ControllerSession, StateSnapshot, StateStore};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn main() {
    let full_argv: Vec<String> = std::env::args().collect();

    let cli = Cli::try_parse().unwrap_or_else(|e| {
        // --help and --version print to stdout and exit 0 (normal clap behaviour).
        // All other parse errors are config errors → exit 3 (same as hand-rolled parser).
        match e.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                print!("{e}");
                process::exit(0);
            }
            _ => {
                eprintln!("{e}");
                process::exit(3);
            }
        }
    });

    let exit_code = match run(cli, &full_argv) {
        Ok(()) => 0,
        Err(error) => {
            if !error.message.is_empty() {
                eprintln!("{error}");
            }
            error.exit_code
        }
    };

    process::exit(exit_code);
}

fn run(
    cli: Cli,
    #[cfg_attr(not(feature = "tui"), allow(unused_variables))] full_argv: &[String],
) -> Result<(), CliError> {
    match cli.command {
        SubCommand::Once(raw) => {
            let parsed = parse_once(&raw)?;
            run_once(&parsed)
        }
        SubCommand::Pipe(raw) => {
            let parsed = parse_pipe(&raw)?;
            run_pipe(&parsed)
        }
        SubCommand::Loop(raw) => {
            let mut parsed = parse_loop(&raw)?;
            #[cfg(feature = "tui")]
            if parsed.tune {
                return tune::run(parsed, full_argv);
            }
            #[cfg(not(feature = "tui"))]
            if parsed.tune {
                return Err(CliError::config(
                    "--tune requires the 'tui' feature (not compiled in)",
                ));
            }
            run_loop(&mut parsed)
        }
        SubCommand::Status(raw) => {
            let flags = parse_status_flags(&raw)?;
            run_status_dispatch(&flags)
        }
        #[cfg(unix)]
        SubCommand::Set(raw) => run_socket_set(&raw),
        #[cfg(unix)]
        SubCommand::Hold(raw) => run_socket_hold(&raw),
        #[cfg(unix)]
        SubCommand::Resume(raw) => run_socket_resume(&raw),
        #[cfg(unix)]
        SubCommand::Reset(raw) => run_socket_reset(&raw),
        #[cfg(unix)]
        SubCommand::Save(raw) => run_socket_save(&raw),
        SubCommand::Purge(raw) => {
            let state_path =
                get_state_path(&raw).map_err(|_| CliError::config("purge requires --state"))?;
            run_purge(&state_path)
        }
        SubCommand::Init(raw) => {
            let state_path =
                get_state_path(&raw).map_err(|_| CliError::config("init requires --state"))?;
            run_init(&state_path)
        }
    }
}

fn run_once(args: &OnceArgs) -> Result<(), CliError> {
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
    let mut log_file = open_log_optional(args.log_path.as_deref())?;

    let dt = resolve_once_dt(&session, args, &mut log_file);

    let raw_pv = resolve_pv(&args.pv_source, args.pv_cmd_timeout)
        .map_err(|error| CliError::new(1, format!("failed to read PV: {error}")))?;
    let scaled_pv = raw_pv * args.scale;
    match session.process_pv(scaled_pv, dt, sink.as_mut()) {
        Ok(outcome) => {
            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(&mut log_file, reason, outcome.record.iter);
            }

            if matches!(args.output_format, OutputFormat::Json) {
                print_iteration_json(&outcome.record)?;
            }

            if let Some(file) = log_file.as_mut()
                && let Ok(json) = serde_json::to_string(&outcome.record)
            {
                let _ = writeln!(file, "{json}");
            }

            if let Some(error) = outcome.state_write_failed {
                if let Some(path) = &args.state_path {
                    json_events::emit_state_write_failed(
                        &mut log_file,
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
            json_events::emit_cv_write_failed(&mut log_file, error.to_string(), 1);
            Err(CliError::new(5, error.to_string()))
        }
    }
}

fn run_pipe(args: &PipeArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = StdoutCvSink {
        precision: args.cv_precision,
    };
    let mut log_file = open_log_optional(args.log_path.as_deref())?;

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
            json_events::emit_d_term_skipped(&mut log_file, reason, outcome.record.iter);
        }

        if let Some(file) = log_file.as_mut()
            && let Ok(json) = serde_json::to_string(&outcome.record)
        {
            let _ = writeln!(file, "{json}");
        }

        if let Some(error) = outcome.state_write_failed {
            emit_state_write_failure(
                &session,
                args.state_path.as_ref(),
                &mut log_file,
                &error,
                false,
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn run_loop(args: &mut LoopArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut pv_source =
        build_pv_source(&args.pv_source, args.pv_cmd_timeout, args.pv_stdin_timeout);
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

    let mut log_file = open_log_optional(args.log_path.as_deref())?;

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
        json_events::emit_socket_ready(&mut log_file, path.clone());
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

    let mut next_deadline = Instant::now() + args.interval;
    let mut last_tick = Instant::now();
    let mut cv_fail_count: u32 = 0;
    let mut pv_fail_count: u32 = 0;
    let mut hold = false;

    loop {
        // Check shutdown flag at top of each iteration.
        if shutdown.load(Ordering::Relaxed) {
            write_safe_cv(args.safe_cv, cv_sink.as_mut(), &mut session);
            flush_state_at_shutdown(&mut session, args.state_path.as_ref(), &mut log_file);
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
                    args,
                    &mut hold,
                    &shutdown,
                    &mut log_file,
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
            flush_state_at_shutdown(&mut session, args.state_path.as_ref(), &mut log_file);
            break;
        }

        let now = Instant::now();
        let tick_deadline = next_deadline;
        next_deadline = next_deadline_after_tick(tick_deadline, args.interval, now);

        // Hold mode: skip PID computation, keep servicing socket.
        if hold {
            continue;
        }

        // Measure dt — wall-clock elapsed since last PID step (actual time between ticks).
        let raw_dt = now.duration_since(last_tick).as_secs_f64();
        last_tick = now;

        // Interval slip (plan: Reliability §10 — tick longer than configured `--interval`).
        let interval_secs = args.interval.as_secs_f64();
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
            json_events::emit_interval_slip(&mut log_file, interval_ms, actual_ms);
        }

        let dt = match apply_measured_dt(
            raw_dt,
            args.min_dt,
            args.max_dt,
            args.dt_clamp,
            args.quiet,
            &mut log_file,
        ) {
            MeasuredDt::Skip => {
                handle_dt_skip_state_write(
                    session.on_dt_skipped(),
                    &session,
                    args.state_path.as_ref(),
                    &mut log_file,
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
                json_events::emit_pv_read_failure(&mut log_file, error.to_string(), args.safe_cv);
                write_safe_cv(args.safe_cv, cv_sink.as_mut(), &mut session);
                if let Some(limit) = args.fail_after
                    && pv_fail_count >= limit
                {
                    json_events::emit_pv_fail_after_reached(&mut log_file, pv_fail_count, limit);
                    return Err(CliError::new(
                        2,
                        format!("exiting after {pv_fail_count} consecutive PV read failures"),
                    ));
                }
                continue;
            }
        };

        run_loop_tick(
            args,
            raw_pv,
            dt,
            &mut session,
            cv_sink.as_mut(),
            &mut log_file,
            &mut cv_fail_count,
        )?;
    }

    Ok(())
}

/// Executes one PID tick: computes CV, writes to sink, logs outcome.
///
/// Returns `Err` when CV write failures exceed the configured limit.
fn run_loop_tick(
    args: &LoopArgs,
    raw_pv: f64,
    dt: f64,
    session: &mut ControllerSession,
    cv_sink: &mut dyn CvSink,
    log_file: &mut Option<std::fs::File>,
    cv_fail_count: &mut u32,
) -> Result<(), CliError> {
    let scaled_pv = raw_pv * args.scale;

    match session.process_pv(scaled_pv, dt, cv_sink) {
        Ok(outcome) => {
            *cv_fail_count = 0;

            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(log_file, reason, outcome.record.iter);
            }

            if matches!(args.output_format, OutputFormat::Json)
                && let Err(error) = print_iteration_json(&outcome.record)
            {
                eprintln!("output write failed: {error}");
            }

            if let Some(file) = log_file
                && let Ok(json) = serde_json::to_string(&outcome.record)
            {
                let _ = writeln!(file, "{json}");
            }

            if args.verbose {
                let r = &outcome.record;
                eprintln!(
                    "iter={} pv={:.4} sp={:.4} err={:.4} cv={:.4} p={:.4} i={:.4} d={:.4}",
                    r.iter, r.pv, r.sp, r.err, r.cv, r.p, r.i, r.d
                );
            }

            if let Some(error) = outcome.state_write_failed {
                emit_state_write_failure(
                    session,
                    args.state_path.as_ref(),
                    log_file,
                    &error,
                    args.quiet,
                );
            }
        }
        Err(error) => {
            *cv_fail_count += 1;
            let limit = args.cv_fail_after;
            eprintln!("CV write failed ({cv_fail_count}/{limit}): {error}");
            json_events::emit_cv_write_failed(log_file, error.to_string(), *cv_fail_count);
            if *cv_fail_count >= limit {
                write_safe_cv(args.safe_cv, cv_sink, session);
                return Err(CliError::new(
                    2,
                    format!("exiting after {cv_fail_count} consecutive CV write failures: {error}"),
                ));
            }
        }
    }

    Ok(())
}

pub(crate) enum MeasuredDt {
    Skip,
    Use(f64),
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn millis_round_u64(ms: f64) -> u64 {
    let v = ms.round();
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    if v >= u64::MAX as f64 {
        return u64::MAX;
    }
    v as u64
}

pub(crate) fn open_log_optional(path: Option<&Path>) -> Result<Option<std::fs::File>, CliError> {
    match path {
        Some(p) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|error| {
                    CliError::new(
                        1,
                        format!("failed to open log file {}: {error}", p.display()),
                    )
                })?;
            Ok(Some(file))
        }
        None => Ok(None),
    }
}

/// Applies `--min-dt` / `--max-dt` for measured `dt` in `loop`: skip (default) or clamp (`--dt-clamp`).
pub(crate) fn apply_measured_dt(
    raw_dt: f64,
    min_dt: f64,
    max_dt: f64,
    dt_clamp: bool,
    quiet: bool,
    log: &mut Option<std::fs::File>,
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
        json_events::emit_dt_clamped(log, raw_dt, clamped);
        MeasuredDt::Use(clamped)
    } else {
        if !quiet {
            if raw_dt < min_dt {
                eprintln!("dt {raw_dt:.6}s below min_dt {min_dt:.6}s — skipping tick");
            } else {
                eprintln!("dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — skipping tick");
            }
        }
        json_events::emit_dt_skipped(log, raw_dt, min_dt, max_dt);
        MeasuredDt::Skip
    }
}

fn resolve_once_dt(
    session: &ControllerSession,
    args: &OnceArgs,
    log: &mut Option<std::fs::File>,
) -> f64 {
    if args.dt_explicit {
        return args.dt;
    }
    if args.state_path.is_none() {
        return args.dt;
    }
    session
        .wall_clock_dt_since_state_update()
        .map_or(args.dt, |raw| {
            clamp_once_wall_clock_dt(raw, args.min_dt, args.max_dt, log)
        })
}

fn clamp_once_wall_clock_dt(
    raw: f64,
    min_dt: f64,
    max_dt: f64,
    log: &mut Option<std::fs::File>,
) -> f64 {
    let raw = raw.max(0.0);
    if raw < min_dt {
        eprintln!("once: wall-clock dt {raw:.6}s below --min-dt {min_dt:.6}s — clamping to min_dt");
        json_events::emit_dt_clamped(log, raw, min_dt);
        return min_dt;
    }
    if raw > max_dt {
        eprintln!(
            "once: wall-clock dt {raw:.6}s exceeds --max-dt {max_dt:.6}s — clamping to max_dt"
        );
        json_events::emit_dt_clamped(log, raw, max_dt);
        return max_dt;
    }
    raw
}

/// Writes the safe CV when configured; on success, records it as the last confirmed-applied CV.
pub(crate) fn write_safe_cv(
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

/// Applies a new loop interval at runtime, updating derived defaults (`max_dt`,
/// `pv_stdin_timeout`, `state_write_interval`) unless the user set them explicitly.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn apply_runtime_interval(
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    new_interval: Duration,
) -> Result<(), CliError> {
    args.interval = new_interval;
    let s = new_interval.as_secs_f64();
    if !args.explicit_max_dt {
        args.max_dt = (s * 3.0_f64).clamp(0.01, 60.0);
    }
    if !args.explicit_pv_stdin_timeout {
        args.pv_stdin_timeout = new_interval;
    }
    if !args.explicit_state_write_interval {
        let min_flush = Duration::from_millis(100);
        args.state_write_interval = Some(new_interval.max(min_flush));
    }
    session.set_flush_interval(args.state_write_interval);
    Ok(())
}

#[cfg(unix)]
/// Side effects that a socket command may request the loop to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketSideEffect {
    None,
    Hold,
    Resume,
    IntervalChanged,
}

#[cfg(unix)]
/// Dispatches a socket [`Request`] against the live controller session and
/// returns a JSON [`Response`] plus any side effect for the loop to apply.
pub(crate) fn handle_socket_request(
    req: &pid_ctl::socket::Request,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    log_file: &mut Option<std::fs::File>,
) -> (pid_ctl::socket::Response, SocketSideEffect) {
    use pid_ctl::socket::{Request, Response};

    match req {
        Request::Status => {
            let cfg = session.config();
            (
                Response::Status {
                    ok: true,
                    iter: session.iter(),
                    pv: session.last_pv().unwrap_or(0.0),
                    sp: cfg.setpoint,
                    err: session.last_error().unwrap_or(0.0),
                    kp: cfg.kp,
                    ki: cfg.ki,
                    kd: cfg.kd,
                    cv: session.last_applied_cv().unwrap_or(0.0),
                    i_acc: session.i_acc(),
                },
                SocketSideEffect::None,
            )
        }
        Request::Set { param, value } => handle_socket_set(param, *value, session, args, log_file),
        Request::Reset => {
            let i_acc_before = session.i_acc();
            session.reset_integral();
            json_events::emit_integral_reset(log_file, i_acc_before, session.iter(), "socket");
            (
                Response::Reset {
                    ok: true,
                    i_acc_before,
                },
                SocketSideEffect::None,
            )
        }
        Request::Hold => (
            Response::Ack {
                ok: true,
                error: None,
            },
            SocketSideEffect::Hold,
        ),
        Request::Resume => (
            Response::Ack {
                ok: true,
                error: None,
            },
            SocketSideEffect::Resume,
        ),
        Request::Save => {
            if !session.has_state_store() {
                return (
                    Response::Ack {
                        ok: false,
                        error: Some(String::from(
                            "no state store: loop was not started with --state",
                        )),
                    },
                    SocketSideEffect::None,
                );
            }
            if let Some(err) = session.force_flush() {
                (
                    Response::Ack {
                        ok: false,
                        error: Some(format!("save failed: {err}")),
                    },
                    SocketSideEffect::None,
                )
            } else {
                (
                    Response::Ack {
                        ok: true,
                        error: None,
                    },
                    SocketSideEffect::None,
                )
            }
        }
    }
}

/// Apply a single gain parameter (kp/ki/kd) to the session and emit the change event.
/// Returns the old value. Gains are ordered [kp, ki, kd] throughout.
#[cfg(unix)]
fn apply_gain_param(
    param: &str,
    value: f64,
    session: &mut ControllerSession,
    log_file: &mut Option<std::fs::File>,
) -> f64 {
    // gains[0]=kp, gains[1]=ki, gains[2]=kd
    let idx = match param {
        "kp" => 0usize,
        "ki" => 1,
        "kd" => 2,
        _ => unreachable!("apply_gain_param called with non-gain param: {param}"),
    };
    let cfg = session.config();
    let mut gains = [cfg.kp, cfg.ki, cfg.kd];
    let old = gains[idx];
    gains[idx] = value;
    session.set_gains(gains[0], gains[1], gains[2]);
    json_events::emit_gains_changed(
        log_file,
        session.config().kp,
        session.config().ki,
        session.config().kd,
        session.config().setpoint,
        session.iter(),
        "socket",
    );
    old
}

#[cfg(unix)]
fn handle_socket_set(
    param: &str,
    value: f64,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    log_file: &mut Option<std::fs::File>,
) -> (pid_ctl::socket::Response, SocketSideEffect) {
    use pid_ctl::socket::Response;

    let settable = || {
        vec![
            String::from("kp"),
            String::from("ki"),
            String::from("kd"),
            String::from("sp"),
            String::from("interval"),
        ]
    };

    match param {
        "kp" | "ki" | "kd" => {
            let old = apply_gain_param(param, value, session, log_file);
            (
                Response::Set {
                    ok: true,
                    param: param.to_string(),
                    old,
                    new: value,
                },
                SocketSideEffect::None,
            )
        }
        "sp" => {
            let old = session.config().setpoint;
            session.set_setpoint(value);
            json_events::emit_gains_changed(
                log_file,
                session.config().kp,
                session.config().ki,
                session.config().kd,
                value,
                session.iter(),
                "socket",
            );
            (
                Response::Set {
                    ok: true,
                    param: String::from("sp"),
                    old,
                    new: value,
                },
                SocketSideEffect::None,
            )
        }
        "interval" => {
            let old = args.interval.as_secs_f64();
            let new_interval = Duration::from_secs_f64(value);
            if let Err(e) = apply_runtime_interval(session, args, new_interval) {
                return (
                    Response::ErrorUnknownCommand {
                        ok: false,
                        error: format!("interval change failed: {e}"),
                        available: vec![],
                    },
                    SocketSideEffect::None,
                );
            }
            (
                Response::Set {
                    ok: true,
                    param: String::from("interval"),
                    old,
                    new: value,
                },
                SocketSideEffect::IntervalChanged,
            )
        }
        _ => (
            Response::ErrorUnknownParam {
                ok: false,
                error: format!("unknown parameter: {param}"),
                settable: settable(),
            },
            SocketSideEffect::None,
        ),
    }
}

#[cfg(unix)]
/// Sleeps until `until` in 50ms chunks, servicing socket connections between chunks.
fn sleep_with_socket(
    until: Instant,
    listener: &pid_ctl::socket::SocketListener,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    hold: &mut bool,
    shutdown: &AtomicBool,
    log_file: &mut Option<std::fs::File>,
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
                let (resp, effect) = handle_socket_request(&req, session, args, log_file);
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

/// Handles a state write failure that occurred during a dt-skip, applying escalation logic.
pub(crate) fn handle_dt_skip_state_write(
    err: Option<pid_ctl::app::StateStoreError>,
    session: &ControllerSession,
    state_path: Option<&std::path::PathBuf>,
    log_file: &mut Option<std::fs::File>,
    quiet: bool,
) {
    let Some(err) = err else {
        return;
    };
    emit_state_write_failure(session, state_path, log_file, &err, quiet);
}

/// Emits a state write failure — escalated warning if threshold reached, plain log otherwise.
pub(crate) fn emit_state_write_failure(
    session: &ControllerSession,
    state_path: Option<&std::path::PathBuf>,
    log_file: &mut Option<std::fs::File>,
    err: &pid_ctl::app::StateStoreError,
    quiet: bool,
) {
    if let Some(path) = state_path {
        if session.state_fail_escalated() {
            let count = session.state_fail_count();
            if !quiet {
                eprintln!("WARNING: state write failing persistently ({count} consecutive): {err}");
            }
            json_events::emit_state_write_escalated(log_file, path.clone(), err.to_string(), count);
        } else {
            if !quiet {
                eprintln!("state write failed: {err}");
            }
            json_events::emit_state_write_failed(log_file, path.clone(), err.to_string());
        }
    } else if !quiet {
        eprintln!("state write failed: {err}");
    }
}

/// Forces a final state flush at loop shutdown, logging any failure.
fn flush_state_at_shutdown(
    session: &mut ControllerSession,
    state_path: Option<&std::path::PathBuf>,
    log_file: &mut Option<std::fs::File>,
) {
    if let Some(err) = session.force_flush() {
        eprintln!("state write failed at shutdown: {err}");
        if let Some(path) = state_path {
            json_events::emit_state_write_failed(log_file, path.clone(), err.to_string());
        }
    }
}

pub(crate) fn build_pv_source(
    source: &PvSourceConfig,
    cmd_timeout: Duration,
    pv_stdin_timeout: Duration,
) -> Box<dyn PvSource> {
    match source {
        PvSourceConfig::Literal(_) => unreachable!("loop rejects literal PV"),
        PvSourceConfig::File(path) => Box::new(FilePvSource::new(path.clone())),
        PvSourceConfig::Cmd(cmd) => Box::new(CmdPvSource::new(cmd.clone(), cmd_timeout)),
        PvSourceConfig::Stdin => Box::new(StdinPvSource::new(pv_stdin_timeout)),
    }
}

pub(crate) fn build_cv_sink(
    cv_sink: &CvSinkConfig,
    precision: usize,
    default_cmd_timeout: Duration,
) -> Box<dyn CvSink> {
    match cv_sink {
        CvSinkConfig::Stdout => Box::new(StdoutCvSink { precision }),
        CvSinkConfig::File { path, verify } => {
            let mut sink = FileCvSink::new(path.clone());
            sink.precision = precision;
            sink.verify = *verify;
            Box::new(sink)
        }
        CvSinkConfig::Cmd { command, timeout } => {
            let effective_timeout = timeout.unwrap_or(default_cmd_timeout);
            Box::new(CmdCvSink::new(
                command.clone(),
                effective_timeout,
                precision,
            ))
        }
    }
}

pub(crate) fn print_iteration_json(record: &pid_ctl::app::IterationRecord) -> Result<(), CliError> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, record)
        .map_err(|error| CliError::new(3, format!("failed to serialize JSON output: {error}")))?;
    writeln!(handle).map_err(|error| CliError::new(1, format!("stdout write failed: {error}")))
}

fn run_status(state_path: &std::path::Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let snapshot = store
        .load()
        .map_err(|error| CliError::new(1, error.to_string()))?
        .ok_or_else(|| {
            CliError::new(1, format!("{}: state file not found", state_path.display()))
        })?;

    let json = snapshot
        .to_json_string()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{json}")
        .map_err(|error| CliError::new(1, format!("stdout write failed: {error}")))?;

    Ok(())
}

fn run_purge(state_path: &std::path::Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let _lock = store
        .acquire_lock()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    let snapshot = store
        .load()
        .map_err(|error| CliError::new(1, error.to_string()))?
        .ok_or_else(|| {
            CliError::new(1, format!("{}: state file not found", state_path.display()))
        })?;

    let purged = StateSnapshot {
        schema_version: snapshot.schema_version,
        name: snapshot.name,
        kp: snapshot.kp,
        ki: snapshot.ki,
        kd: snapshot.kd,
        setpoint: snapshot.setpoint,
        out_min: snapshot.out_min,
        out_max: snapshot.out_max,
        created_at: snapshot.created_at,
        updated_at: Some(app::now_iso8601()),
        // Runtime fields cleared:
        i_acc: 0.0,
        last_pv: None,
        last_error: None,
        last_cv: None,
        iter: 0,
        effective_sp: None,
        target_sp: None,
    };

    store
        .save(&purged)
        .map_err(|error| CliError::new(1, error.to_string()))?;

    eprintln!("purged runtime state from {}", state_path.display());

    Ok(())
}

fn run_init(state_path: &std::path::Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let _lock = store
        .acquire_lock()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    if state_path.exists() {
        std::fs::remove_file(state_path).map_err(|error| {
            CliError::new(
                1,
                format!("{}: failed to remove: {error}", state_path.display()),
            )
        })?;
    }

    let now = app::now_iso8601();
    let fresh = StateSnapshot {
        created_at: Some(now.clone()),
        updated_at: Some(now),
        ..StateSnapshot::default()
    };

    store
        .save(&fresh)
        .map_err(|error| CliError::new(1, error.to_string()))?;

    eprintln!("initialized state file {}", state_path.display());

    Ok(())
}

fn run_status_dispatch(flags: &StatusFlags) -> Result<(), CliError> {
    // Try socket first if provided (Unix only).
    #[cfg(unix)]
    if let Some(ref socket_path) = flags.socket_path {
        match pid_ctl::socket::client_request(socket_path, &pid_ctl::socket::Request::Status) {
            Ok(response) => {
                let json = serde_json::to_string(&response)
                    .map_err(|e| CliError::new(1, e.to_string()))?;
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                writeln!(handle, "{json}")
                    .map_err(|e| CliError::new(1, format!("stdout write failed: {e}")))?;
                return Ok(());
            }
            Err(pid_ctl::socket::SocketError::Io(ref e))
                if e.kind() == io::ErrorKind::ConnectionRefused =>
            {
                if flags.state_path.is_none() {
                    return Err(CliError::new(
                        1,
                        format!(
                            "socket connection refused at {} (no --state fallback)",
                            socket_path.display()
                        ),
                    ));
                }
                eprintln!("socket connection refused, falling back to state file");
            }
            Err(e) => return Err(CliError::new(1, format!("socket: {e}"))),
        }
    }

    // Fall back to state file.
    if let Some(ref state_path) = flags.state_path {
        run_status(state_path)
    } else {
        Err(CliError::config("status requires --state or --socket"))
    }
}

// ---------------------------------------------------------------------------
// Socket control subcommands: set, hold, resume, reset, save  (Unix only)
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// Send a socket request, print the JSON response to stdout, and exit non-zero when `ok` is false.
fn socket_send_and_print(
    socket_path: &Path,
    req: &pid_ctl::socket::Request,
    cmd: &str,
) -> Result<(), CliError> {
    match pid_ctl::socket::client_request(socket_path, req) {
        Ok(response) => {
            let json =
                serde_json::to_string(&response).map_err(|e| CliError::new(1, e.to_string()))?;
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            writeln!(handle, "{json}")
                .map_err(|e| CliError::new(1, format!("stdout write failed: {e}")))?;
            // Mirror ok:false as a non-zero exit so callers can detect failure without parsing JSON.
            let ok = serde_json::to_value(&response)
                .ok()
                .and_then(|v| v["ok"].as_bool())
                .unwrap_or(true);
            if ok {
                Ok(())
            } else {
                Err(CliError::new(1, String::new()))
            }
        }
        Err(e) => Err(CliError::new(1, format!("{cmd}: socket error: {e}"))),
    }
}

#[cfg(unix)]
fn run_socket_hold(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Hold, "hold")
}

#[cfg(unix)]
fn run_socket_resume(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Resume, "resume")
}

#[cfg(unix)]
fn run_socket_reset(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Reset, "reset")
}

#[cfg(unix)]
fn run_socket_save(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Save, "save")
}

#[cfg(unix)]
fn run_socket_set(raw: &SetRawArgs) -> Result<(), CliError> {
    let parsed = parse_set_args(raw);
    let req = pid_ctl::socket::Request::Set {
        param: parsed.param,
        value: parsed.value,
    };
    socket_send_and_print(&parsed.socket_path, &req, "set")
}

#[derive(Debug)]
pub(crate) struct CliError {
    exit_code: i32,
    message: String,
}

impl CliError {
    fn new(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
        }
    }

    fn config(message: impl Into<String>) -> Self {
        Self::new(3, message)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
