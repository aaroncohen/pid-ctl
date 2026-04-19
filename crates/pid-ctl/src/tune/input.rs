use super::export::{
    build_export_line, build_tuned_gains_only_line, export_line_stderr, flush_shutdown,
    format_interval_arg,
};
use super::model::{GainFocus, TuneUiState};
use crate::CliError;
use crate::LoopArgs;
use crossterm::event::{KeyCode, KeyModifiers};
use pid_ctl::app::ControllerSession;
use pid_ctl::json_events;
use pid_ctl_core::PidConfig;
use std::time::{Duration, Instant};

pub(in crate::tune) fn step_125_up(v: f64) -> f64 {
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

pub(in crate::tune) fn step_125_down(v: f64) -> f64 {
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

pub(in crate::tune) fn handle_normal_key(
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

pub(in crate::tune) fn adjust_focused_gain(
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

pub(in crate::tune) fn handle_command_key(
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
pub(in crate::tune) fn run_command_line(
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
            pid_ctl::app::loop_runtime::apply_runtime_interval(session, args, new_interval);
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

pub(in crate::tune) fn fuzzy_command_names(token: &str) -> Vec<&'static str> {
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

pub(in crate::tune) fn needed_decimals(v: f64) -> usize {
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

pub(in crate::tune) fn command_mode_hint(
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

pub(in crate::tune) const HELP_OVERLAY_TEXT: &str = "\
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
Export / copy-paste: c or export prints a full non-interactive CLI (and a short \u{201c}Tuned gains only\u{201d} line) to stderr; also printed on clean quit.\n\
";
