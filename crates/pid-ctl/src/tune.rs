//! Interactive `loop --tune` dashboard (`pid-ctl_plan.md` § `--tune`).

use crate::CliError;
use crate::LoopArgs;
use crate::MeasuredDt;
use crate::OutputFormat;
use crate::apply_measured_dt;
use crate::build_cv_sink;
use crate::build_pv_source;
use crate::emit_state_write_failure;
use crate::handle_dt_skip_state_write;
use crate::open_log_optional;
use crate::print_iteration_json;
use crate::write_safe_cv;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use pid_ctl::adapters::{CvSink, DryRunCvSink};
use pid_ctl::app::{self, ControllerSession, TickOutcome};
use pid_ctl::json_events;
use pid_ctl::schedule::next_deadline_after_tick;
use pid_ctl_core::PidConfig;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GainFocus {
    Kp,
    Ki,
    Kd,
    Sp,
}

/// Sparkline gain-change annotation: merges within 3 ticks; `marker_tick` selects the `|` column.
#[derive(Clone, Debug)]
struct GainAnnotation {
    /// Tick column for the `|` marker (latest tick in the merge group).
    marker_tick: u64,
    kp: Option<(f64, f64)>,
    ki: Option<(f64, f64)>,
    kd: Option<(f64, f64)>,
    sp: Option<(f64, f64)>,
}

impl GainAnnotation {
    fn display_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some((f, t)) = self.kp {
            parts.push(format!("Kp {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.ki {
            parts.push(format!("Ki {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.kd {
            parts.push(format!("Kd {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.sp {
            parts.push(format!("SP {f:.3}→{t:.3}"));
        }
        parts.join("  ")
    }
}

impl GainFocus {
    const fn next(self) -> Self {
        match self {
            Self::Kp => Self::Ki,
            Self::Ki => Self::Kd,
            Self::Kd => Self::Sp,
            Self::Sp => Self::Kp,
        }
    }

    const fn prev(self) -> Self {
        match self {
            Self::Kp => Self::Sp,
            Self::Ki => Self::Kp,
            Self::Kd => Self::Ki,
            Self::Sp => Self::Kd,
        }
    }

    const fn idx(self) -> usize {
        match self {
            Self::Kp => 0,
            Self::Ki => 1,
            Self::Kd => 2,
            Self::Sp => 3,
        }
    }
}

// The four bools are independent, orthogonal flags (command_mode, help_overlay, hold, quit/dry_run).
// An enum-per-flag state machine would add complexity with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
struct TuneUiState {
    focus: GainFocus,
    step: [f64; 4],
    command_mode: bool,
    command_buf: String,
    help_overlay: bool,
    hold: bool,
    dry_run: bool,
    last_record: Option<app::IterationRecord>,
    pv_history: VecDeque<f64>,
    cv_history: VecDeque<f64>,
    /// Parallel to PV/CV history — tick id for sparkline column mapping.
    serial_history: VecDeque<u64>,
    tick_serial: u64,
    annotations: VecDeque<GainAnnotation>,
    /// Last-known sparkline width (terminal columns). Updated each render so
    /// history is kept long enough to fill the screen even when `tune_history` is small.
    spark_w: usize,
    last_kp: f64,
    last_ki: f64,
    last_kd: f64,
    last_sp: f64,
    start: Instant,
    quit: bool,
    status_flash: Option<(String, Instant)>,
    export_overlay: Option<String>,
}

impl TuneUiState {
    fn new(args: &LoopArgs) -> Self {
        Self {
            focus: GainFocus::Kp,
            step: [
                args.tune_step_kp,
                args.tune_step_ki,
                args.tune_step_kd,
                args.tune_step_sp,
            ],
            command_mode: false,
            command_buf: String::new(),
            help_overlay: false,
            hold: false,
            dry_run: args.dry_run,
            last_record: None,
            pv_history: VecDeque::new(),
            cv_history: VecDeque::new(),
            serial_history: VecDeque::new(),
            tick_serial: 0,
            annotations: VecDeque::new(),
            spark_w: args.tune_history,
            last_kp: f64::NAN,
            last_ki: f64::NAN,
            last_kd: f64::NAN,
            last_sp: f64::NAN,
            start: Instant::now(),
            quit: false,
            status_flash: None,
            export_overlay: None,
        }
    }

    fn push_history(&mut self, args: &LoopArgs, pv: f64, cv: f64) {
        // Keep enough history to fill the terminal width even when tune_history < spark_w.
        let cap = args.tune_history.max(self.spark_w);
        while self.pv_history.len() >= cap {
            self.pv_history.pop_front();
            self.cv_history.pop_front();
            self.serial_history.pop_front();
        }
        self.tick_serial = self.tick_serial.saturating_add(1);
        self.pv_history.push_back(pv);
        self.cv_history.push_back(cv);
        self.serial_history.push_back(self.tick_serial);
        while self.annotations.len() > cap {
            self.annotations.pop_front();
        }
    }

    fn note_gain_change(&mut self, _args: &LoopArgs, cfg: &PidConfig) {
        let kp_changed = self.last_kp.is_finite() && (cfg.kp - self.last_kp).abs() > f64::EPSILON;
        let ki_changed = self.last_ki.is_finite() && (cfg.ki - self.last_ki).abs() > f64::EPSILON;
        let kd_changed = self.last_kd.is_finite() && (cfg.kd - self.last_kd).abs() > f64::EPSILON;
        let sp_changed =
            self.last_sp.is_finite() && (cfg.setpoint - self.last_sp).abs() > f64::EPSILON;
        let prev_kp = self.last_kp;
        let prev_ki = self.last_ki;
        let prev_kd = self.last_kd;
        let prev_sp = self.last_sp;
        self.last_kp = cfg.kp;
        self.last_ki = cfg.ki;
        self.last_kd = cfg.kd;
        self.last_sp = cfg.setpoint;
        if !kp_changed && !ki_changed && !kd_changed && !sp_changed {
            return;
        }
        if let Some(ann) = self.annotations.back_mut()
            && self.tick_serial.saturating_sub(ann.marker_tick) <= 3
        {
            if kp_changed {
                ann.kp = Some(ann.kp.map_or((prev_kp, cfg.kp), |(from, _)| (from, cfg.kp)));
            }
            if ki_changed {
                ann.ki = Some(ann.ki.map_or((prev_ki, cfg.ki), |(from, _)| (from, cfg.ki)));
            }
            if kd_changed {
                ann.kd = Some(ann.kd.map_or((prev_kd, cfg.kd), |(from, _)| (from, cfg.kd)));
            }
            if sp_changed {
                ann.sp = Some(
                    ann.sp
                        .map_or((prev_sp, cfg.setpoint), |(from, _)| (from, cfg.setpoint)),
                );
            }
            ann.marker_tick = self.tick_serial;
            return;
        }
        self.annotations.push_back(GainAnnotation {
            marker_tick: self.tick_serial,
            kp: kp_changed.then_some((prev_kp, cfg.kp)),
            ki: ki_changed.then_some((prev_ki, cfg.ki)),
            kd: kd_changed.then_some((prev_kd, cfg.kd)),
            sp: sp_changed.then_some((prev_sp, cfg.setpoint)),
        });
    }
}

/// Minimum time between full-frame redraws while waiting for the next PID tick.
///
/// Without this, the loop redraws on every `event::poll` wakeup (~20 Hz for a 50 ms cap), which
/// steals wall time from the subprocess + controller work and stretches measured `raw_dt` away
/// from `--interval` — undermining tuning on a production-like cadence.
const TUNE_IDLE_DRAW_MIN: Duration = Duration::from_millis(200);
/// Redraw at least this often when the next tick is near so the countdown stays legible.
const TUNE_IDLE_DRAW_DEADLINE_NEAR: Duration = Duration::from_millis(120);

/// Runs the interactive tuning dashboard until the operator quits or a fatal loop error occurs.
// The event loop integrates input, PID ticking, socket servicing, and throttled redraws in one
// place — splitting it further would require passing state through many helper boundaries.
#[allow(clippy::too_many_lines)]
pub fn run(mut args: LoopArgs, full_argv: &[String]) -> Result<(), CliError> {
    let _suppress_structured_json_stderr = json_events::suppress_structured_json_stderr();
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
    let mut log_file = open_log_optional(args.log_path.as_deref())?;

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
                flush_shutdown(&mut session, &args, &mut log_file);
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
                                &mut log_file,
                            )?;
                        } else {
                            handle_normal_key(
                                &mut ui,
                                &mut session,
                                &mut args,
                                key,
                                full_argv,
                                &mut log_file,
                            );
                        }
                    }
                    _ => {}
                }
            }

