use super::raw::{
    AutotuneRawArgs, FfRawArgs, LoopRawArgs, OnceRawArgs, PipeRawArgs, StatusRawArgs,
};
use super::types::{
    AutotuneArgs, CvMode, CvSinkConfig, LoopArgs, LoopFfSource, LoopPvSource, LoopRuntimeConfig,
    OnceArgs, OnceFfSource, OncePvSource, OutputFormat, PidFlags, PipeArgs, SetArgs, StatusFlags,
};
use super::user_set::UserSet;
use crate::CliError;
use pid_ctl::adapters::{CmdPvSource, FilePvSource, PvSource};
use pid_ctl::app::StateStore;
use pid_ctl::app::defaults::{
    ANTI_WINDUP_TT_INTERVAL_MULTIPLIER, DEFAULT_CMD_TIMEOUT_CAP, MAX_DT_DEFAULT,
    MAX_DT_INTERVAL_MULTIPLIER, MIN_DT_DEFAULT, MIN_STATE_FLUSH,
};
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

    let cv_mode = match (raw.dry_run, cv_sink) {
        (true, _) | (false, None) => CvMode::DryRun,
        (false, Some(cfg)) => CvMode::Sink(cfg),
    };

    let (dt, dt_explicit) = if let Some(v) = raw.common.dt {
        (v, true)
    } else {
        (1.0, false)
    };

    let ff_source = parse_once_ff_source(&raw.ff)?;

    Ok(OnceArgs {
        pv_source,
        ff_source,
        cmd_timeout: effective_cmd_timeout,
        pv_cmd_timeout,
        dt,
        dt_explicit,
        min_dt: raw.common.min_dt.unwrap_or(MIN_DT_DEFAULT),
        max_dt: raw.common.max_dt.unwrap_or(MAX_DT_DEFAULT),
        output_format,
        cv_mode,
        pid_config,
        state_path: raw.common.state.clone(),
        name: raw.common.name.clone(),
        reset_accumulator: raw.common.reset_accumulator,
        scale: raw.common.scale.unwrap_or(1.0),
        cv_precision: raw.common.cv_precision.unwrap_or(2) as usize,
        log_path: raw.common.log.clone(),
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
    let tune_history: usize = raw.tune_history.unwrap_or(60).max(1);
    #[cfg(not(feature = "tui"))]
    let tune_history: usize = 60;

    #[cfg(feature = "tui")]
    let tune_step_kp: f64 = raw.tune_step_kp.unwrap_or(0.1);
    #[cfg(not(feature = "tui"))]
    let tune_step_kp: f64 = 0.1;

    #[cfg(feature = "tui")]
    let tune_step_ki: f64 = raw.tune_step_ki.unwrap_or(0.01);
    #[cfg(not(feature = "tui"))]
    let tune_step_ki: f64 = 0.01;

    #[cfg(feature = "tui")]
    let tune_step_kd: f64 = raw.tune_step_kd.unwrap_or(0.05);
    #[cfg(not(feature = "tui"))]
    let tune_step_kd: f64 = 0.05;

    #[cfg(feature = "tui")]
    let tune_step_sp: f64 = raw.tune_step_sp.unwrap_or(0.1);
    #[cfg(not(feature = "tui"))]
    let tune_step_sp: f64 = 0.1;

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
        if matches!(pv_source, LoopPvSource::Stdin) {
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

    let interval_secs = interval.as_secs_f64();

    if raw.pid.anti_windup_tt.is_none() {
        pid_config.tt_upper_bound = Some(ANTI_WINDUP_TT_INTERVAL_MULTIPLIER * interval_secs);
    }
    let max_dt_default =
        (interval_secs * MAX_DT_INTERVAL_MULTIPLIER).clamp(MIN_DT_DEFAULT, MAX_DT_DEFAULT);

    let pv_stdin_timeout = raw
        .pv
        .pv_stdin_timeout
        .map_or_else(|| UserSet::Default(interval), UserSet::Explicit);

    // Default cmd-timeout: min(interval, DEFAULT_CMD_TIMEOUT_CAP).
    let effective_cmd_timeout = raw.common.cmd_timeout.map_or_else(
        || interval.min(DEFAULT_CMD_TIMEOUT_CAP),
        Duration::from_secs_f64,
    );
    let pv_cmd_timeout = raw
        .common
        .pv_cmd_timeout
        .map_or(effective_cmd_timeout, Duration::from_secs_f64);

    // Default state_write_interval for loop: max(tick_interval, MIN_STATE_FLUSH).
    let default_state_write_interval = interval.max(MIN_STATE_FLUSH);
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

    let max_dt = raw
        .common
        .max_dt
        .map_or_else(|| UserSet::Default(max_dt_default), UserSet::Explicit);

    let ff_source = parse_loop_ff_source(&raw.ff)?;

    Ok(LoopArgs {
        runtime: LoopRuntimeConfig {
            interval,
            max_dt,
            pv_stdin_timeout,
            state_write_interval,
        },
        pv_source,
        ff_source,
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
        dt_clamp: raw.common.dt_clamp,
        log_path: raw.common.log.clone(),
        dry_run: raw.dry_run,
        verify_cv: raw.cv.verify_cv,
        state_fail_after: raw.common.state_fail_after.unwrap_or(10),
        tune,
        tune_history,
        tune_step_kp,
        tune_step_ki,
        tune_step_kd,
        tune_step_sp,
        units: raw.common.units.clone(),
        quiet: raw.common.quiet,
        verbose: raw.common.verbose,
        #[cfg(unix)]
        socket_path,
        #[cfg(unix)]
        socket_mode,
        max_iterations: raw.max_iterations,
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
        feedforward_gain: flags.feedforward_gain.unwrap_or(0.0),
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

pub(crate) fn resolve_pv(source: &OncePvSource, cmd_timeout: Duration) -> io::Result<f64> {
    match source {
        OncePvSource::Literal(v) => Ok(*v),
        OncePvSource::File(path) => FilePvSource::new(path.clone()).read_pv(),
        OncePvSource::Cmd(cmd) => CmdPvSource::new(cmd.clone(), cmd_timeout).read_pv(),
    }
}

/// Minimum allowed autotune duration.
const AUTOTUNE_MIN_DURATION_SECS: f64 = 10.0;

pub(crate) fn parse_autotune(raw: &AutotuneRawArgs) -> Result<AutotuneArgs, CliError> {
    let duration_secs = raw.duration.as_secs_f64();
    if duration_secs < AUTOTUNE_MIN_DURATION_SECS {
        return Err(CliError::config(format!(
            "--duration must be at least {AUTOTUNE_MIN_DURATION_SECS}s for a meaningful relay test, got {duration_secs:.1}s"
        )));
    }

    let cmd_timeout = raw
        .cmd_timeout
        .map_or(raw.interval.min(Duration::from_secs(5)), |s| {
            Duration::from_secs_f64(s)
        });

    let cfg = pid_ctl::autotune::AutotuneConfig {
        bias: raw.bias,
        amp: raw.amp,
        out_min: raw.out_min,
        out_max: raw.out_max,
    };
    cfg.validate().map_err(CliError::config)?;

    Ok(AutotuneArgs {
        pv_cmd: raw.pv_cmd.clone(),
        cv_cmd: raw.cv_cmd.clone(),
        bias: raw.bias,
        amp: raw.amp,
        duration: raw.duration,
        rule: raw.rule.clone().into(),
        out_min: raw.out_min,
        out_max: raw.out_max,
        interval: raw.interval,
        cmd_timeout,
        cv_precision: raw.cv_precision as usize,
        state: raw.state.clone(),
    })
}

fn parse_once_ff_source(ff: &FfRawArgs) -> Result<OnceFfSource, CliError> {
    let count = u32::from(ff.value.is_some())
        + u32::from(ff.from_file.is_some())
        + u32::from(ff.cmd.is_some());
    if count > 1 {
        return Err(CliError::config(
            "only one FF source may be specified (--ff-value, --ff-from-file, or --ff-cmd)",
        ));
    }
    if let Some(v) = ff.value {
        Ok(OnceFfSource::Literal(v))
    } else if let Some(ref path) = ff.from_file {
        Ok(OnceFfSource::File(path.clone()))
    } else if let Some(ref cmd) = ff.cmd {
        Ok(OnceFfSource::Cmd(cmd.clone()))
    } else {
        Ok(OnceFfSource::Zero)
    }
}

fn parse_loop_ff_source(ff: &FfRawArgs) -> Result<LoopFfSource, CliError> {
    if ff.value.is_some() {
        return Err(CliError::config(
            "--ff-value is only supported with once — use --ff-from-file or --ff-cmd for loop",
        ));
    }
    let count = u32::from(ff.from_file.is_some()) + u32::from(ff.cmd.is_some());
    if count > 1 {
        return Err(CliError::config(
            "only one FF source may be specified (--ff-from-file or --ff-cmd)",
        ));
    }
    if let Some(ref path) = ff.from_file {
        Ok(LoopFfSource::File(path.clone()))
    } else if let Some(ref cmd) = ff.cmd {
        Ok(LoopFfSource::Cmd(cmd.clone()))
    } else {
        Ok(LoopFfSource::Zero)
    }
}

pub(crate) fn parse_replay(
    raw: &super::raw::ReplayRawArgs,
) -> Result<super::types::ReplayArgs, CliError> {
    if raw.diff && raw.output_log.is_some() {
        return Err(CliError::config(
            "--diff and --output-log are mutually exclusive: --diff suppresses the full log",
        ));
    }

    Ok(super::types::ReplayArgs {
        log: raw.log.clone(),
        kp: raw.kp,
        ki: raw.ki,
        kd: raw.kd,
        out_min: raw.out_min.unwrap_or(f64::NEG_INFINITY),
        out_max: raw.out_max.unwrap_or(f64::INFINITY),
        output_log: raw.output_log.clone(),
        diff: raw.diff,
    })
}

pub(crate) fn parse_f64_value(flag: &str, value: &str) -> Result<f64, CliError> {
    value.parse::<f64>().map_err(|error| {
        CliError::config(format!("{flag} expects a float, got `{value}`: {error}"))
    })
}
