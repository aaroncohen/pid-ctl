use super::raw::{LoopRawArgs, OnceRawArgs, PipeRawArgs, StatusRawArgs};
use super::types::{
    CvSinkConfig, LoopArgs, OnceArgs, OutputFormat, PidFlags, PipeArgs, PvSourceConfig, SetArgs,
    StatusFlags,
};
use super::user_set::UserSet;
use crate::CliError;
use pid_ctl::adapters::{CmdPvSource, FilePvSource, PvSource};
use pid_ctl::app::StateStore;
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Public duration parser (also used by socket request handler in main.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Conversion: raw clap structs → resolved args
// ---------------------------------------------------------------------------

pub(crate) fn parse_once(raw: &OnceRawArgs) -> Result<OnceArgs, CliError> {
    // Check once-specific rejections (--tune etc.)
    raw.rejected.check()?;

    let pv_source = raw.pv.to_pv_source_config()?.ok_or_else(|| {
        CliError::config(
            "once requires a PV source: --pv <float>, --pv-file <path>, or --pv-cmd <cmd>",
        )
    })?;

    let mut cv_sink = raw.cv.to_cv_sink_config()?;

    // CV sink is required unless --dry-run is active.
    if cv_sink.is_none() && !raw.dry_run {
        return Err(CliError::config(
            "once requires exactly one CV sink: --cv-stdout or --cv-file <path>",
        ));
    }

    let output_format: OutputFormat = raw.common.format.clone().into();

    if matches!(output_format, OutputFormat::Json) && matches!(cv_sink, Some(CvSinkConfig::Stdout))
    {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with --cv-stdout — use --log for machine-readable telemetry",
        ));
    }

    let pid_flags = raw.pid.to_pid_flags()?;
    let pid_config = resolve_pid_config(&pid_flags, raw.common.state.as_deref())?;

    let cv_cmd_timeout = raw.common.cv_cmd_timeout.map(Duration::from_secs_f64);
    let effective_cmd_timeout = raw
        .common
        .cmd_timeout
        .map_or(Duration::from_secs(5), Duration::from_secs_f64);
    let pv_cmd_timeout = raw
        .common
        .pv_cmd_timeout
        .map_or(effective_cmd_timeout, Duration::from_secs_f64);

    if let Some(ref mut sink) = cv_sink {
        apply_cv_cmd_timeout(sink, cv_cmd_timeout);
        apply_verify_cv(sink, raw.cv.verify_cv);
    }

    let (dt, dt_explicit) = if let Some(v) = raw.common.dt {
        (v, true)
    } else {
        (1.0, false)
    };

    Ok(OnceArgs {
        pv_source,
        cmd_timeout: effective_cmd_timeout,
        pv_cmd_timeout,
        dt,
        dt_explicit,
        min_dt: raw.common.min_dt.unwrap_or(0.01),
        max_dt: raw.common.max_dt.unwrap_or(60.0),
        output_format,
        cv_sink,
        pid_config,
        state_path: raw.common.state.clone(),
        name: raw.common.name.clone(),
        reset_accumulator: raw.common.reset_accumulator,
        scale: raw.common.scale.unwrap_or(1.0),
        cv_precision: raw.common.cv_precision.unwrap_or(2) as usize,
        log_path: raw.common.log.clone(),
        dry_run: raw.dry_run,
    })
}

pub(crate) fn parse_pipe(raw: &PipeRawArgs) -> Result<PipeArgs, CliError> {
    // Check pipe-specific rejections.
    raw.rejected.check()?;

    let output_format: OutputFormat = raw.common.format.clone().into();

    if matches!(output_format, OutputFormat::Json) {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with pipe's CV output — use --log for machine-readable telemetry",
        ));
    }

    let pid_flags = raw.pid.to_pid_flags()?;
    let pid_config = resolve_pid_config(&pid_flags, raw.common.state.as_deref())?;

    // Default state_write_interval for pipe: 1s.
    let state_write_interval = Some(
        raw.common
            .state_write_interval
            .unwrap_or(Duration::from_secs(1)),
    );

    let (dt, _dt_explicit) = if let Some(v) = raw.common.dt {
        (v, true)
    } else {
        (1.0, false)
    };

    Ok(PipeArgs {
        dt,
        pid_config,
        state_path: raw.common.state.clone(),
        name: raw.common.name.clone(),
        reset_accumulator: raw.common.reset_accumulator,
        scale: raw.common.scale.unwrap_or(1.0),
        cv_precision: raw.common.cv_precision.unwrap_or(2) as usize,
        log_path: raw.common.log.clone(),
        state_write_interval,
        state_fail_after: raw.common.state_fail_after.unwrap_or(10),
    })
}