            // Service socket connections between ticks.
            if let Some(ref listener) = socket_listener {
                for _ in 0..10 {
                    match listener.try_service_one(|req| {
                        let (resp, effect) = crate::handle_socket_request(
                            &req,
                            &mut session,
                            &mut args,
                            &mut log_file,
                        );
                        match effect {
                            crate::SocketSideEffect::Hold => ui.hold = true,
                            crate::SocketSideEffect::Resume => ui.hold = false,
                            crate::SocketSideEffect::IntervalChanged
                            | crate::SocketSideEffect::None => {}
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
                &mut log_file,
            ) {
                MeasuredDt::Skip => {
                    handle_dt_skip_state_write(
                        session.on_dt_skipped(),
                        &session,
                        args.state_path.as_ref(),
                        &mut log_file,
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
                    json_events::emit_pv_read_failure(
                        &mut log_file,
                        error.to_string(),
                        args.safe_cv,
                    );
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
                                &mut log_file,
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
                ui.push_history(&args, scaled_pv, held);
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
                    &mut log_file,
                    &mut cv_fail_count,
                ) {
                    Ok(Some(outcome)) => {
                        ui.last_record = Some(outcome.record.clone());
                        if let Ok(sz) = terminal.size() {
                            ui.spark_w = sz.width.saturating_sub(4) as usize;
                        }
                        ui.push_history(&args, outcome.record.pv, outcome.record.cv);
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
    log_file: &mut Option<std::fs::File>,
    cv_fail_count: &mut u32,
) -> Result<Option<TickOutcome>, CliError> {
    let scaled_pv = raw_pv * args.scale;

    match session.process_pv(scaled_pv, dt, cv_sink) {
        Ok(outcome) => {
            *cv_fail_count = 0;

            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(log_file, reason, outcome.record.iter);
            }

            if matches!(args.output_format, OutputFormat::Json) {
                let _ = print_iteration_json(&outcome.record);
            }

            if let Some(file) = log_file.as_mut()
                && let Ok(json) = serde_json::to_string(&outcome.record)
            {
                let _ = writeln!(file, "{json}");
            }

            if let Some(ref error) = outcome.state_write_failed {
                emit_state_write_failure(session, args.state_path.as_ref(), log_file, error, false);
            }

            Ok(Some(outcome))
        }
        Err(error) => {
            *cv_fail_count += 1;
            let limit = args.cv_fail_after;
            json_events::emit_cv_write_failed(log_file, error.to_string(), *cv_fail_count);
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

fn flush_shutdown(
    session: &mut ControllerSession,
    args: &LoopArgs,
    log_file: &mut Option<std::fs::File>,
) {
    if let Some(err) = session.force_flush() {
        eprintln!("state write failed at shutdown: {err}");
        if let Some(path) = &args.state_path {
            json_events::emit_state_write_failed(log_file, path.clone(), err.to_string());
        }
    }
}

fn export_line_stderr(full_argv: &[String], args: &LoopArgs) {
    eprintln!("{}", build_export_line(full_argv, args));
    eprintln!("{}", build_tuned_gains_only_line(args));
}

fn is_tune_cli_flag(s: &str) -> bool {
    matches!(
        s,
        "--tune"
            | "--tune-history"
            | "--tune-step-kp"
            | "--tune-step-ki"
            | "--tune-step-kd"
            | "--tune-step-sp"
    )
}

/// Number of value tokens after a `--tune*` flag (`--tune` has none).
fn tune_cli_flag_values(s: &str) -> usize {
    usize::from(s != "--tune")
}

fn is_export_tunable_flag(s: &str) -> bool {
    matches!(s, "--setpoint" | "--kp" | "--ki" | "--kd" | "--interval")
}

fn build_export_line(full_argv: &[String], args: &LoopArgs) -> String {
    let c = &args.pid_config;
    build_export_line_values(full_argv, c.setpoint, c.kp, c.ki, c.kd, args.interval)
}

fn build_export_line_values(
    full_argv: &[String],
    setpoint: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    interval: Duration,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(bin) = full_argv.first() {
        parts.push(bin.clone());
    } else {
        parts.push("pid-ctl".to_string());
    }
    let mut i = 1usize;
    while i < full_argv.len() {
        let t = full_argv[i].as_str();
        if is_tune_cli_flag(t) {
            i += 1 + tune_cli_flag_values(t);
            continue;
        }
        if is_export_tunable_flag(t) {
            i += 2;
            continue;
        }
        parts.push(full_argv[i].clone());
        i += 1;
    }
    parts.push("--setpoint".into());
    parts.push(format!("{setpoint}"));
    parts.push("--kp".into());
    parts.push(format!("{kp}"));
    parts.push("--ki".into());
    parts.push(format!("{ki}"));
    parts.push("--kd".into());
    parts.push(format!("{kd}"));
    parts.push("--interval".into());
    parts.push(format_interval_arg(interval));
    parts.join(" ")
}

fn build_tuned_gains_only_line(args: &LoopArgs) -> String {
    let c = &args.pid_config;
    format!(
        "Tuned gains only: --kp {} --ki {} --kd {} --setpoint {}",
        c.kp, c.ki, c.kd, c.setpoint
    )
}

fn format_interval_arg(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

fn step_125_up(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 0.1;
    }
    let mag = v.log10().floor();
    let base = 10f64.powf(mag);
    // mantissa is always 1, 2, or 5 for well-formed 1-2-5 steps; cast is safe.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mantissa = (v / base).round() as u32;
    match mantissa {
        1 => base * 2.0,
        2 => base * 5.0,
        _ => base * 10.0,
    }
}

fn step_125_down(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 0.1;
    }
    let mag = v.log10().floor();
    let base = 10f64.powf(mag);
    // mantissa is always 1, 2, or 5 for well-formed 1-2-5 steps; cast is safe.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mantissa = (v / base).round() as u32;
    match mantissa {
        5 => base * 2.0,
        2 => base * 1.0,
        _ => base * 0.5,
    }
}

fn handle_normal_key(
    ui: &mut TuneUiState,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    key: crossterm::event::KeyEvent,
    full_argv: &[String],
    log_file: &mut Option<std::fs::File>,
) {
    match key.code {
        KeyCode::Char('q') => {
            export_line_stderr(full_argv, args);
            flush_shutdown(session, args, log_file);
            ui.quit = true;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            export_line_stderr(full_argv, args);
            flush_shutdown(session, args, log_file);
            ui.quit = true;
        }
        KeyCode::Char('c') => {
            let full = build_export_line(full_argv, args);
            let short = build_tuned_gains_only_line(args);
            ui.export_overlay = Some(format!(
                "Full command (copy and run without --tune):\n\n  {full}\n\nGains only:\n\n  {short}\n\nPress any key to dismiss."
            ));
        }
        KeyCode::Char('s') => {
            if let Some(err) = session.force_flush() {
                eprintln!("save failed: {err}");
            } else {
                json_events::emit_gains_saved(
                    log_file,
                    ui.last_record.as_ref().map_or(0, |r| r.iter),
                );
                ui.status_flash = Some(("Saved".into(), Instant::now()));
            }
        }
        KeyCode::Char('r') => {
            session.reset_integral();
            ui.status_flash = Some(("Integrator reset".into(), Instant::now()));
        }
        KeyCode::Char('h') => {
            ui.hold = !ui.hold;
            let msg = if ui.hold { "Hold on" } else { "Hold off" };
            ui.status_flash = Some((msg.into(), Instant::now()));
        }
        KeyCode::Char('d') => {
            ui.dry_run = !ui.dry_run;
            let msg = if ui.dry_run {
                "Dry-run on"
            } else {
                "Dry-run off"
            };
            ui.status_flash = Some((msg.into(), Instant::now()));
        }
        KeyCode::Char('?') => ui.help_overlay = !ui.help_overlay,
        KeyCode::Char('/') => {
            ui.command_mode = true;
            ui.command_buf.clear();
        }
        KeyCode::Up => ui.focus = ui.focus.prev(),
        KeyCode::Down => ui.focus = ui.focus.next(),
        KeyCode::Left => adjust_focused_gain(ui, session, args, log_file, -1.0),
        KeyCode::Right => adjust_focused_gain(ui, session, args, log_file, 1.0),
        KeyCode::Char('[') => {
            ui.step[ui.focus.idx()] = step_125_down(ui.step[ui.focus.idx()]);
        }
        KeyCode::Char(']') => {
            ui.step[ui.focus.idx()] = step_125_up(ui.step[ui.focus.idx()]);
        }
        _ => {}
    }
}

fn adjust_focused_gain(
    ui: &mut TuneUiState,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    log_file: &mut Option<std::fs::File>,
    sign: f64,
) {
    let step = ui.step[ui.focus.idx()] * sign;
    let c = session.config().clone();
    match ui.focus {
        GainFocus::Kp => session.set_gains((c.kp + step).max(0.0), c.ki, c.kd),
        GainFocus::Ki => session.set_gains(c.kp, (c.ki + step).max(0.0), c.kd),
        GainFocus::Kd => session.set_gains(c.kp, c.ki, c.kd + step),
        GainFocus::Sp => session.set_setpoint(c.setpoint + step),
    }
    args.pid_config = session.config().clone();
    ui.note_gain_change(args, session.config());
    let iter = ui.last_record.as_ref().map_or(0, |r| r.iter);
    let c = session.config();
    json_events::emit_gains_changed(log_file, c.kp, c.ki, c.kd, c.setpoint, iter, "tui");
}

fn handle_command_key(
    ui: &mut TuneUiState,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    key: crossterm::event::KeyEvent,
    full_argv: &[String],
    log_file: &mut Option<std::fs::File>,
) -> Result<(), CliError> {
    match key.code {
        KeyCode::Esc => {
            ui.command_mode = false;
            ui.command_buf.clear();
        }
        KeyCode::Enter => {
            run_command_line(ui, session, args, full_argv, log_file)?;
            ui.command_mode = false;
            ui.command_buf.clear();
        }
        KeyCode::Char(c) => ui.command_buf.push(c),
        KeyCode::Backspace => {
            ui.command_buf.pop();
        }
        _ => {}
    }
    Ok(())
}

// The command interpreter handles many distinct sub-commands; each arm is a few lines but the
// combined match body is inherently long.
#[allow(clippy::too_many_lines)]
fn run_command_line(
    ui: &mut TuneUiState,
    session: &mut ControllerSession,
    args: &mut LoopArgs,
    full_argv: &[String],
    log_file: &mut Option<std::fs::File>,
) -> Result<(), CliError> {
    let line = ui.command_buf.trim();
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("").to_lowercase();
    match cmd.as_str() {
        "" => Ok(()),
        "quit" | "exit" => {
            export_line_stderr(full_argv, args);
            flush_shutdown(session, args, log_file);
            ui.quit = true;
            Ok(())
        }
        "kp" => {
            let v: f64 = parts
                .next()
                .ok_or_else(|| CliError::config("kp requires a value"))?
                .parse()
                .map_err(|_| CliError::config("kp value must be a float"))?;
            let c = session.config().clone();
            session.set_gains(v, c.ki, c.kd);
            args.pid_config = session.config().clone();
            ui.note_gain_change(args, session.config());
            json_events::emit_gains_changed(
                log_file,
                v,
                c.ki,
                c.kd,
                c.setpoint,
                ui.last_record.as_ref().map_or(0, |r| r.iter),
                "tui",
            );
            Ok(())
        }
        "ki" => {
            let v: f64 = parts
                .next()
                .ok_or_else(|| CliError::config("ki requires a value"))?
                .parse()
                .map_err(|_| CliError::config("ki value must be a float"))?;
            let c = session.config().clone();
            session.set_gains(c.kp, v, c.kd);
            args.pid_config = session.config().clone();
            ui.note_gain_change(args, session.config());
            json_events::emit_gains_changed(
                log_file,
                c.kp,
                v,
                c.kd,
                c.setpoint,
                ui.last_record.as_ref().map_or(0, |r| r.iter),
                "tui",
            );
            Ok(())
        }
        "kd" => {
            let v: f64 = parts
                .next()
                .ok_or_else(|| CliError::config("kd requires a value"))?
                .parse()
                .map_err(|_| CliError::config("kd value must be a float"))?;
            let c = session.config().clone();
            session.set_gains(c.kp, c.ki, v);
            args.pid_config = session.config().clone();
            ui.note_gain_change(args, session.config());
            json_events::emit_gains_changed(
                log_file,
                c.kp,
                c.ki,
                v,
                c.setpoint,
                ui.last_record.as_ref().map_or(0, |r| r.iter),
                "tui",
            );
            Ok(())
        }
        "sp" => {
            let v: f64 = parts
                .next()
                .ok_or_else(|| CliError::config("sp requires a value"))?
                .parse()
                .map_err(|_| CliError::config("sp value must be a float"))?;
            session.set_setpoint(v);
            args.pid_config = session.config().clone();
            ui.note_gain_change(args, session.config());
            let c = session.config();
            json_events::emit_gains_changed(
                log_file,
                c.kp,
                c.ki,
                c.kd,
                v,
                ui.last_record.as_ref().map_or(0, |r| r.iter),
                "tui",
            );
            Ok(())
        }
        "interval" => {
            let dur_s = parts
                .next()
                .ok_or_else(|| CliError::config("interval requires a duration"))?;
            let new_interval = crate::parse_duration_flag("--interval", dur_s)?;
            crate::apply_runtime_interval(session, args, new_interval)?;
            Ok(())
        }
        "reset" => {
            session.reset_integral();
            ui.status_flash = Some(("Integrator reset".into(), Instant::now()));
            Ok(())
        }
        "hold" => {
            ui.hold = true;
            ui.status_flash = Some(("Hold on".into(), Instant::now()));
            Ok(())
        }
        "resume" => {
            ui.hold = false;
            ui.status_flash = Some(("Hold off".into(), Instant::now()));
            Ok(())
        }
        "save" => {
            if let Some(err) = session.force_flush() {
                eprintln!("save failed: {err}");
            } else {
                json_events::emit_gains_saved(
                    log_file,
                    ui.last_record.as_ref().map_or(0, |r| r.iter),
                );
                ui.status_flash = Some(("Saved".into(), Instant::now()));
            }
            Ok(())
        }
        "export" => {
            let full = build_export_line(full_argv, args);
            let short = build_tuned_gains_only_line(args);
            ui.export_overlay = Some(format!(
                "Full command (copy and run without --tune):\n\n  {full}\n\nGains only:\n\n  {short}\n\nPress any key to dismiss."
            ));
            Ok(())
        }
        other => {
            let sug = fuzzy_command_names(other);
            let tail = if sug.is_empty() {
                "try kp, ki, kd, sp, interval, reset, hold, resume, save, export, quit".to_string()
            } else {
                format!("did you mean: {}?", sug.join(", "))
            };
            Err(CliError::config(format!(
                "unknown command `{other}` — {tail}"
            )))
        }
    }
}

fn draw(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    session: &ControllerSession,
    args: &LoopArgs,
    ui: &TuneUiState,
    interval_secs: f64,
    until_next: Duration,
) -> Result<(), CliError> {
    terminal
        .draw(|f| {
            render_frame(f, session, args, ui, interval_secs, until_next);
        })
        .map_err(|e| CliError::new(1, format!("draw: {e}")))?;
    Ok(())
}

fn expand_scale(lo: f64, hi: f64, center: f64) -> (f64, f64) {
    let min_span = (0.01 * center.abs()).max(1e-9);
    let actual_span = hi - lo;
    if actual_span >= min_span {
        return (lo, hi);
    }
    let half = (min_span * 0.5).max(hi - center).max(center - lo);
    (center - half, center + half)
}

fn history_range(history: &VecDeque<f64>) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for &v in history {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / count as f64;
    Some(expand_scale(lo, hi, mean))
}

fn spark_data(values: &VecDeque<f64>) -> Vec<u64> {
    if values.is_empty() {
        return vec![];
    }
    let mut min_v = f64::INFINITY;
    let mut max_v = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for &v in values {
        if v.is_finite() {
            min_v = min_v.min(v);
            max_v = max_v.max(v);
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return vec![0; values.len()];
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / count as f64;
    let (lo, hi) = expand_scale(min_v, max_v, mean);
    let span = hi - lo;
    if span <= 1e-9 {
        // Constant series: `(v - min) / span` would be all zeros — ratatui draws no visible bars.
        // Use a flat mid-line so history is visible (e.g. dry-run + sim: PV stuck until CV reaches plant).
        return vec![50; values.len()];
    }
    values
        .iter()
        .map(|v| {
            if !v.is_finite() {
                return 0u64;
            }
            // Value is clamped to [0.0, 100.0] before rounding, so truncation and sign loss are safe.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                (((v - lo) / span) * 100.0).clamp(0.0, 100.0).round() as u64
            }
        })
        .collect()
}

fn spark_tail_slice<T: Clone>(values: &[T], max_w: usize) -> Vec<T> {
    let n = values.len();
    if n == 0 {
        return vec![];
    }
    let take = n.min(max_w);
    values[n - take..].to_vec()
}

fn spark_marker_row(
    serial_window: &[u64],
    annotations: &VecDeque<GainAnnotation>,
    width: usize,
) -> String {
    let w = width.max(1);
    // Start with time-tick dots at multiples of 10.
    let mut chars: Vec<char> = serial_window
        .iter()
        .take(w)
        .map(|&s| if s % 10 == 0 { '·' } else { ' ' })
        .collect();
    while chars.len() < w {
        chars.push(' ');
    }
    // Gain-change pipes overwrite dots.
    for ann in annotations {
        if let Some(col) = serial_window.iter().position(|s| *s == ann.marker_tick)
            && col < w
        {
            chars[col] = '|';
        }
    }
    chars.into_iter().collect()
}

fn annotation_caret_line(annotations: &VecDeque<GainAnnotation>, max_width: usize) -> String {
    let parts: Vec<String> = annotations
        .iter()
        .map(|a| format!("^ {}", a.display_text()))
        .collect();
    let mut s = parts.join("  ");
    let n = s.chars().count();
    if n > max_width {
        s = format!(
            "{}…",
            s.chars()
                .take(max_width.saturating_sub(1))
                .collect::<String>()
        );
    }
    s
}

fn cv_fill_fraction(cv: f64, lo: f64, hi: f64) -> Option<f64> {
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let span = hi - lo;
    if span.abs() < f64::EPSILON {
        return None;
    }
    Some(((cv - lo) / span).clamp(0.0, 1.0))
}

fn cv_percent(cv: f64, lo: f64, hi: f64) -> Option<f64> {
    cv_fill_fraction(cv, lo, hi).map(|f| f * 100.0)
}

fn cv_bar_block(frac: f64, width: usize) -> String {
    let width = width.max(1);
    // frac is in [0.0, 1.0]; width is a small terminal column count.
    // Casting width (usize) to f64 is lossless for any realistic terminal width.
    // Casting the rounded product back to usize: result is clamped by .min(width).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let filled = ((frac * width as f64).round() as usize).min(width);
    let mut s = String::with_capacity(width + 2);
    s.push('[');
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s.push(']');
    s
}

/// Edit distance for short command tokens (fuzzy match).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let old = row[j];
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (prev + cost).min(row[j] + 1).min(row[j - 1] + 1);
            prev = old;
        }
    }
    row[b.len()]
}

const COMMAND_MODE_WORDS: &[&str] = &[
    "kp", "ki", "kd", "sp", "interval", "reset", "hold", "resume", "save", "export", "quit", "exit",
];

fn fuzzy_command_names(token: &str) -> Vec<&'static str> {
    if token.is_empty() {
        return vec![];
    }
    let prefixes: Vec<&'static str> = COMMAND_MODE_WORDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(token))
        .collect();
    if !prefixes.is_empty() {
        return prefixes;
    }
    if token.len() > 8 {
        return vec![];
    }
    let mut scored: Vec<(&'static str, usize)> = COMMAND_MODE_WORDS
        .iter()
        .copied()
        .map(|c| (c, levenshtein(token, c)))
        .filter(|(_, d)| *d <= 2)
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    scored.into_iter().map(|(c, _)| c).take(5).collect()
}

fn needed_decimals(v: f64) -> usize {
    if !v.is_finite() {
        return 2;
    }
    let v = v.abs();
    for d in 0i32..=6 {
        let factor = 10f64.powi(d);
        if (v * factor - (v * factor).round()).abs() < 1e-9 * factor.max(1.0) {
            #[allow(clippy::cast_sign_loss)]
            return d as usize;
        }
    }
    6
}

fn gains_precision(cfg: &PidConfig, step: &[f64; 4]) -> usize {
    [
        cfg.kp,
        cfg.ki,
        cfg.kd,
        cfg.setpoint,
        step[0],
        step[1],
        step[2],
        step[3],
    ]
    .iter()
    .map(|&v| needed_decimals(v))
    .max()
    .unwrap_or(2)
    .max(1)
}

fn step_cell_for_row(focus: GainFocus, row: usize, step: &[f64; 4], prec: usize) -> String {
    if focus.idx() == row {
        format!("[step {:.*}]", prec, step[row])
    } else {
        " ".repeat(prec + 8)
    }
}

fn history_trend(history: &VecDeque<f64>) -> &'static str {
    let first = history.iter().find(|v| v.is_finite()).copied();
    let last = history.iter().rev().find(|v| v.is_finite()).copied();
    match (first, last) {
        (Some(f), Some(l)) if l > f + 1e-9 => "▲",
        (Some(f), Some(l)) if l < f - 1e-9 => "▼",
        _ => "→",
    }
}

fn command_mode_hint(
    line: &str,
    cfg: &PidConfig,
    interval: Duration,
    units: Option<&str>,
) -> String {
    let line = line.trim_start();
    let u = units.unwrap_or("");
    let token = line.split_whitespace().next().unwrap_or("");
    match token {
        "" => "Commands: kp, ki, kd, sp, interval, reset, hold, resume, save, export, quit — type a name to see a hint."
            .to_string(),
        "kp" => format!(
            "Proportional gain — controls immediate reaction to error. Too low: slow/drifting. Too high: oscillates. Current: Kp={:.6} {}",
            cfg.kp, u
        ),
        "ki" => format!(
            "Integral gain — corrects persistent offset over time. Too high: windup/saturation. Tip: press r to clear accumulator after change. Current: Ki={:.6} {}",
            cfg.ki, u
        ),
        "kd" => format!(
            "Derivative gain — brakes output as PV approaches setpoint; D-on-measurement (no kick on setpoint-only moves). Too high: amplifies noise. Current: Kd={:.6} {}",
            cfg.kd, u
        ),
        "sp" => format!(
            "Setpoint — the target value. Large jumps are handled smoothly; use --setpoint-ramp to limit rate of change. Current: {:.6} {}",
            cfg.setpoint, u
        ),
        "reset" => "Clears the I accumulator (i_acc only). Triggers D-term protection on next tick. Use after gain changes or if output is stuck at its limit."
            .to_string(),
        "interval" => format!(
            "How often the controller runs. Faster = more responsive but more actuation. Current: {}",
            format_interval_arg(interval)
        ),
        "export" => "Prints a full copy-pasteable command for this subcommand without --tune — use after tuning for cron, systemd, or scripts."
            .to_string(),
        "hold" => "Hold last applied CV while continuing to service status/socket.".to_string(),
        "resume" => "Resume normal PID output after hold.".to_string(),
        "save" => "Write current gains to the state file.".to_string(),
        "quit" | "exit" => "Exit cleanly (prints copy-pasteable CLI to stderr).".to_string(),
        other => {
            let prefixes = fuzzy_command_names(other);
            if prefixes.is_empty() {
                format!("Unknown `{other}` — try kp, ki, kd, sp, interval, reset, hold, resume, save, export, quit")
            } else {
                format!("Did you mean: {}?", prefixes.join(", "))
            }
        }
    }
}

const HELP_OVERLAY_TEXT: &str = "\
PID tuning — quick reference\n\
\n\
Error convention: error = Setpoint − PV. Positive error → PV is below target (push output up). Negative → PV is above target.\n\
\n\
P (proportional): reacts to current error — immediate push toward the target.\n\
I (integral): corrects persistent offset over time — fixes drift; can wind up if too high.\n\
D (derivative): uses rate of change of PV (D-on-measurement), not error — no derivative kick when only the setpoint moves. Reduces overshoot; amplifies sensor noise if too high.\n\
\n\
Tuning tips: start with Ki=0 and Kd=0; raise Kp until responsive without heavy oscillation; add Ki to remove steady-state error; add Kd to damp overshoot. Combine D with --pv-filter when noisy.\n\
\n\
Anti-windup: integral back-calculates when the actuator saturates so the I accumulator self-corrects.\n\
\n\
Keys: ↑↓ focus gains  ←→ adjust  [ ] step  / command  r integrator reset  s save  c export  h hold  d dry-run  ? help  q quit\n\
Dry-run / simulation: with `d` on (or `--dry-run`), CV is not sent to `--cv-cmd` — a simulated plant will not move; sparklines stay flat until dry-run is off.\n\
Command mode: kp/ki/kd/sp <value>, interval <dur>, reset, hold, resume, save, export, quit\n\
Export / copy-paste: c or export prints a full non-interactive CLI (and a short “Tuned gains only” line) to stderr; also printed on clean quit.\n\
";

// Drawing a complex multi-panel TUI layout necessarily touches many widgets in sequence;
// extracting sub-panels would require threading the frame reference through many helpers.
#[allow(clippy::too_many_lines)]
fn render_frame(
    f: &mut Frame<'_>,
    session: &ControllerSession,
    args: &LoopArgs,
    ui: &TuneUiState,
    interval_secs: f64,
    until_next: Duration,
) {
    // Export overlay — full-screen, highest priority (any key dismisses).
    if let Some(export_text) = &ui.export_overlay {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Export (any key to dismiss)");
        let p = Paragraph::new(export_text.as_str())
            .wrap(Wrap { trim: false })
            .block(block);
        f.render_widget(p, f.area());
        return;
    }

    // Help overlay — full-screen, replaces dashboard.
    if ui.help_overlay {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help (Esc or ? to close)");
        let p = Paragraph::new(HELP_OVERLAY_TEXT)
            .wrap(Wrap { trim: true })
            .block(block);
        f.render_widget(p, f.area());
        return;
    }

    let cfg = session.config();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    let name = args
        .name
        .clone()
        .or_else(|| {
            args.state_path
                .as_ref()
                .and_then(|p| p.file_stem()?.to_str().map(String::from))
        })
        .unwrap_or_else(|| "pid-ctl".to_string());
    let elapsed = ui.start.elapsed();
    let elapsed_s = elapsed.as_secs();
    // tune_history is a small integer (default 60); as f64 is exact for all practical values.
    #[allow(clippy::cast_precision_loss)]
    let hist_wall_s = interval_secs * args.tune_history as f64;
    let header = Line::from(vec![Span::styled(
        format!(
            "pid-ctl  controller={name}  interval={:.1}s  last {} ticks ~{:.0}s wall  iter {}  {:02}m{:02}s  next ~{:.1}s",
            interval_secs,
            args.tune_history,
            hist_wall_s,
            ui.last_record.as_ref().map_or(0, |r| r.iter),
            elapsed_s / 60,
            elapsed_s % 60,
            until_next.as_secs_f64()
        ),
        Style::default().fg(Color::Cyan),
    )]);
    f.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(chunks[1]);

    let spark_w = body_chunks[1].width.saturating_sub(4) as usize;
    let pv_spark_full = spark_data(&ui.pv_history);
    let cv_spark_full = spark_data(&ui.cv_history);
    let pv_spark = spark_tail_slice(&pv_spark_full, spark_w);
    let cv_spark = spark_tail_slice(&cv_spark_full, spark_w);

    let serial_vec: Vec<u64> = ui.serial_history.iter().copied().collect();
    let serial_window = spark_tail_slice(&serial_vec, spark_w);

    let marker_row = spark_marker_row(&serial_window, &ui.annotations, spark_w);
    let ann_w = chunks[1].width.saturating_sub(2) as usize;
    let caret_line = annotation_caret_line(&ui.annotations, ann_w);

    let units = args.units.as_deref().unwrap_or("");
    let pv_val = ui.last_record.as_ref().map_or(0.0, |r| r.pv);
    let err_val = ui.last_record.as_ref().map_or(0.0, |r| r.err);
    let err_hint = if err_val > 0.0 {
        "▼ positive — PV below target, output increasing"
    } else if err_val < 0.0 {
        "▼ negative — PV above target, output reducing"
    } else {
        "at setpoint"
    };

    let cv_val = ui.last_record.as_ref().map_or(0.0, |r| r.cv);
    let bar_w = 15usize;
    let bar_str = cv_fill_fraction(cv_val, cfg.out_min, cfg.out_max)
        .map_or_else(|| "[ n/a ]".to_string(), |f| cv_bar_block(f, bar_w));
    let pct_str = cv_percent(cv_val, cfg.out_min, cfg.out_max)
        .map_or_else(|| "—".to_string(), |p| format!("{p:.0}%"));

    let i_acc_hint = "anti-windup active — accumulator self-corrects when saturated; press r to reset manually if output is stuck";

    let gprec = gains_precision(cfg, &ui.step);
    #[allow(clippy::uninlined_format_args)]
    let fmtg = |v: f64| format!("{:>8.*}", gprec, v);
    let gains_lines = format!(
        "{}Kp  {}  {}  proportional — immediate reaction\n\
         {}Ki  {}  {}  integral — drift correction\n\
         {}Kd  {}  {}  derivative — damping / braking\n\
         {}SP  {}  {}  setpoint target",
        if ui.focus == GainFocus::Kp {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.kp),
        step_cell_for_row(ui.focus, 0, &ui.step, gprec),
        if ui.focus == GainFocus::Ki {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.ki),
        step_cell_for_row(ui.focus, 1, &ui.step, gprec),
        if ui.focus == GainFocus::Kd {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.kd),
        step_cell_for_row(ui.focus, 2, &ui.step, gprec),
        if ui.focus == GainFocus::Sp {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.setpoint),
        step_cell_for_row(ui.focus, 3, &ui.step, gprec),
    );

    let process_block = format!(
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         PROCESS                                              HINT\n\
         ──────────────────────────────────────────────────────────────────────\n\
         Setpoint      {sp:>8.3} {u}    target (ramps when --setpoint-ramp is active)\n\
         PV (actual)   {pv:>8.3} {u}    sensor reading\n\
         Error         {err:>8.3} {u}    Setpoint−PV  ({err_hint})\n\
         \n\
         OUTPUT\n\
         ──────────────────────────────────────────────────────────────────────\n\
         CV            {cv:>8.3}  {bar} {pct}    last applied / commanded output\n\
         Range         {lo:.3} – {hi:.3}    hold={hold}  dry_run={dry}\n\
         \n\
         PID BREAKDOWN\n\
         ──────────────────────────────────────────────────────────────────────\n\
         P  (proportional)   {p:>+9.4}    responds to current error\n\
         I  (integral)       {i:>+9.4}    persistent offset correction\n\
         D  (derivative)     {d:>+9.4}    PV rate (D-on-measurement)\n\
         I accumulator       {iac:>+9.4}    {iacc_hint}\n\
         \n\
         GAINS — ↑↓ select   ←→ adjust   [ ] step size              s save   q quit\n\
         ──────────────────────────────────────────────────────────────────────\n\
         {gains_lines}",
        sp = cfg.setpoint,
        u = units,
        pv = pv_val,
        err = err_val,
        err_hint = err_hint,
        cv = cv_val,
        bar = bar_str,
        pct = pct_str,
        lo = cfg.out_min,
        hi = cfg.out_max,
        hold = ui.hold,
        dry = ui.dry_run,
        p = ui.last_record.as_ref().map_or(0.0, |r| r.p),
        i = ui.last_record.as_ref().map_or(0.0, |r| r.i),
        d = ui.last_record.as_ref().map_or(0.0, |r| r.d),
        iac = ui.last_record.as_ref().map_or(0.0, |r| r.i_acc),
        iacc_hint = i_acc_hint,
        gains_lines = gains_lines,
    );

    let hist_title = format!(
        "HISTORY (last {} ticks / ~{:.0}s wall at {:.1}s interval)",
        args.tune_history, hist_wall_s, interval_secs
    );

    f.render_widget(
        Paragraph::new(process_block).wrap(Wrap { trim: true }),
        body_chunks[0],
    );

    // PV and CV sparklines each need Length(2): 1 row for the block title ("PV"/"CV") and
    // 1 row for the sparkline bars. With Length(1), Block::inner() subtracts 1 for the title,
    // leaving height=0 so ratatui renders nothing (the sparklines go missing).
    let hist_inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(body_chunks[1]);

    f.render_widget(Paragraph::new(hist_title), hist_inner[0]);

    let pv_scale = history_range(&ui.pv_history);
    let pv_trend = history_trend(&ui.pv_history);
    let pv_range_str = pv_scale
        .map(|(lo, hi)| format!("  [{lo:.2} – {hi:.2}]"))
        .unwrap_or_default();
    let pv_title = format!("PV {pv_trend} {pv_val:.2}{pv_range_str}");
    let spark_pv = Sparkline::default()
        .data(&pv_spark)
        .style(Style::default().fg(Color::LightBlue));
    f.render_widget(
        spark_pv.block(Block::default().title(pv_title)),
        hist_inner[1],
    );

    // Setpoint indicator — white dash just past the last bar on the PV sparkline.
    if let Some((pv_lo, pv_hi)) = pv_scale {
        let pv_span = pv_hi - pv_lo;
        if pv_span > 1e-12 {
            let sp_pct = ((cfg.setpoint - pv_lo) / pv_span * 100.0).clamp(0.0, 100.0);
            let num_rows = f64::from(hist_inner[1].height);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let row_offset = ((1.0 - sp_pct / 100.0) * num_rows)
                .floor()
                .clamp(0.0, num_rows - 1.0) as u16;
            let sp_y = hist_inner[1].y + row_offset;
            #[allow(clippy::cast_possible_truncation)]
            let ind_x = hist_inner[1].x + pv_spark.len() as u16;
            if ind_x + 2 <= hist_inner[1].right() {
                f.render_widget(
                    Paragraph::new("──").style(Style::default().fg(Color::White)),
                    Rect {
                        x: ind_x,
                        y: sp_y,
                        width: 2,
                        height: 1,
                    },
                );
            }
        }
    }

    f.render_widget(
        Paragraph::new(marker_row.clone()).style(Style::default().fg(Color::White)),
        hist_inner[2],
    );

    let cv_trend = history_trend(&ui.cv_history);
    let cv_range_str = history_range(&ui.cv_history)
        .map(|(lo, hi)| format!("  [{lo:.2} – {hi:.2}]"))
        .unwrap_or_default();
    let cv_title = format!("CV {cv_trend} {cv_val:.2}{cv_range_str}");
    let cv_sparkline = Sparkline::default()
        .data(&cv_spark)
        .style(Style::default().fg(Color::LightGreen));
    f.render_widget(
        cv_sparkline.block(Block::default().title(cv_title)),
        hist_inner[3],
    );
    f.render_widget(
        Paragraph::new(marker_row).style(Style::default().fg(Color::White)),
        hist_inner[4],
    );

    let caret_para = if caret_line.is_empty() {
        String::new()
    } else {
        caret_line
    };
    f.render_widget(Paragraph::new(caret_para), hist_inner[5]);

    let keymap = "↑↓ select  ←→ adjust  [] step  / cmd  r reset  s save  c export  h hold  d dry-run  ? help  q quit";
    let flash_msg = ui.status_flash.as_ref().and_then(|(msg, t)| {
        if t.elapsed() < Duration::from_secs(3) {
            Some(msg.as_str())
        } else {
            None
        }
    });
    let footer_text = if let Some(msg) = flash_msg {
        format!("{msg}  |  {keymap}")
    } else {
        keymap.to_string()
    };
    let footer_style = if flash_msg.is_some() {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    let footer = Paragraph::new(footer_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::TOP));
    f.render_widget(footer, chunks[2]);

    if ui.command_mode {
        use ratatui::style::Modifier;
        use ratatui::widgets::Clear;
        let hint = command_mode_hint(
            &ui.command_buf,
            session.config(),
            args.interval,
            args.units.as_deref(),
        );
        let area = centered_rect(88, 18, f.area());
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Command (Esc)");
        f.render_widget(Clear, area);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [hint_area, input_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .areas(inner);
        if !hint.is_empty() {
            f.render_widget(Paragraph::new(hint).wrap(Wrap { trim: true }), hint_area);
        }
        let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
        let input_line = Line::from(vec![
            Span::raw(format!("> {}", ui.command_buf)),
            Span::styled(" ", cursor_style),
        ]);
        f.render_widget(Paragraph::new(input_line), input_area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::{
        GainAnnotation, build_export_line_values, history_trend, spark_data, spark_marker_row,
    };
    use std::collections::VecDeque;
    use std::time::Duration;

    #[test]
    fn spark_data_flat_series_is_visible_mid_line() {
        let mut d = VecDeque::new();
        d.push_back(22.0);
        d.push_back(22.0);
        d.push_back(22.0);
        let s = spark_data(&d);
        assert_eq!(s, vec![50, 50, 50]);
    }

    #[test]
    fn spark_data_spanning_series_normalized() {
        let mut d = VecDeque::new();
        d.push_back(0.0);
        d.push_back(10.0);
        let s = spark_data(&d);
        assert_eq!(s, vec![0, 100]);
    }

    #[test]
    fn pv_trend_arrow_when_history_rises() {
        let mut d = VecDeque::new();
        d.push_back(1.0);
        d.push_back(3.0);
        assert_eq!(history_trend(&d), "▲");
    }

    #[test]
    fn spark_marker_row_places_pipe_at_tick_column() {
        let serials = vec![1_u64, 2, 3, 4, 5];
        let mut ann = VecDeque::new();
        ann.push_back(GainAnnotation {
            marker_tick: 4,
            kp: Some((1.0, 2.0)),
            ki: None,
            kd: None,
            sp: None,
        });
        let row = spark_marker_row(&serials, &ann, 5);
        assert_eq!(row.chars().nth(3), Some('|'));
    }

    /// Regression test for sparklines going missing after layout refactor.
    ///
    /// `Block::default().title(...)` eats 1 row for the title.  With a sparkline given
    /// `Constraint::Length(1)` the inner height after `Block::inner()` becomes 0 and ratatui
    /// early-exits without rendering any bars.  Each sparkline row must be at least Length(2).
    #[test]
    fn sparkline_constraints_are_at_least_2_rows() {
        use ratatui::layout::Constraint;
        // Minimum height required so Block::inner (which subtracts 1 for the title) still
        // leaves ≥ 1 row for the sparkline bars.
        const MIN_SPARK_ROWS: u16 = 2;
        // Mirror the constraints used in render_frame's hist_inner layout.
        // Indices 1 and 3 are the PV and CV sparklines respectively.
        let constraints = [
            Constraint::Length(2), // [0] history title
            Constraint::Length(2), // [1] PV sparkline — must be ≥ MIN_SPARK_ROWS
            Constraint::Length(1), // [2] PV marker row
            Constraint::Length(2), // [3] CV sparkline — must be ≥ MIN_SPARK_ROWS
            Constraint::Length(1), // [4] CV marker row
            Constraint::Length(2), // [5] caret
        ];
        for idx in [1usize, 3] {
            if let Constraint::Length(h) = constraints[idx] {
                assert!(
                    h >= MIN_SPARK_ROWS,
                    "hist_inner[{idx}] (sparkline) has Length({h}) but needs ≥ {MIN_SPARK_ROWS}"
                );
            } else {
                panic!("hist_inner[{idx}] is not a Length constraint");
            }
        }
    }

    /// Regression: history cap must be max(tune_history, spark_w) so that sparklines fill
    /// the terminal width even when tune_history < screen columns.
    #[test]
    fn history_cap_uses_spark_w_when_wider_than_tune_history() {
        // Simulate a TuneUiState with tune_history=10 but spark_w=100 (wide terminal).
        // After 20 pushes, we expect 100 items retained (not 10).
        let tune_history = 10usize;
        let spark_w = 100usize;
        // The cap logic: cap = tune_history.max(spark_w)
        let cap = tune_history.max(spark_w);
        assert_eq!(
            cap, 100,
            "cap should follow spark_w when spark_w > tune_history"
        );
        // Simulate the dequeue trimming
        let mut history: VecDeque<f64> = VecDeque::new();
        for i in 0..200usize {
            while history.len() >= cap {
                history.pop_front();
            }
            history.push_back(i as f64);
        }
        assert_eq!(history.len(), cap, "history should hold exactly cap items");
        // Conversely: if spark_w < tune_history, tune_history wins
        let cap2 = tune_history.max(20);
        assert_eq!(
            cap2, 20,
            "cap should follow tune_history when tune_history > spark_w"
        );
    }

    #[test]
    fn history_trend_falling() {
        let mut d = VecDeque::new();
        d.push_back(5.0);
        d.push_back(1.0);
        assert_eq!(super::history_trend(&d), "▼");
    }

    #[test]
    fn history_trend_stable() {
        let mut d = VecDeque::new();
        d.push_back(3.0);
        d.push_back(3.0);
        assert_eq!(super::history_trend(&d), "→");
    }

    #[test]
    fn history_trend_empty_is_stable() {
        let d: VecDeque<f64> = VecDeque::new();
        assert_eq!(super::history_trend(&d), "→");
    }

    #[test]
    fn gain_annotation_display_text_shows_net_change() {
        let ann = GainAnnotation {
            marker_tick: 5,
            kp: Some((1.0, 2.0)),
            ki: Some((0.1, 0.3)),
            kd: None,
            sp: None,
        };
        let text = ann.display_text();
        assert!(text.contains("Kp 1.000→2.000"), "got: {text}");
        assert!(text.contains("Ki 0.100→0.300"), "got: {text}");
        assert!(!text.contains("Kd"), "got: {text}");
    }

    #[test]
    fn spark_marker_row_time_dots_at_multiples_of_10() {
        let serials: Vec<u64> = vec![8, 9, 10, 11, 20];
        let ann: VecDeque<GainAnnotation> = VecDeque::new();
        let row = spark_marker_row(&serials, &ann, 5);
        let chars: Vec<char> = row.chars().collect();
        assert_eq!(chars[0], ' ', "8 % 10 != 0");
        assert_eq!(chars[1], ' ', "9 % 10 != 0");
        assert_eq!(chars[2], '·', "10 % 10 == 0");
        assert_eq!(chars[3], ' ', "11 % 10 != 0");
        assert_eq!(chars[4], '·', "20 % 10 == 0");
    }

    #[test]
    fn spark_marker_row_pipe_overwrites_dot() {
        let serials: Vec<u64> = vec![10, 20, 30];
        let mut ann: VecDeque<GainAnnotation> = VecDeque::new();
        ann.push_back(GainAnnotation {
            marker_tick: 10,
            kp: Some((1.0, 2.0)),
            ki: None,
            kd: None,
            sp: None,
        });
        let row = spark_marker_row(&serials, &ann, 3);
        let chars: Vec<char> = row.chars().collect();
        assert_eq!(chars[0], '|', "pipe should overwrite dot at tick 10");
        assert_eq!(chars[1], '·', "tick 20 should show dot");
        assert_eq!(chars[2], '·', "tick 30 should show dot");
    }

    #[test]
    fn step_125_up_sequence() {
        use super::step_125_up;
        assert!((step_125_up(0.1) - 0.2).abs() < 1e-9);
        assert!((step_125_up(0.2) - 0.5).abs() < 1e-9);
        assert!((step_125_up(0.5) - 1.0).abs() < 1e-9);
        assert!((step_125_up(1.0) - 2.0).abs() < 1e-9);
        assert!((step_125_up(2.0) - 5.0).abs() < 1e-9);
        assert!((step_125_up(5.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn step_125_down_sequence() {
        use super::step_125_down;
        assert!((step_125_down(10.0) - 5.0).abs() < 1e-9);
        assert!((step_125_down(5.0) - 2.0).abs() < 1e-9);
        assert!((step_125_down(2.0) - 1.0).abs() < 1e-9);
        assert!((step_125_down(1.0) - 0.5).abs() < 1e-9);
        assert!((step_125_down(0.5) - 0.2).abs() < 1e-9);
        assert!((step_125_down(0.2) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn needed_decimals_integers_need_zero() {
        use super::needed_decimals;
        assert_eq!(needed_decimals(1.0), 0);
        assert_eq!(needed_decimals(10.0), 0);
        assert_eq!(needed_decimals(0.0), 0);
    }

    #[test]
    fn needed_decimals_step_values() {
        use super::needed_decimals;
        assert_eq!(needed_decimals(0.1), 1);
        assert_eq!(needed_decimals(0.01), 2);
        assert_eq!(needed_decimals(0.001), 3);
        assert_eq!(needed_decimals(0.5), 1);
        assert_eq!(needed_decimals(0.25), 2);
    }

    #[test]
    fn expand_scale_widens_tiny_range() {
        use super::expand_scale;
        let (lo, hi) = expand_scale(2.770, 2.780, 2.775);
        assert!(
            hi - lo >= 0.01 * 2.775,
            "span should be at least 1% of mean"
        );
        assert!(lo <= 2.770, "lo should not exceed original min");
        assert!(hi >= 2.780, "hi should not exceed original max");
    }

    #[test]
    fn expand_scale_leaves_wide_range_unchanged() {
        use super::expand_scale;
        let (lo, hi) = expand_scale(0.0, 100.0, 50.0);
        assert!((lo - 0.0).abs() < 1e-9);
        assert!((hi - 100.0).abs() < 1e-9);
    }

    #[test]
    fn history_range_returns_expanded_scale() {
        use super::history_range;
        let mut d = VecDeque::new();
        // Tight range around 100 — should be expanded to at least 1%
        for _ in 0..10 {
            d.push_back(100.0);
        }
        d.push_back(100.001);
        let (lo, hi) = history_range(&d).unwrap();
        assert!(hi - lo >= 0.01 * 100.0 * 0.99, "span should be ~1% of mean");
    }

    #[test]
    fn export_dedupes_tunables_and_strips_tune_flags() {
        let argv = vec![
            "pid-ctl".into(),
            "loop".into(),
            "--pv-file".into(),
            "/tmp/p".into(),
            "--setpoint".into(),
            "50".into(),
            "--kp".into(),
            "1".into(),
            "--ki".into(),
            "0.1".into(),
            "--kd".into(),
            "0".into(),
            "--interval".into(),
            "2s".into(),
            "--tune-history".into(),
            "80".into(),
            "--tune".into(),
        ];
        let s = build_export_line_values(&argv, 78.3, 2.1, 0.05, 0.8, Duration::from_secs(5));
        assert!(
            s.contains("--setpoint 78.3")
                && s.contains("--kp 2.1")
                && s.contains("--ki 0.05")
                && s.contains("--kd 0.8")
                && s.contains("--interval 5s")
        );
        assert_eq!(s.matches("--kp").count(), 1);
        assert_eq!(s.matches("--setpoint").count(), 1);
        assert!(!s.contains("--tune"));
        assert!(!s.contains("--tune-history"));
    }
}
