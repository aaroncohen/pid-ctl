//! Interactive `loop --tune` dashboard (`pid-ctl_plan.md` § `--tune`).

mod model;

mod export;
mod history;
mod input;
mod render;

use crate::CliError;
use crate::LoopArgs;
use crate::OutputFormat;
use crate::print_iteration_json;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use export::{export_line_stderr, flush_shutdown};
use input::{handle_command_key, handle_normal_key};
use model::{TUNE_IDLE_DRAW_DEADLINE_NEAR, TUNE_IDLE_DRAW_MIN, TuneUiState};
use pid_ctl::adapters::{CvSink, DryRunCvSink};
use pid_ctl::app::adapters_build::{build_cv_sink, build_pv_source};
use pid_ctl::app::logger::Logger;
use pid_ctl::app::loop_runtime::{
    MeasuredDt, apply_measured_dt, emit_state_write_failure, handle_dt_skip_state_write,
    write_safe_cv,
};
use pid_ctl::app::{ControllerSession, TickOutcome};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use ratatui::Terminal;
use render::draw;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs the interactive tuning dashboard until the operator quits or a fatal loop error occurs.
// The event loop integrates input, PID ticking, socket servicing, and throttled redraws in one
// place — splitting it further would require passing state through many helper boundaries.
#[allow(clippy::too_many_lines)]
pub fn run(mut args: LoopArgs, full_argv: &[String]) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|e| CliError::config(e.to_string()))?;
    let cfg0 = session.config().clone();
    let mut ui = TuneUiState::new(&args);
    ui.last_kp = cfg0.kp;
    ui.last_ki = cfg0.ki;
    ui.last_kd = cfg0.kd;
    ui.last_sp = cfg0.setpoint;

    let mut pv_source =
        build_pv_source(&args.pv_source, args.pv_cmd_timeout, args.pv_stdin_timeout);
    let mut hardware: Option<Box<dyn CvSink>> = args
        .cv_sink
        .as_ref()
        .map(|cfg| build_cv_sink(cfg, args.cv_precision, args.cmd_timeout));
    // Suppress stderr so JSON event lines don't interleave with the ratatui alternate-screen TUI.
    let mut logger = Logger::open(args.log_path.as_deref())
        .map_err(|e| {
            CliError::new(
                1,
                format!(
                    "failed to open log file {}: {e}",
                    args.log_path.as_deref().unwrap().display()
                ),
            )
        })?
        .suppressed();

    // Bind socket listener when --socket is set.
    let socket_listener = if let Some(ref path) = args.socket_path {
        Some(
            pid_ctl::socket::SocketListener::bind(path, args.socket_mode)
                .map_err(|e| CliError::new(3, format!("socket: {e}")))?,
        )
    } else {
        None
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::Relaxed);
    })
    .map_err(|e| CliError::new(1, format!("signal handler: {e}")))?;

    let mut stdout = io::stdout();
    enable_raw_mode().map_err(|e| CliError::new(1, format!("terminal: {e}")))?;
    let _ = execute!(stdout, EnterAlternateScreen);
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| CliError::new(1, format!("terminal: {e}")))?;

    let mut next_deadline = Instant::now() + args.interval;
    let mut last_interval = args.interval;
    let mut last_tick = Instant::now();
    let mut last_idle_draw = Instant::now()
        .checked_sub(TUNE_IDLE_DRAW_MIN)
        .unwrap_or_else(Instant::now);
    let mut cv_fail_count: u32 = 0;
    let mut pv_fail_count: u32 = 0;

    let run_result = (|| -> Result<(), CliError> {
        loop {
            if shutdown.load(Ordering::Relaxed) || ui.quit {
                export_line_stderr(full_argv, &args);
                flush_shutdown(&mut session, &args, &mut logger);
                return Ok(());
            }

            let now = Instant::now();
            let until_deadline = next_deadline.saturating_duration_since(now);
            let poll_wait = until_deadline.min(Duration::from_millis(50));

            let mut had_terminal_event = false;
            if event::poll(poll_wait).map_err(|e| CliError::new(1, format!("event poll: {e}")))? {
                match event::read().map_err(|e| CliError::new(1, format!("event read: {e}")))? {
                    Event::Resize(_, _) => {
                        had_terminal_event = true;
                    }
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        had_terminal_event = true;
                        if ui.export_overlay.is_some() {
                            ui.export_overlay = None;
                        } else if ui.help_overlay
                            && matches!(key.code, KeyCode::Esc | KeyCode::Char('?'))
                        {
                            ui.help_overlay = false;
                        } else if ui.command_mode {
                            handle_command_key(
                                &mut ui,
                                &mut session,
                                &mut args,
                                key,
                                full_argv,
                                &mut logger,
                            )?;
                        } else {
                            handle_normal_key(
                                &mut ui,
                                &mut session,
                                &mut args,
                                key,
                                full_argv,
                                &mut logger,
                            );
                        }
                    }
                    _ => {}
                }
            }

            // Service socket connections between ticks.
            #[cfg(unix)]
            if let Some(ref listener) = socket_listener {
                use pid_ctl::app::socket_dispatch::{SocketSideEffect, handle_socket_request};
                for _ in 0..10 {
                    match listener.try_service_one(|req| {
                        let (resp, effect) =
                            handle_socket_request(&req, &mut session, &mut args, &mut logger);
                        match effect {
                            SocketSideEffect::Hold => ui.hold = true,
                            SocketSideEffect::Resume => ui.hold = false,
                            SocketSideEffect::IntervalChanged | SocketSideEffect::None => {}
                        }
                        resp
                    }) {
                        Ok(Some(())) => {}
                        _ => break,
                    }
                }
            }

            if args.interval != last_interval {
                last_interval = args.interval;
                next_deadline = Instant::now() + args.interval;
            }

            let now = Instant::now();
            let interval_secs = args.interval.as_secs_f64();
            if now < next_deadline {
                let until = next_deadline.saturating_duration_since(now);
                let should_idle_draw = had_terminal_event
                    || until < TUNE_IDLE_DRAW_DEADLINE_NEAR
                    || now.duration_since(last_idle_draw) >= TUNE_IDLE_DRAW_MIN;
                if should_idle_draw {
                    draw(&mut terminal, &session, &args, &ui, interval_secs, until)?;
                    last_idle_draw = now;
                }
                continue;
            }

            let tick_deadline = next_deadline;
            next_deadline = next_deadline_after_tick(tick_deadline, args.interval, now);
            let raw_dt = now.duration_since(last_tick).as_secs_f64();
            last_tick = now;

            // Do not emit `interval_slip` here: interactive `--tune` interleaves keyboard polling,
            // PV/CV subprocess work, and throttled full-frame redraws. Measured `raw_dt` between tick
            // starts can still exceed `--interval` without indicating an unattended scheduling
            // problem (see Reliability §10 — `interval_slip` remains for non-tune `loop` in `main.rs`).

            let dt = match apply_measured_dt(
                raw_dt,
                args.min_dt,
                args.max_dt,
                args.dt_clamp,
                false, // tune is never quiet (tune and --quiet are mutually exclusive)
                &mut logger,
            ) {
                MeasuredDt::Skip => {
                    handle_dt_skip_state_write(
                        session.on_dt_skipped(),
                        &session,
                        args.state_path.as_ref(),
                        &mut logger,
                        false,
                    );
                    draw(
                        &mut terminal,
                        &session,
                        &args,
                        &ui,
                        interval_secs,
                        next_deadline.saturating_duration_since(Instant::now()),
                    )?;
                    last_idle_draw = Instant::now();
                    continue;
                }
                MeasuredDt::Use(dt) => dt,
            };

            let raw_pv = match pv_source.read_pv() {
                Ok(pv) => {
                    pv_fail_count = 0;
                    pv
                }
                Err(error) => {
                    pv_fail_count += 1;
                    json_events::emit_pv_read_failure(&mut logger, error.to_string(), args.safe_cv);
                    if let Some(ref mut sink) = hardware {
                        write_safe_cv(args.safe_cv, sink.as_mut(), &mut session);
                    }
                    if let Some(limit) = args.fail_after
                        && pv_fail_count >= limit
                    {
                        return Err(CliError::new(
                            2,
                            format!("exiting after {pv_fail_count} consecutive PV read failures"),
                        ));
                    }
                    draw(
                        &mut terminal,
                        &session,
                        &args,
                        &ui,
                        interval_secs,
                        next_deadline.saturating_duration_since(Instant::now()),
                    )?;
                    last_idle_draw = Instant::now();
                    continue;
                }
            };

            let scaled_pv = raw_pv * args.scale;

            if ui.hold {
                let held = session
                    .last_applied_cv()
                    .unwrap_or_else(|| ui.last_record.as_ref().map_or(0.0, |r| r.cv));
                let mut dry = DryRunCvSink;
                let active: &mut dyn CvSink = if ui.dry_run {
                    &mut dry
                } else {
                    match hardware.as_mut() {
                        Some(s) => s.as_mut(),
                        None => {
                            return Err(CliError::config(
                                "cannot disable dry-run without a CV sink — specify --cv-file, --cv-cmd, or --cv-stdout",
                            ));
                        }
                    }
                };
                match session.hold_tick_write(scaled_pv, held, active) {
                    Ok(h) => {
                        if let Some(e) = h.state_write_failed {
                            emit_state_write_failure(
                                &session,
                                args.state_path.as_ref(),
                                &mut logger,
                                &e,
                                false,
                            );
                        }
                    }
                    Err(e) => return Err(CliError::new(2, e.to_string())),
                }
                if let Ok(sz) = terminal.size() {
                    ui.spark_w = sz.width.saturating_sub(4) as usize;
                }
                ui.push_history(&args, scaled_pv, held, session.config().setpoint);
            } else {
                let mut dry = DryRunCvSink;
                let active: &mut dyn CvSink = if ui.dry_run {
                    &mut dry
                } else {
                    match hardware.as_mut() {
                        Some(s) => s.as_mut(),
                        None => {
                            return Err(CliError::config(
                                "cannot disable dry-run without a CV sink — specify --cv-file, --cv-cmd, or --cv-stdout",
                            ));
                        }
                    }
                };
                match tune_tick(
                    &args,
                    raw_pv,
                    dt,
                    &mut session,
                    active,
                    &mut logger,
                    &mut cv_fail_count,
                ) {
                    Ok(Some(outcome)) => {
                        ui.last_record = Some(outcome.record.clone());
                        if let Ok(sz) = terminal.size() {
                            ui.spark_w = sz.width.saturating_sub(4) as usize;
                        }
                        ui.push_history(
                            &args,
                            outcome.record.pv,
                            outcome.record.cv,
                            session.config().setpoint,
                        );
                    }
                    Ok(None) => {}
                    Err(e) => return Err(e),
                }
            }

            draw(
                &mut terminal,
                &session,
                &args,
                &ui,
                interval_secs,
                next_deadline.saturating_duration_since(Instant::now()),
            )?;
            last_idle_draw = Instant::now();
        }
    })();

    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);

    run_result
}

