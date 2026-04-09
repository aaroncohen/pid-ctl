mod tune;

use pid_ctl::adapters::{
    CmdCvSink, CmdPvSource, CvSink, DryRunCvSink, FileCvSink, FilePvSource, PvSource,
    StdinPvSource, StdoutCvSink,
};
use pid_ctl::app::{self, ControllerSession, SessionConfig, StateSnapshot, StateStore};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::env;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn main() {
    let full_argv: Vec<String> = env::args().collect();
    let args: Vec<String> = env::args().skip(1).collect();
    let exit_code = match run(&args, &full_argv) {
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

fn run(args: &[String], full_argv: &[String]) -> Result<(), CliError> {
    let Some((command, rest)) = args.split_first() else {
        return Err(CliError::config(
            "usage: pid-ctl <once|loop|pipe|status|set|hold|resume|reset|save|purge|init> [OPTIONS]",
        ));
    };

    match command.as_str() {
        "once" => {
            let parsed = parse_once(rest)?;
            run_once(&parsed)
        }
        "pipe" => {
            let parsed = parse_pipe(rest)?;
            run_pipe(&parsed)
        }
        "loop" => {
            let mut parsed = parse_loop(rest)?;
            if parsed.tune {
                tune::run(parsed, full_argv.to_vec())
            } else {
                run_loop(&mut parsed)
            }
        }
        "status" => {
            let flags = parse_status_flags(rest)?;
            run_status_dispatch(&flags)
        }
        "set" => run_socket_set(rest),
        "hold" => run_socket_hold(rest),
        "resume" => run_socket_resume(rest),
        "reset" => run_socket_reset(rest),
        "save" => run_socket_save(rest),
        "purge" => {
            let state_path = parse_state_flag(rest, "purge")?;
            run_purge(&state_path)
        }
        "init" => {
            let state_path = parse_state_flag(rest, "init")?;
            run_init(&state_path)
        }
        other => Err(CliError::config(format!(
            "unknown subcommand `{other}`; expected `once`, `pipe`, `loop`, `status`, `set`, `hold`, `resume`, `reset`, `save`, `purge`, or `init`"
        ))),
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

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| CliError::new(1, format!("stdin read failed: {error}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let pv = parse_f64_value("--stdin", trimmed)? * args.scale;
        let outcome = session
            .process_pv(pv, args.dt, &mut sink)
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
            emit_state_write_failure(&session, args.state_path.as_ref(), &mut log_file, &error);
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
            eprintln!(
                "interval slip: tick took {:.0}ms (interval: {:.0}ms)",
                raw_dt * 1000.0,
                interval_secs * 1000.0
            );
            let interval_ms = millis_round_u64(interval_secs * 1000.0);
            let actual_ms = millis_round_u64(raw_dt * 1000.0);
            json_events::emit_interval_slip(&mut log_file, interval_ms, actual_ms);
        }

        let dt = match apply_measured_dt(
            raw_dt,
            args.min_dt,
            args.max_dt,
            args.dt_clamp,
            &mut log_file,
        ) {
            MeasuredDt::Skip => {
                handle_dt_skip_state_write(
                    session.on_dt_skipped(),
                    &session,
                    args.state_path.as_ref(),
                    &mut log_file,
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

            if let Some(error) = outcome.state_write_failed {
                emit_state_write_failure(session, args.state_path.as_ref(), log_file, &error);
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
                    format!(
                        "exiting after {cv_fail_count} consecutive CV write failures: {error}"
                    ),
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
    log: &mut Option<std::fs::File>,
) -> MeasuredDt {
    if raw_dt >= min_dt && raw_dt <= max_dt {
        return MeasuredDt::Use(raw_dt);
    }

    if dt_clamp {
        let clamped = raw_dt.clamp(min_dt, max_dt);
        if raw_dt < min_dt {
            eprintln!("dt {raw_dt:.6}s below min_dt {min_dt:.6}s — clamping to min_dt");
        } else {
            eprintln!("dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — clamping to max_dt");
        }
        json_events::emit_dt_clamped(log, raw_dt, clamped);
        MeasuredDt::Use(clamped)
    } else {
        if raw_dt < min_dt {
            eprintln!("dt {raw_dt:.6}s below min_dt {min_dt:.6}s — skipping tick");
        } else {
            eprintln!("dt {raw_dt:.6}s exceeds max_dt {max_dt:.6}s — skipping tick");
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

/// Side effects that a socket command may request the loop to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketSideEffect {
    None,
    Hold,
    Resume,
    IntervalChanged,
}

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
        Request::Hold => (Response::Ack { ok: true, error: None }, SocketSideEffect::Hold),
        Request::Resume => (Response::Ack { ok: true, error: None }, SocketSideEffect::Resume),
        Request::Save => {
            if !session.has_state_store() {
                return (
                    Response::Ack {
                        ok: false,
                        error: Some(String::from("no state store: loop was not started with --state")),
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
                (Response::Ack { ok: true, error: None }, SocketSideEffect::None)
            }
        }
    }
}

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
        "kp" => {
            let old = session.config().kp;
            session.set_gains(value, session.config().ki, session.config().kd);
            json_events::emit_gains_changed(log_file, value, session.config().ki, session.config().kd, session.config().setpoint, session.iter(), "socket");
            (Response::Set { ok: true, param: String::from("kp"), old, new: value }, SocketSideEffect::None)
        }
        "ki" => {
            let old = session.config().ki;
            session.set_gains(session.config().kp, value, session.config().kd);
            json_events::emit_gains_changed(log_file, session.config().kp, value, session.config().kd, session.config().setpoint, session.iter(), "socket");
            (Response::Set { ok: true, param: String::from("ki"), old, new: value }, SocketSideEffect::None)
        }
        "kd" => {
            let old = session.config().kd;
            session.set_gains(session.config().kp, session.config().ki, value);
            json_events::emit_gains_changed(log_file, session.config().kp, session.config().ki, value, session.config().setpoint, session.iter(), "socket");
            (Response::Set { ok: true, param: String::from("kd"), old, new: value }, SocketSideEffect::None)
        }
        "sp" => {
            let old = session.config().setpoint;
            session.set_setpoint(value);
            json_events::emit_gains_changed(log_file, session.config().kp, session.config().ki, session.config().kd, value, session.iter(), "socket");
            (Response::Set { ok: true, param: String::from("sp"), old, new: value }, SocketSideEffect::None)
        }
        "interval" => {
            let old = args.interval.as_secs_f64();
            let new_interval = Duration::from_secs_f64(value);
            if let Err(e) = apply_runtime_interval(session, args, new_interval) {
                return (
                    Response::ErrorUnknownCommand { ok: false, error: format!("interval change failed: {e}"), available: vec![] },
                    SocketSideEffect::None,
                );
            }
            (Response::Set { ok: true, param: String::from("interval"), old, new: value }, SocketSideEffect::IntervalChanged)
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
                Ok(Some(())) => {},
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
) {
    let Some(err) = err else {
        return;
    };
    emit_state_write_failure(session, state_path, log_file, &err);
}

/// Emits a state write failure — escalated warning if threshold reached, plain log otherwise.
pub(crate) fn emit_state_write_failure(
    session: &ControllerSession,
    state_path: Option<&std::path::PathBuf>,
    log_file: &mut Option<std::fs::File>,
    err: &pid_ctl::app::StateStoreError,
) {
    if let Some(path) = state_path {
        if session.state_fail_escalated() {
            let count = session.state_fail_count();
            eprintln!("WARNING: state write failing persistently ({count} consecutive): {err}");
            json_events::emit_state_write_escalated(log_file, path.clone(), err.to_string(), count);
        } else {
            eprintln!("state write failed: {err}");
            json_events::emit_state_write_failed(log_file, path.clone(), err.to_string());
        }
    } else {
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

fn parse_loop(args: &[String]) -> Result<LoopArgs, CliError> {
    let common = parse_common_args(args, CommandKind::Loop)?;

    // loop does not accept --pv <literal>
    if matches!(common.pv_source, Some(PvSourceConfig::Literal(_))) {
        return Err(CliError::config(
            "loop requires --pv-file or --pv-cmd for PV source — use once for literal PV values",
        ));
    }

    let pv_source = common.pv_source.ok_or_else(|| {
        CliError::config("loop requires a PV source (--pv-file, --pv-cmd, or --pv-stdin)")
    })?;

    if common.tune {
        if matches!(common.output_format, OutputFormat::Json) {
            return Err(CliError::config(
                "--tune and --format json are incompatible",
            ));
        }
        if common.quiet {
            return Err(CliError::config(
                "--tune and --quiet are incompatible — tune requires a TTY",
            ));
        }
        if matches!(pv_source, PvSourceConfig::Stdin) {
            return Err(CliError::config(
                "--tune cannot be used with --pv-stdin — stdin is used for the tuning dashboard",
            ));
        }
        if !io::stdout().is_terminal() {
            return Err(CliError::config(
                "--tune requires a TTY; use --format json for non-interactive output",
            ));
        }
    }

    // CV sink is required unless --dry-run is active.
    if common.cv_sink.is_none() && !common.dry_run {
        return Err(CliError::config(
            "loop requires a CV sink (--cv-file, --cv-cmd, or --cv-stdout)",
        ));
    }

    let mut cv_sink = common.cv_sink;
    if let Some(ref mut sink) = cv_sink {
        apply_cv_cmd_timeout(sink, common.cv_cmd_timeout);
        apply_verify_cv(sink, common.verify_cv);
    }

    let interval = common
        .loop_interval
        .ok_or_else(|| CliError::config("loop requires --interval"))?;

    let pid_config = resolve_pid_config(&common.pid_flags, common.state_path.as_deref())?;

    // Default max_dt is 3×interval or 60.0 if interval is very large.
    let interval_secs = interval.as_secs_f64();
    let max_dt_default = (interval_secs * 3.0).min(60.0);
    // Ensure max_dt_default is never below min_dt default (0.01).
    let max_dt_default = max_dt_default.max(0.01);

    let pv_stdin_timeout = common.pv_stdin_timeout.unwrap_or(interval);
    let effective_cmd_timeout = common.cmd_timeout.unwrap_or(Duration::from_secs(5));
    let pv_cmd_timeout = common.pv_cmd_timeout.unwrap_or(effective_cmd_timeout);

    // Default state_write_interval for loop: max(tick_interval, 100ms).
    let min_flush = Duration::from_millis(100);
    let default_state_write_interval = interval.max(min_flush);
    let state_write_interval = Some(
        common
            .state_write_interval
            .unwrap_or(default_state_write_interval),
    );

    Ok(LoopArgs {
        interval,
        pv_source,
        cv_sink,
        pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
        scale: common.scale.unwrap_or(1.0),
        cv_precision: common.cv_precision.unwrap_or(2) as usize,
        output_format: common.output_format,
        cmd_timeout: effective_cmd_timeout,
        pv_cmd_timeout,
        safe_cv: common.safe_cv,
        cv_fail_after: common.cv_fail_after.unwrap_or(10),
        fail_after: common.fail_after,
        min_dt: common.min_dt.unwrap_or(0.01),
        max_dt: common.max_dt.unwrap_or(max_dt_default),
        dt_clamp: common.dt_clamp,
        log_path: common.log_path,
        dry_run: common.dry_run,
        pv_stdin_timeout,
        verify_cv: common.verify_cv,
        state_write_interval,
        state_fail_after: common.state_fail_after.unwrap_or(10),
        tune: common.tune,
        tune_history: common.tune_history.unwrap_or(60).max(1),
        tune_step_kp: common.tune_step_kp.unwrap_or(0.1),
        tune_step_ki: common.tune_step_ki.unwrap_or(0.01),
        tune_step_kd: common.tune_step_kd.unwrap_or(0.05),
        tune_step_sp: common.tune_step_sp.unwrap_or(0.1),
        units: common.units,
        quiet: common.quiet,
        explicit_max_dt: common.explicit_max_dt,
        explicit_min_dt: common.explicit_min_dt,
        explicit_pv_stdin_timeout: common.explicit_pv_stdin_timeout,
        explicit_state_write_interval: common.explicit_state_write_interval,
        socket_path: common.socket_path,
        socket_mode: common.socket_mode.unwrap_or(0o600),
    })
}

/// If `timeout` is `Some`, sets it on the `Cmd` variant.
const fn apply_cv_cmd_timeout(cv_sink: &mut CvSinkConfig, timeout: Option<Duration>) {
    if let (Some(t), CvSinkConfig::Cmd { timeout: slot, .. }) = (timeout, cv_sink) {
        *slot = Some(t);
    }
}

/// Sets the verify flag on `File` variant sinks.
const fn apply_verify_cv(cv_sink: &mut CvSinkConfig, verify: bool) {
    if let CvSinkConfig::File { verify: slot, .. } = cv_sink {
        *slot = verify;
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

/// Parses a duration string such as `"2s"`, `"500ms"`, `"0.5"`, or `"1.5s"`.
///
/// - Suffix `ms` → milliseconds
/// - Suffix `s` or no suffix → seconds (supports fractional)
pub(crate) fn parse_duration_flag(flag: &str, value: &str) -> Result<Duration, CliError> {
    if let Some(ms_str) = value.strip_suffix("ms") {
        let ms = ms_str.parse::<f64>().map_err(|_| {
            CliError::config(format!(
                "{flag} expects a duration like '2s', '500ms', or '0.5s', got `{value}`"
            ))
        })?;
        return Ok(Duration::from_secs_f64(ms / 1000.0));
    }

    let secs_str = value.strip_suffix('s').unwrap_or(value);
    let secs = secs_str.parse::<f64>().map_err(|_| {
        CliError::config(format!(
            "{flag} expects a duration like '2s', '500ms', or '0.5s', got `{value}`"
        ))
    })?;
    Ok(Duration::from_secs_f64(secs))
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

/// Parses `--state <path>` from a minimal argument list.
///
/// Used by `status`, `purge`, and `init` which only need a state path.
fn parse_state_flag(args: &[String], command: &str) -> Result<PathBuf, CliError> {
    let mut index = 0;
    while index < args.len() {
        if args[index].as_str() == "--state" {
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| CliError::config("--state requires a value"))?;
            return Ok(PathBuf::from(value));
        }
        index += 1;
    }

    Err(CliError::config(format!("{command} requires --state")))
}

struct StatusFlags {
    state_path: Option<PathBuf>,
    socket_path: Option<PathBuf>,
}

fn parse_status_flags(args: &[String]) -> Result<StatusFlags, CliError> {
    let mut state_path = None;
    let mut socket_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--state requires a value"))?;
                state_path = Some(PathBuf::from(val));
            }
            "--socket" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--socket requires a path"))?;
                socket_path = Some(PathBuf::from(val));
            }
            other if other.starts_with('-') => {
                return Err(CliError::config(format!(
                    "status: unrecognized option `{other}`"
                )));
            }
            other => {
                return Err(CliError::config(format!(
                    "status: unexpected argument `{other}`"
                )));
            }
        }
        i += 1;
    }
    if state_path.is_none() && socket_path.is_none() {
        return Err(CliError::config(
            "status requires --state or --socket (or both)",
        ));
    }
    Ok(StatusFlags {
        state_path,
        socket_path,
    })
}

fn run_status_dispatch(flags: &StatusFlags) -> Result<(), CliError> {
    // Try socket first if provided.
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
// Socket control subcommands: set, hold, resume, reset, save
// ---------------------------------------------------------------------------

/// Parse the `--socket <path>` flag required by socket-control subcommands.
fn parse_socket_control_flag(args: &[String], cmd: &str) -> Result<PathBuf, CliError> {
    let mut socket_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--socket requires a path"))?;
                socket_path = Some(PathBuf::from(val));
            }
            other if other.starts_with('-') => {
                return Err(CliError::config(format!(
                    "{cmd}: unrecognized option `{other}`"
                )));
            }
            other => {
                return Err(CliError::config(format!(
                    "{cmd}: unexpected argument `{other}`"
                )));
            }
        }
        i += 1;
    }
    socket_path.ok_or_else(|| CliError::config(format!("{cmd} requires --socket <path>")))
}

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

fn run_socket_hold(args: &[String]) -> Result<(), CliError> {
    let path = parse_socket_control_flag(args, "hold")?;
    socket_send_and_print(&path, &pid_ctl::socket::Request::Hold, "hold")
}

fn run_socket_resume(args: &[String]) -> Result<(), CliError> {
    let path = parse_socket_control_flag(args, "resume")?;
    socket_send_and_print(&path, &pid_ctl::socket::Request::Resume, "resume")
}

fn run_socket_reset(args: &[String]) -> Result<(), CliError> {
    let path = parse_socket_control_flag(args, "reset")?;
    socket_send_and_print(&path, &pid_ctl::socket::Request::Reset, "reset")
}

fn run_socket_save(args: &[String]) -> Result<(), CliError> {
    let path = parse_socket_control_flag(args, "save")?;
    socket_send_and_print(&path, &pid_ctl::socket::Request::Save, "save")
}

struct SetArgs {
    socket_path: PathBuf,
    param: String,
    value: f64,
}

fn parse_set_args(args: &[String]) -> Result<SetArgs, CliError> {
    let mut socket_path = None;
    let mut param: Option<String> = None;
    let mut value: Option<f64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--socket requires a path"))?;
                socket_path = Some(PathBuf::from(val));
            }
            "--param" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--param requires a value"))?;
                param = Some(val.clone());
            }
            "--value" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| CliError::config("--value requires a number"))?;
                value = Some(
                    val.parse::<f64>()
                        .map_err(|_| CliError::config(format!("--value: invalid float `{val}`")))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(CliError::config(format!(
                    "set: unrecognized option `{other}`"
                )));
            }
            other => {
                return Err(CliError::config(format!(
                    "set: unexpected argument `{other}`"
                )));
            }
        }
        i += 1;
    }
    Ok(SetArgs {
        socket_path: socket_path
            .ok_or_else(|| CliError::config("set requires --socket <path>"))?,
        param: param.ok_or_else(|| CliError::config("set requires --param <name>"))?,
        value: value.ok_or_else(|| CliError::config("set requires --value <float>"))?,
    })
}

fn run_socket_set(args: &[String]) -> Result<(), CliError> {
    let parsed = parse_set_args(args)?;
    let req = pid_ctl::socket::Request::Set {
        param: parsed.param,
        value: parsed.value,
    };
    socket_send_and_print(&parsed.socket_path, &req, "set")
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

fn parse_once(args: &[String]) -> Result<OnceArgs, CliError> {
    let common = parse_common_args(args, CommandKind::Once)?;
    let pv_source = common.pv_source.ok_or_else(|| {
        CliError::config(
            "once requires a PV source: --pv <float>, --pv-file <path>, or --pv-cmd <cmd>",
        )
    })?;

    // CV sink is required unless --dry-run is active.
    if common.cv_sink.is_none() && !common.dry_run {
        return Err(CliError::config(
            "once requires exactly one CV sink: --cv-stdout or --cv-file <path>",
        ));
    }

    if matches!(common.output_format, OutputFormat::Json)
        && matches!(common.cv_sink, Some(CvSinkConfig::Stdout))
    {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with --cv-stdout — use --log for machine-readable telemetry",
        ));
    }

    let pid_config = resolve_pid_config(&common.pid_flags, common.state_path.as_deref())?;

    let mut cv_sink = common.cv_sink;
    if let Some(ref mut sink) = cv_sink {
        apply_cv_cmd_timeout(sink, common.cv_cmd_timeout);
        apply_verify_cv(sink, common.verify_cv);
    }

    let effective_cmd_timeout = common.cmd_timeout.unwrap_or(Duration::from_secs(5));
    let pv_cmd_timeout = common.pv_cmd_timeout.unwrap_or(effective_cmd_timeout);

    Ok(OnceArgs {
        pv_source,
        cmd_timeout: effective_cmd_timeout,
        pv_cmd_timeout,
        dt: common.dt,
        dt_explicit: common.dt_explicit,
        min_dt: common.min_dt.unwrap_or(0.01),
        max_dt: common.max_dt.unwrap_or(60.0),
        output_format: common.output_format,
        cv_sink,
        pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
        scale: common.scale.unwrap_or(1.0),
        cv_precision: common.cv_precision.unwrap_or(2) as usize,
        log_path: common.log_path,
        dry_run: common.dry_run,
    })
}

fn parse_pipe(args: &[String]) -> Result<PipeArgs, CliError> {
    let common = parse_common_args(args, CommandKind::Pipe)?;

    if matches!(common.output_format, OutputFormat::Json) {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with pipe's CV output — use --log for machine-readable telemetry",
        ));
    }

    let pid_config = resolve_pid_config(&common.pid_flags, common.state_path.as_deref())?;

    // Default state_write_interval for pipe: 1s.
    let state_write_interval = Some(
        common
            .state_write_interval
            .unwrap_or(Duration::from_secs(1)),
    );

    Ok(PipeArgs {
        dt: common.dt,
        pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
        scale: common.scale.unwrap_or(1.0),
        cv_precision: common.cv_precision.unwrap_or(2) as usize,
        log_path: common.log_path,
        state_write_interval,
        state_fail_after: common.state_fail_after.unwrap_or(10),
    })
}