// parse_loop sets many optional fields; splitting it would just move lines without adding clarity.
#[allow(clippy::too_many_lines, clippy::similar_names)]
pub(crate) fn parse_loop(raw: &LoopRawArgs) -> Result<LoopArgs, CliError> {
    let pv_source = raw.pv.to_pv_source_config()?.ok_or_else(|| {
        CliError::config("loop requires a PV source (--pv-file, --pv-cmd, or --pv-stdin)")
    })?;

    #[cfg(feature = "tui")]
    let tune = raw.tune;
    #[cfg(not(feature = "tui"))]
    let tune = false;

    #[cfg(feature = "tui")]
    let tune_history = raw.tune_history;
    #[cfg(not(feature = "tui"))]
    let tune_history: Option<usize> = None;

    #[cfg(feature = "tui")]
    let tune_step_kp = raw.tune_step_kp;
    #[cfg(not(feature = "tui"))]
    let tune_step_kp: Option<f64> = None;

    #[cfg(feature = "tui")]
    let tune_step_ki = raw.tune_step_ki;
    #[cfg(not(feature = "tui"))]
    let tune_step_ki: Option<f64> = None;

    #[cfg(feature = "tui")]
    let tune_step_kd = raw.tune_step_kd;
    #[cfg(not(feature = "tui"))]
    let tune_step_kd: Option<f64> = None;

    #[cfg(feature = "tui")]
    let tune_step_sp = raw.tune_step_sp;
    #[cfg(not(feature = "tui"))]
    let tune_step_sp: Option<f64> = None;

    let output_format: OutputFormat = raw.common.format.clone().into();

    if tune {
        if matches!(output_format, OutputFormat::Json) {
            return Err(CliError::config(
                "--tune and --format json are incompatible",
            ));
        }
        if raw.common.quiet {
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

    let mut cv_sink = raw.cv.to_cv_sink_config()?;

    // CV sink is required unless --dry-run is active.
    if cv_sink.is_none() && !raw.dry_run {
        return Err(CliError::config(
            "loop requires a CV sink (--cv-file, --cv-cmd, or --cv-stdout)",
        ));
    }

    // --format json and --cv-stdout conflict: both write to stdout.
    if matches!(output_format, OutputFormat::Json) && matches!(cv_sink, Some(CvSinkConfig::Stdout))
    {
        return Err(CliError::config(
            "--format json and --cv-stdout are incompatible — JSON iteration records and raw CV values would corrupt stdout; use --log for machine-readable telemetry",
        ));
    }

    let cv_cmd_timeout = raw.common.cv_cmd_timeout.map(Duration::from_secs_f64);
    if let Some(ref mut sink) = cv_sink {
        apply_cv_cmd_timeout(sink, cv_cmd_timeout);
        apply_verify_cv(sink, raw.cv.verify_cv);
    }

    let interval = raw
        .interval
        .ok_or_else(|| CliError::config("loop requires --interval"))?;

    let pid_flags = raw.pid.to_pid_flags()?;
    let mut pid_config = resolve_pid_config(&pid_flags, raw.common.state.as_deref())?;

    // Default max_dt is 3×interval or 60.0 if interval is very large.
    let interval_secs = interval.as_secs_f64();

    // Set tt_upper_bound from interval.
    if raw.pid.anti_windup_tt.is_none() {
        pid_config.tt_upper_bound = Some(100.0 * interval_secs);
    }
    let max_dt_default = (interval_secs * 3.0).min(60.0);
    // Ensure max_dt_default is never below min_dt default (0.01).
    let max_dt_default = max_dt_default.max(0.01);

    let pv_stdin_timeout = raw
        .pv
        .pv_stdin_timeout
        .map_or_else(|| UserSet::Default(interval), UserSet::Explicit);

    // Default cmd-timeout: min(interval, 30s).
    let effective_cmd_timeout = raw.common.cmd_timeout.map_or_else(
        || interval.min(Duration::from_secs(30)),
        Duration::from_secs_f64,
    );
    let pv_cmd_timeout = raw
        .common
        .pv_cmd_timeout
        .map_or(effective_cmd_timeout, Duration::from_secs_f64);

    // Default state_write_interval for loop: max(tick_interval, 100ms).
    let min_flush = Duration::from_millis(100);
    let default_state_write_interval = interval.max(min_flush);
    let state_write_interval = raw.common.state_write_interval.map_or_else(
        || UserSet::Default(Some(default_state_write_interval)),
        |t| UserSet::Explicit(Some(t)),
    );

    let (dt, _dt_explicit) = if let Some(v) = raw.common.dt {
        (v, true)
    } else {
        (1.0, false)
    };

    let _ = dt; // dt is not stored in LoopArgs (loop uses wall-clock)

    #[cfg(unix)]
    let socket_path = raw.socket.clone();
    #[cfg(unix)]
    let socket_mode = raw.socket_mode.unwrap_or(0o600);

    Ok(LoopArgs {
        interval,
        pv_source,
        cv_sink,
        pid_config,
        state_path: raw.common.state.clone(),
        name: raw.common.name.clone(),
        reset_accumulator: raw.common.reset_accumulator,
        scale: raw.common.scale.unwrap_or(1.0),
        cv_precision: raw.common.cv_precision.unwrap_or(2) as usize,
        output_format,
        cmd_timeout: effective_cmd_timeout,
        pv_cmd_timeout,
        safe_cv: raw.common.safe_cv,
        cv_fail_after: raw.common.cv_fail_after.unwrap_or(10),
        fail_after: raw.common.fail_after,
        min_dt: raw
            .common
            .min_dt
            .map_or_else(|| UserSet::Default(0.01), UserSet::Explicit),
        max_dt: raw
            .common
            .max_dt
            .map_or_else(|| UserSet::Default(max_dt_default), UserSet::Explicit),
        dt_clamp: raw.common.dt_clamp,
        log_path: raw.common.log.clone(),
        dry_run: raw.dry_run,
        pv_stdin_timeout,
        verify_cv: raw.cv.verify_cv,
        state_write_interval,
        state_fail_after: raw.common.state_fail_after.unwrap_or(10),
        tune,
        tune_history: tune_history.unwrap_or(60).max(1),
        tune_step_kp: tune_step_kp.unwrap_or(0.1),
        tune_step_ki: tune_step_ki.unwrap_or(0.01),
        tune_step_kd: tune_step_kd.unwrap_or(0.05),
        tune_step_sp: tune_step_sp.unwrap_or(0.1),
        units: raw.common.units.clone(),
        quiet: raw.common.quiet,
        verbose: raw.common.verbose,
        #[cfg(unix)]
        socket_path,
        #[cfg(unix)]
        socket_mode,
    })
}

pub(crate) fn parse_status_flags(raw: &StatusRawArgs) -> Result<StatusFlags, CliError> {
    #[cfg(unix)]
    {
        if raw.state.is_none() && raw.socket.is_none() {
            return Err(CliError::config(
                "status requires --state or --socket (or both)",
            ));
        }
        Ok(StatusFlags {
            state_path: raw.state.clone(),
            socket_path: raw.socket.clone(),
        })
    }
    #[cfg(not(unix))]
    {
        if raw.state.is_none() {
            return Err(CliError::config("status requires --state"));
        }
        Ok(StatusFlags {
            state_path: raw.state.clone(),
        })
    }
}

#[cfg(unix)]
pub(crate) fn parse_set_args(raw: &super::raw::SetRawArgs) -> SetArgs {
    SetArgs {
        socket_path: raw.socket.clone(),
        param: raw.param.clone(),
        value: raw.value,
    }
}

#[cfg(unix)]
pub(crate) fn get_socket_path(raw: &super::raw::SocketOnlyArgs) -> PathBuf {
    raw.socket.clone()
}

pub(crate) fn get_state_path(raw: &super::raw::StateOnlyArgs) -> Result<PathBuf, CliError> {
    raw.state
        .clone()
        .ok_or_else(|| CliError::config("requires --state"))
}

/// Merges CLI PID flags with any values stored in the state file.
///
/// Priority: CLI flag > state file value > `PidConfig` default.
/// Returns an error (exit 3) if setpoint is absent from both CLI and state file.
pub(crate) fn resolve_pid_config(
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
        anti_windup: flags
            .anti_windup
            .unwrap_or(AntiWindupStrategy::BackCalculation),
        anti_windup_tt: flags.anti_windup_tt,
        tt_upper_bound: None,
    })
}

