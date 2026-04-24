mod cli;
#[cfg(feature = "tui")]
mod tune;
#[allow(clippy::wildcard_imports)]
pub(crate) use cli::*;

use clap::Parser;
use pid_ctl::adapters::{CvSink, DryRunCvSink, StdoutCvSink};
use pid_ctl::app::adapters_build::{build_cv_sink, build_pv_source};
use pid_ctl::app::logger::Logger;
use pid_ctl::app::loop_runtime::{
    LoopControls, MeasuredDt, apply_measured_dt, emit_state_write_failure, flush_state_at_shutdown,
    handle_dt_skip_state_write, millis_round_u64, write_safe_cv,
};
use pid_ctl::app::ticker::{self, TickContext, TickObserver, TickStepResult};
use pid_ctl::app::{self, ControllerSession, StateSnapshot, StateStore, TickError, TickOutcome};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use pid_ctl::app::socket_dispatch::{SocketSideEffect, handle_socket_request};

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

fn open_log(path: Option<&Path>) -> Result<Logger, CliError> {
    Logger::open(path).map_err(|e| {
        CliError::new(
            1,
            format!("failed to open log file {}: {e}", path.unwrap().display()),
        )
    })
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
    let mut logger = open_log(args.log_path.as_deref())?;

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

fn run_pipe(args: &PipeArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = StdoutCvSink {
        precision: args.cv_precision,
    };
    let mut logger = open_log(args.log_path.as_deref())?;

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

#[allow(clippy::too_many_lines)]
fn run_loop(args: &mut LoopArgs) -> Result<(), CliError> {
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

    let mut logger = open_log(args.log_path.as_deref())?;

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

#[cfg(unix)]
/// Sleeps until `until` in 50ms chunks, servicing socket connections between chunks.
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