/// Merges CLI PID flags with any values stored in the state file.
///
/// Priority: CLI flag > state file value > `PidConfig` default.
/// Returns an error (exit 3) if setpoint is absent from both CLI and state file.
fn resolve_pid_config(
    flags: &PidFlags,
    state_path: Option<&std::path::Path>,
) -> Result<PidConfig, CliError> {
    // Load snapshot for config merging (read-only, no lock). ControllerSession::new
    // will re-load under lock for runtime state — safe because save uses atomic rename.
    let snapshot = if let Some(path) = state_path {
        let store = StateStore::new(path);
        store
            .load()
            .map_err(|error| CliError::config(error.to_string()))?
    } else {
        None
    };

    let snap = snapshot.as_ref();

    // Resolve setpoint — required if absent from both CLI and state file.
    let setpoint = match flags.setpoint {
        Some(v) => v,
        None => snap.and_then(|s| s.setpoint).ok_or_else(|| {
            CliError::config("--setpoint is required on first run (no setpoint in state file)")
        })?,
    };

    let defaults = PidConfig::default();

    let kp = flags
        .kp
        .or_else(|| snap.and_then(|s| s.kp))
        .unwrap_or(defaults.kp);
    let ki = flags
        .ki
        .or_else(|| snap.and_then(|s| s.ki))
        .unwrap_or(defaults.ki);
    let kd = flags
        .kd
        .or_else(|| snap.and_then(|s| s.kd))
        .unwrap_or(defaults.kd);
    let out_min = flags
        .out_min
        .or_else(|| snap.and_then(|s| s.out_min))
        .unwrap_or(defaults.out_min);
    let out_max = flags
        .out_max
        .or_else(|| snap.and_then(|s| s.out_max))
        .unwrap_or(defaults.out_max);
    let deadband = flags.deadband.unwrap_or(defaults.deadband);
    let setpoint_ramp = flags.setpoint_ramp.or(defaults.setpoint_ramp);
    let slew_rate = flags.slew_rate.or(defaults.slew_rate);
    let pv_filter_alpha = flags.pv_filter_alpha.unwrap_or(defaults.pv_filter_alpha);

    Ok(PidConfig {
        setpoint,
        kp,
        ki,
        kd,
        out_min,
        out_max,
        deadband,
        setpoint_ramp,
        slew_rate,
        pv_filter_alpha,
        anti_windup: AntiWindupStrategy::BackCalculation,
        anti_windup_tt: None,
        tt_upper_bound: None,
    })
}