// ---------------------------------------------------------------------------
// Helper functions used by parsing and main.rs
// ---------------------------------------------------------------------------

/// If `timeout` is `Some`, sets it on the `Cmd` variant.
pub(crate) const fn apply_cv_cmd_timeout(cv_sink: &mut CvSinkConfig, timeout: Option<Duration>) {
    if let (Some(t), CvSinkConfig::Cmd { timeout: slot, .. }) = (timeout, cv_sink) {
        *slot = Some(t);
    }
}

/// Sets the verify flag on `File` variant sinks.
pub(crate) const fn apply_verify_cv(cv_sink: &mut CvSinkConfig, verify: bool) {
    if let CvSinkConfig::File { verify: slot, .. } = cv_sink {
        *slot = verify;
    }
}

pub(crate) fn resolve_pv(source: &PvSourceConfig, cmd_timeout: Duration) -> io::Result<f64> {
    match source {
        PvSourceConfig::Literal(v) => Ok(*v),
        PvSourceConfig::File(path) => FilePvSource::new(path.clone()).read_pv(),
        PvSourceConfig::Cmd(cmd) => CmdPvSource::new(cmd.clone(), cmd_timeout).read_pv(),
        PvSourceConfig::Stdin => unreachable!("--pv-stdin is only valid for loop, not once"),
    }
}

pub(crate) fn parse_f64_value(flag: &str, value: &str) -> Result<f64, CliError> {
    value.parse::<f64>().map_err(|error| {
        CliError::config(format!("{flag} expects a float, got `{value}`: {error}"))
    })
}
