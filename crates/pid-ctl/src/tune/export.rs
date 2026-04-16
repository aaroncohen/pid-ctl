use crate::LoopArgs;
use pid_ctl::app::ControllerSession;
use pid_ctl::json_events;
use std::time::Duration;

pub(in crate::tune) fn flush_shutdown(
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

pub(in crate::tune) fn export_line_stderr(full_argv: &[String], args: &LoopArgs) {
    eprintln!("{}", build_export_line(full_argv, args));
    eprintln!("{}", build_tuned_gains_only_line(args));
}

pub(in crate::tune) fn is_tune_cli_flag(s: &str) -> bool {
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
pub(in crate::tune) fn tune_cli_flag_values(s: &str) -> usize {
    usize::from(s != "--tune")
}

pub(in crate::tune) fn is_export_tunable_flag(s: &str) -> bool {
    matches!(s, "--setpoint" | "--kp" | "--ki" | "--kd" | "--interval")
}

pub(in crate::tune) fn build_export_line(full_argv: &[String], args: &LoopArgs) -> String {
    let c = &args.pid_config;
    build_export_line_values(full_argv, c.setpoint, c.kp, c.ki, c.kd, args.interval)
}

pub(in crate::tune) fn build_export_line_values(
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

pub(in crate::tune) fn build_tuned_gains_only_line(args: &LoopArgs) -> String {
    let c = &args.pid_config;
    format!(
        "Tuned gains only: --kp {} --ki {} --kd {} --setpoint {}",
        c.kp, c.ki, c.kd, c.setpoint
    )
}

pub(in crate::tune) fn format_interval_arg(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}