fn parse_common_args(args: &[String], command_kind: CommandKind) -> Result<CommonArgs, CliError> {
    let mut parsed = CommonArgs {
        output_format: OutputFormat::Text,
        dt: 1.0,
        dt_explicit: false,
        dt_clamp: false,
        ..CommonArgs::default()
    };

    let mut index = 0;
    while index < args.len() {
        // `handle_loop_only_option` only runs for `loop`; catch tuning flags early for other modes.
        if args[index].starts_with("--tune")
            && matches!(command_kind, CommandKind::Once | CommandKind::Pipe)
        {
            let msg = if matches!(command_kind, CommandKind::Once) {
                "--tune requires loop"
            } else {
                "--tune is unavailable with pipe — pipe is a pure stdin→stdout transformer in v1"
            };
            return Err(CliError::config(msg.to_string()));
        }

        if handle_pid_option(args[index].as_str(), args, &mut index, &mut parsed)? {
            index += 1;
            continue;
        }

        if handle_common_option(args[index].as_str(), args, &mut index, &mut parsed)? {
            index += 1;
            continue;
        }

        if handle_cv_option(
            args[index].as_str(),
            args,
            &mut index,
            command_kind,
            &mut parsed,
        )? {
            index += 1;
            continue;
        }

        if handle_pv_option(
            args[index].as_str(),
            args,
            &mut index,
            command_kind,
            &mut parsed,
        )? {
            index += 1;
            continue;
        }

        if matches!(command_kind, CommandKind::Loop)
            && handle_loop_only_option(args[index].as_str(), args, &mut index, &mut parsed)?
        {
            index += 1;
            continue;
        }

        match args[index].as_str() {
            "--cmd-timeout" => {
                let secs = parse_f64_flag("--cmd-timeout", args, &mut index)?;
                parsed.cmd_timeout = Some(Duration::from_secs_f64(secs));
            }
            "--cv-cmd-timeout" => {
                let secs = parse_f64_flag("--cv-cmd-timeout", args, &mut index)?;
                parsed.cv_cmd_timeout = Some(Duration::from_secs_f64(secs));
            }
            "--pv-cmd-timeout" => {
                let secs = parse_f64_flag("--pv-cmd-timeout", args, &mut index)?;
                parsed.pv_cmd_timeout = Some(Duration::from_secs_f64(secs));
            }
            "--dry-run" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "--dry-run is not meaningful with pipe — pipe has no side effects to suppress",
                    ));
                }

                parsed.dry_run = true;
            }
            unknown if unknown.starts_with("--") => {
                return Err(CliError::config(format!("unrecognized option `{unknown}`")));
            }
            value => {
                return Err(CliError::config(format!(
                    "unexpected positional argument `{value}`"
                )));
            }
        }

        index += 1;
    }

    Ok(parsed)
}