/// Like `run_loop_tick` but returns the outcome for the dashboard; skips JSON on stdout (TUI owns the screen).
fn tune_tick(
    args: &LoopArgs,
    raw_pv: f64,
    dt: f64,
    session: &mut ControllerSession,
    cv_sink: &mut dyn CvSink,
    logger: &mut Logger,
    cv_fail_count: &mut u32,
) -> Result<Option<TickOutcome>, CliError> {
    let scaled_pv = raw_pv * args.scale;

    match session.process_pv(scaled_pv, dt, cv_sink) {
        Ok(outcome) => {
            *cv_fail_count = 0;

            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(logger, reason, outcome.record.iter);
            }

            if matches!(args.output_format, OutputFormat::Json) {
                let _ = print_iteration_json(&outcome.record);
            }

            logger.write_iteration_line(&outcome.record);

            if let Some(ref error) = outcome.state_write_failed {
                emit_state_write_failure(session, args.state_path.as_ref(), logger, error, false);
            }

            Ok(Some(outcome))
        }
        Err(error) => {
            *cv_fail_count += 1;
            let limit = args.cv_fail_after;
            json_events::emit_cv_write_failed(logger, error.to_string(), *cv_fail_count);
            if *cv_fail_count >= limit {
                write_safe_cv(args.safe_cv, cv_sink, session);
                return Err(CliError::new(
                    2,
                    format!("exiting after {cv_fail_count} consecutive CV write failures: {error}"),
                ));
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests;

// Additional imports used only in tests — items not needed by the non-test event loop.
#[cfg(test)]
use export::build_export_line_values;
#[cfg(test)]
use history::{
    annotation_caret_line, expand_scale, history_range, history_trend, scale_ticks, spark_data,
    spark_marker_row,
};
#[cfg(test)]
use input::{needed_decimals, step_125_down, step_125_up};
#[cfg(test)]
use model::{GainAnnotation, GainFocus};
#[cfg(test)]
use render::render_frame;