fn handle_loop_only_option(
    flag: &str,
    args: &[String],
    index: &mut usize,
    parsed: &mut CommonArgs,
) -> Result<bool, CliError> {
    match flag {
        "--tune" => {
            parsed.tune = true;
            Ok(true)
        }
        "--tune-history" => {
            let n = parse_u32_flag("--tune-history", args, index)? as usize;
            parsed.tune_history = Some(n.max(1));
            Ok(true)
        }
        "--tune-step-kp" => {
            parsed.tune_step_kp = Some(parse_f64_flag("--tune-step-kp", args, index)?);
            Ok(true)
        }
        "--tune-step-ki" => {
            parsed.tune_step_ki = Some(parse_f64_flag("--tune-step-ki", args, index)?);
            Ok(true)
        }
        "--tune-step-kd" => {
            parsed.tune_step_kd = Some(parse_f64_flag("--tune-step-kd", args, index)?);
            Ok(true)
        }
        "--tune-step-sp" => {
            parsed.tune_step_sp = Some(parse_f64_flag("--tune-step-sp", args, index)?);
            Ok(true)
        }
        "--socket" => {
            parsed.socket_path = Some(parse_path_flag("--socket", args, index)?);
            Ok(true)
        }
        "--socket-mode" => {
            let val = next_value("--socket-mode", args, index)?;
            let mode = u32::from_str_radix(&val, 8).map_err(|_| {
                CliError::config("--socket-mode expects an octal value like 0600 or 0660")
            })?;
            parsed.socket_mode = Some(mode);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_pid_option(
    flag: &str,
    args: &[String],
    index: &mut usize,
    parsed: &mut CommonArgs,
) -> Result<bool, CliError> {
    match flag {
        "--setpoint" => {
            parsed.pid_flags.setpoint = Some(parse_f64_flag("--setpoint", args, index)?);
        }
        "--kp" => {
            parsed.pid_flags.kp = Some(parse_f64_flag("--kp", args, index)?);
        }
        "--ki" => {
            parsed.pid_flags.ki = Some(parse_f64_flag("--ki", args, index)?);
        }
        "--kd" => {
            parsed.pid_flags.kd = Some(parse_f64_flag("--kd", args, index)?);
        }
        "--out-min" => {
            parsed.pid_flags.out_min = Some(parse_f64_flag("--out-min", args, index)?);
        }
        "--out-max" => {
            parsed.pid_flags.out_max = Some(parse_f64_flag("--out-max", args, index)?);
        }
        "--deadband" => {
            parsed.pid_flags.deadband = Some(parse_f64_flag("--deadband", args, index)?);
        }
        "--setpoint-ramp" => {
            parsed.pid_flags.setpoint_ramp = Some(parse_f64_flag("--setpoint-ramp", args, index)?);
        }
        "--slew-rate" => {
            parsed.pid_flags.slew_rate = Some(parse_f64_flag("--slew-rate", args, index)?);
        }
        "--pv-filter" => {
            parsed.pid_flags.pv_filter_alpha = Some(parse_f64_flag("--pv-filter", args, index)?);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

fn handle_common_option(
    flag: &str,
    args: &[String],
    index: &mut usize,
    parsed: &mut CommonArgs,
) -> Result<bool, CliError> {
    match flag {
        "--dt" => {
            parsed.dt = parse_f64_flag("--dt", args, index)?;
            parsed.dt_explicit = true;
        }
        "--dt-clamp" => {
            parsed.dt_clamp = true;
        }
        "--format" => {
            parsed.output_format = parse_output_format(args, index)?;
        }
        "--state" => {
            parsed.state_path = Some(parse_path_flag("--state", args, index)?);
        }
        "--name" => {
            parsed.name = Some(parse_string_flag("--name", args, index)?);
        }
        "--reset-accumulator" => {
            parsed.reset_accumulator = true;
        }
        "--scale" => {
            parsed.scale = Some(parse_f64_flag("--scale", args, index)?);
        }
        "--cv-precision" => {
            parsed.cv_precision = Some(parse_u32_flag("--cv-precision", args, index)?);
        }
        "--interval" => {
            let value = next_value("--interval", args, index)?;
            parsed.loop_interval = Some(parse_duration_flag("--interval", &value)?);
        }
        "--safe-cv" => {
            parsed.safe_cv = Some(parse_f64_flag("--safe-cv", args, index)?);
        }
        "--cv-fail-after" => {
            parsed.cv_fail_after = Some(parse_u32_flag("--cv-fail-after", args, index)?);
        }
        "--fail-after" => {
            parsed.fail_after = Some(parse_u32_flag("--fail-after", args, index)?);
        }
        "--min-dt" => {
            parsed.min_dt = Some(parse_f64_flag("--min-dt", args, index)?);
            parsed.explicit_min_dt = true;
        }
        "--max-dt" => {
            parsed.max_dt = Some(parse_f64_flag("--max-dt", args, index)?);
            parsed.explicit_max_dt = true;
        }
        "--log" => {
            parsed.log_path = Some(parse_path_flag("--log", args, index)?);
        }
        "--state-write-interval" => {
            let value = next_value("--state-write-interval", args, index)?;
            parsed.state_write_interval =
                Some(parse_duration_flag("--state-write-interval", &value)?);
            parsed.explicit_state_write_interval = true;
        }
        "--units" => {
            parsed.units = Some(parse_string_flag("--units", args, index)?);
        }
        "--quiet" => {
            parsed.quiet = true;
        }
        "--state-fail-after" => {
            parsed.state_fail_after = Some(parse_u32_flag("--state-fail-after", args, index)?);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

fn handle_cv_option(
    flag: &str,
    args: &[String],
    index: &mut usize,
    command_kind: CommandKind,
    parsed: &mut CommonArgs,
) -> Result<bool, CliError> {
    let pipe_err = || {
        CliError::config(
            "pipe always writes CV to stdout in v1 — move actuator side effects to the next shell stage",
        )
    };
    match flag {
        "--cv-stdout" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            set_cv_sink(&mut parsed.cv_sink, CvSinkConfig::Stdout)?;
        }
        "--cv-file" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            let path = parse_path_flag("--cv-file", args, index)?;
            set_cv_sink(
                &mut parsed.cv_sink,
                CvSinkConfig::File {
                    path,
                    verify: false,
                },
            )?;
        }
        "--cv-cmd" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            let cmd = parse_string_flag("--cv-cmd", args, index)?;
            set_cv_sink(
                &mut parsed.cv_sink,
                CvSinkConfig::Cmd {
                    command: cmd,
                    timeout: None,
                },
            )?;
        }
        "--verify-cv" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            parsed.verify_cv = true;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn handle_pv_option(
    flag: &str,
    args: &[String],
    index: &mut usize,
    command_kind: CommandKind,
    parsed: &mut CommonArgs,
) -> Result<bool, CliError> {
    let pipe_err = || {
        CliError::config(
            "pipe reads PV from stdin intrinsically — PV source flags are not accepted",
        )
    };
    match flag {
        "--pv" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            if matches!(command_kind, CommandKind::Loop) {
                return Err(CliError::config(
                    "loop requires --pv-file or --pv-cmd for PV source — use once for literal PV values",
                ));
            }
            set_pv_source(
                &mut parsed.pv_source,
                PvSourceConfig::Literal(parse_f64_flag("--pv", args, index)?),
            )?;
        }
        "--pv-file" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            let path = parse_path_flag("--pv-file", args, index)?;
            set_pv_source(&mut parsed.pv_source, PvSourceConfig::File(path))?;
        }
        "--pv-cmd" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            let cmd = parse_string_flag("--pv-cmd", args, index)?;
            set_pv_source(&mut parsed.pv_source, PvSourceConfig::Cmd(cmd))?;
        }
        "--pv-stdin" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            if matches!(command_kind, CommandKind::Once) {
                return Err(CliError::config(
                    "--pv-stdin is only supported with loop — use pipe for externally-timed stdin reads",
                ));
            }
            set_pv_source(&mut parsed.pv_source, PvSourceConfig::Stdin)?;
        }
        "--pv-stdin-timeout" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            if matches!(command_kind, CommandKind::Once) {
                return Err(CliError::config(
                    "--pv-stdin-timeout is only supported with loop",
                ));
            }
            let value = next_value("--pv-stdin-timeout", args, index)?;
            parsed.pv_stdin_timeout = Some(parse_duration_flag("--pv-stdin-timeout", &value)?);
            parsed.explicit_pv_stdin_timeout = true;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_cv_sink(current: &mut Option<CvSinkConfig>, next: CvSinkConfig) -> Result<(), CliError> {
    if current.is_some() {
        return Err(CliError::config("only one CV sink may be specified"));
    }

    *current = Some(next);
    Ok(())
}

fn set_pv_source(
    current: &mut Option<PvSourceConfig>,
    next: PvSourceConfig,
) -> Result<(), CliError> {
    if current.is_some() {
        return Err(CliError::config("only one PV source may be specified"));
    }

    *current = Some(next);
    Ok(())
}

fn resolve_pv(source: &PvSourceConfig, cmd_timeout: Duration) -> io::Result<f64> {
    match source {
        PvSourceConfig::Literal(v) => Ok(*v),
        PvSourceConfig::File(path) => FilePvSource::new(path.clone()).read_pv(),
        PvSourceConfig::Cmd(cmd) => CmdPvSource::new(cmd.clone(), cmd_timeout).read_pv(),
        PvSourceConfig::Stdin => unreachable!("--pv-stdin is only valid for loop, not once"),
    }
}

fn parse_output_format(args: &[String], index: &mut usize) -> Result<OutputFormat, CliError> {
    let value = next_value("--format", args, index)?;
    match value.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => Err(CliError::config(format!(
            "--format must be `text` or `json`, got `{other}`"
        ))),
    }
}

fn parse_f64_flag(flag: &str, args: &[String], index: &mut usize) -> Result<f64, CliError> {
    let value = next_value(flag, args, index)?;
    parse_f64_value(flag, &value)
}

fn parse_f64_value(flag: &str, value: &str) -> Result<f64, CliError> {
    value.parse::<f64>().map_err(|error| {
        CliError::config(format!("{flag} expects a float, got `{value}`: {error}"))
    })
}

fn parse_u32_flag(flag: &str, args: &[String], index: &mut usize) -> Result<u32, CliError> {
    let value = next_value(flag, args, index)?;
    value.parse::<u32>().map_err(|error| {
        CliError::config(format!(
            "{flag} expects a non-negative integer, got `{value}`: {error}"
        ))
    })
}

fn parse_path_flag(flag: &str, args: &[String], index: &mut usize) -> Result<PathBuf, CliError> {
    next_value(flag, args, index).map(PathBuf::from)
}

fn parse_string_flag(flag: &str, args: &[String], index: &mut usize) -> Result<String, CliError> {
    next_value(flag, args, index)
}

fn next_value(flag: &str, args: &[String], index: &mut usize) -> Result<String, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError::config(format!("{flag} requires a value")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandKind {
    Once,
    Pipe,
    Loop,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OutputFormat {
    #[default]
    Text,
    Json,
}

/// PID parameters as optional values — `None` means "not set on CLI".
#[derive(Clone, Debug, Default, PartialEq)]
struct PidFlags {
    setpoint: Option<f64>,
    kp: Option<f64>,
    ki: Option<f64>,
    kd: Option<f64>,
    out_min: Option<f64>,
    out_max: Option<f64>,
    deadband: Option<f64>,
    setpoint_ramp: Option<f64>,
    slew_rate: Option<f64>,
    pv_filter_alpha: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
struct CommonArgs {
    pv_source: Option<PvSourceConfig>,
    pid_flags: PidFlags,
    output_format: OutputFormat,
    dt: f64,
    /// True when `--dt` was passed (fixed dt; bypasses wall-clock / bounds where documented).
    dt_explicit: bool,
    /// Clamp measured dt to `[min_dt, max_dt]` instead of skipping (`loop` / `pipe`; `once` auto-dt always clamps).
    dt_clamp: bool,
    cv_sink: Option<CvSinkConfig>,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
    scale: Option<f64>,
    cv_precision: Option<u32>,
    cmd_timeout: Option<Duration>,
    cv_cmd_timeout: Option<Duration>,
    pv_cmd_timeout: Option<Duration>,
    loop_interval: Option<Duration>,
    safe_cv: Option<f64>,
    cv_fail_after: Option<u32>,
    fail_after: Option<u32>,
    min_dt: Option<f64>,
    max_dt: Option<f64>,
    log_path: Option<PathBuf>,
    dry_run: bool,
    pv_stdin_timeout: Option<Duration>,
    verify_cv: bool,
    state_write_interval: Option<Duration>,
    state_fail_after: Option<u32>,
    /// Loop + dashboard only (`--tune`, `--tune-history`, `--tune-step-*`).
    tune: bool,
    tune_history: Option<usize>,
    tune_step_kp: Option<f64>,
    tune_step_ki: Option<f64>,
    tune_step_kd: Option<f64>,
    tune_step_sp: Option<f64>,
    units: Option<String>,
    quiet: bool,
    explicit_max_dt: bool,
    explicit_min_dt: bool,
    explicit_pv_stdin_timeout: bool,
    explicit_state_write_interval: bool,
    socket_path: Option<PathBuf>,
    socket_mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
struct OnceArgs {
    pv_source: PvSourceConfig,
    cmd_timeout: Duration,
    pv_cmd_timeout: Duration,
    dt: f64,
    dt_explicit: bool,
    min_dt: f64,
    max_dt: f64,
    output_format: OutputFormat,
    cv_sink: Option<CvSinkConfig>,
    pid_config: PidConfig,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
    scale: f64,
    cv_precision: usize,
    log_path: Option<PathBuf>,
    dry_run: bool,
}

/// Which PV source was specified on the CLI.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PvSourceConfig {
    Literal(f64),
    File(PathBuf),
    Cmd(String),
    /// `loop --pv-stdin`: one line per tick, with a per-tick timeout.
    Stdin,
}

impl OnceArgs {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            name: self.name.clone(),
            pid: self.pid_config.clone(),
            state_store: self.state_path.clone().map(StateStore::new),
            reset_accumulator: self.reset_accumulator,
            // once always writes every tick (no coalescing)
            flush_interval: None,
            state_fail_after: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PipeArgs {
    dt: f64,
    pid_config: PidConfig,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
    scale: f64,
    cv_precision: usize,
    log_path: Option<PathBuf>,
    state_write_interval: Option<Duration>,
    state_fail_after: u32,
}

impl PipeArgs {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            name: self.name.clone(),
            pid: self.pid_config.clone(),
            state_store: self.state_path.clone().map(StateStore::new),
            reset_accumulator: self.reset_accumulator,
            flush_interval: self.state_write_interval,
            state_fail_after: self.state_fail_after,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct LoopArgs {
    pub(crate) interval: Duration,
    pub(crate) pv_source: PvSourceConfig,
    pub(crate) cv_sink: Option<CvSinkConfig>,
    pub(crate) pid_config: PidConfig,
    pub(crate) state_path: Option<PathBuf>,
    pub(crate) name: Option<String>,
    reset_accumulator: bool,
    pub(crate) scale: f64,
    pub(crate) cv_precision: usize,
    pub(crate) output_format: OutputFormat,
    pub(crate) cmd_timeout: Duration,
    pub(crate) pv_cmd_timeout: Duration,
    pub(crate) safe_cv: Option<f64>,
    pub(crate) cv_fail_after: u32,
    pub(crate) fail_after: Option<u32>,
    pub(crate) min_dt: f64,
    pub(crate) max_dt: f64,
    pub(crate) dt_clamp: bool,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) pv_stdin_timeout: Duration,
    pub(crate) verify_cv: bool,
    pub(crate) state_write_interval: Option<Duration>,
    state_fail_after: u32,
    pub(crate) tune: bool,
    pub(crate) tune_history: usize,
    pub(crate) tune_step_kp: f64,
    pub(crate) tune_step_ki: f64,
    pub(crate) tune_step_kd: f64,
    pub(crate) tune_step_sp: f64,
    pub(crate) units: Option<String>,
    pub(crate) quiet: bool,
    pub(crate) explicit_max_dt: bool,
    pub(crate) explicit_min_dt: bool,
    pub(crate) explicit_pv_stdin_timeout: bool,
    pub(crate) explicit_state_write_interval: bool,
    pub(crate) socket_path: Option<PathBuf>,
    pub(crate) socket_mode: u32,
}

impl LoopArgs {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            name: self.name.clone(),
            pid: self.pid_config.clone(),
            state_store: self.state_path.clone().map(StateStore::new),
            reset_accumulator: self.reset_accumulator,
            flush_interval: self.state_write_interval,
            state_fail_after: self.state_fail_after,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CvSinkConfig {
    Stdout,
    File {
        path: PathBuf,
        verify: bool,
    },
    Cmd {
        command: String,
        timeout: Option<Duration>,
    },
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
