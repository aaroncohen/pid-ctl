use pid_ctl::adapters::{CmdPvSource, FilePvSource, PvSource};
use pid_ctl::app::{SessionConfig, StateStore};
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use crate::CliError;

// ---------------------------------------------------------------------------
// Enums / structs
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandKind {
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
pub(crate) struct PidFlags {
    pub(crate) setpoint: Option<f64>,
    pub(crate) kp: Option<f64>,
    pub(crate) ki: Option<f64>,
    pub(crate) kd: Option<f64>,
    pub(crate) out_min: Option<f64>,
    pub(crate) out_max: Option<f64>,
    pub(crate) deadband: Option<f64>,
    pub(crate) setpoint_ramp: Option<f64>,
    pub(crate) slew_rate: Option<f64>,
    pub(crate) pv_filter_alpha: Option<f64>,
    pub(crate) anti_windup: Option<AntiWindupStrategy>,
    pub(crate) anti_windup_tt: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CommonArgs {
    pub(crate) pv_source: Option<PvSourceConfig>,
    pub(crate) pid_flags: PidFlags,
    pub(crate) output_format: OutputFormat,
    pub(crate) dt: f64,
    /// True when `--dt` was passed (fixed dt; bypasses wall-clock / bounds where documented).
    pub(crate) dt_explicit: bool,
    /// Clamp measured dt to `[min_dt, max_dt]` instead of skipping (`loop` / `pipe`; `once` auto-dt always clamps).
    pub(crate) dt_clamp: bool,
    pub(crate) cv_sink: Option<CvSinkConfig>,
    pub(crate) state_path: Option<PathBuf>,
    pub(crate) name: Option<String>,
    pub(crate) reset_accumulator: bool,
    pub(crate) scale: Option<f64>,
    pub(crate) cv_precision: Option<u32>,
    pub(crate) cmd_timeout: Option<Duration>,
    pub(crate) cv_cmd_timeout: Option<Duration>,
    pub(crate) pv_cmd_timeout: Option<Duration>,
    pub(crate) loop_interval: Option<Duration>,
    pub(crate) safe_cv: Option<f64>,
    pub(crate) cv_fail_after: Option<u32>,
    pub(crate) fail_after: Option<u32>,
    pub(crate) min_dt: Option<f64>,
    pub(crate) max_dt: Option<f64>,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) pv_stdin_timeout: Option<Duration>,
    pub(crate) verify_cv: bool,
    pub(crate) state_write_interval: Option<Duration>,
    pub(crate) state_fail_after: Option<u32>,
    /// Loop + dashboard only (`--tune`, `--tune-history`, `--tune-step-*`).
    pub(crate) tune: bool,
    pub(crate) tune_history: Option<usize>,
    pub(crate) tune_step_kp: Option<f64>,
    pub(crate) tune_step_ki: Option<f64>,
    pub(crate) tune_step_kd: Option<f64>,
    pub(crate) tune_step_sp: Option<f64>,
    pub(crate) units: Option<String>,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) explicit_max_dt: bool,
    pub(crate) explicit_min_dt: bool,
    pub(crate) explicit_pv_stdin_timeout: bool,
    pub(crate) explicit_state_write_interval: bool,
    pub(crate) socket_path: Option<PathBuf>,
    pub(crate) socket_mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OnceArgs {
    pub(crate) pv_source: PvSourceConfig,
    pub(crate) cmd_timeout: Duration,
    pub(crate) pv_cmd_timeout: Duration,
    pub(crate) dt: f64,
    pub(crate) dt_explicit: bool,
    pub(crate) min_dt: f64,
    pub(crate) max_dt: f64,
    pub(crate) output_format: OutputFormat,
    pub(crate) cv_sink: Option<CvSinkConfig>,
    pub(crate) pid_config: PidConfig,
    pub(crate) state_path: Option<PathBuf>,
    pub(crate) name: Option<String>,
    pub(crate) reset_accumulator: bool,
    pub(crate) scale: f64,
    pub(crate) cv_precision: usize,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) dry_run: bool,
}

impl OnceArgs {
    pub(crate) fn session_config(&self) -> SessionConfig {
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
pub(crate) struct PipeArgs {
    pub(crate) dt: f64,
    pub(crate) pid_config: PidConfig,
    pub(crate) state_path: Option<PathBuf>,
    pub(crate) name: Option<String>,
    pub(crate) reset_accumulator: bool,
    pub(crate) scale: f64,
    pub(crate) cv_precision: usize,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) state_write_interval: Option<Duration>,
    pub(crate) state_fail_after: u32,
}

impl PipeArgs {
    pub(crate) fn session_config(&self) -> SessionConfig {
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
    pub(crate) reset_accumulator: bool,
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
    pub(crate) state_fail_after: u32,
    pub(crate) tune: bool,
    pub(crate) tune_history: usize,
    pub(crate) tune_step_kp: f64,
    pub(crate) tune_step_ki: f64,
    pub(crate) tune_step_kd: f64,
    pub(crate) tune_step_sp: f64,
    pub(crate) units: Option<String>,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) explicit_max_dt: bool,
    pub(crate) explicit_min_dt: bool,
    pub(crate) explicit_pv_stdin_timeout: bool,
    pub(crate) explicit_state_write_interval: bool,
    pub(crate) socket_path: Option<PathBuf>,
    pub(crate) socket_mode: u32,
}

impl LoopArgs {
    pub(crate) fn session_config(&self) -> SessionConfig {
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

/// Which PV source was specified on the CLI.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PvSourceConfig {
    Literal(f64),
    File(PathBuf),
    Cmd(String),
    /// `loop --pv-stdin`: one line per tick, with a per-tick timeout.
    Stdin,
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

pub(crate) struct SetArgs {
    pub(crate) socket_path: PathBuf,
    pub(crate) param: String,
    pub(crate) value: f64,
}

pub(crate) struct StatusFlags {
    pub(crate) state_path: Option<PathBuf>,
    pub(crate) socket_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Parsing functions
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

/// Parses `--state <path>` from a minimal argument list.
///
/// Used by `status`, `purge`, and `init` which only need a state path.
pub(crate) fn parse_state_flag(args: &[String], command: &str) -> Result<PathBuf, CliError> {
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

pub(crate) fn parse_status_flags(args: &[String]) -> Result<StatusFlags, CliError> {
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

/// Parse the `--socket <path>` flag required by socket-control subcommands.
pub(crate) fn parse_socket_control_flag(args: &[String], cmd: &str) -> Result<PathBuf, CliError> {
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

pub(crate) fn parse_set_args(args: &[String]) -> Result<SetArgs, CliError> {
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

pub(crate) fn parse_once(args: &[String]) -> Result<OnceArgs, CliError> {
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

pub(crate) fn parse_pipe(args: &[String]) -> Result<PipeArgs, CliError> {
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

// parse_loop sets many optional fields; splitting it would just move lines without adding clarity.
#[allow(clippy::too_many_lines)]
pub(crate) fn parse_loop(args: &[String]) -> Result<LoopArgs, CliError> {
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

    // --format json and --cv-stdout conflict: both write to stdout (plan §Incompatible Flag Combinations).
    if matches!(common.output_format, OutputFormat::Json)
        && matches!(common.cv_sink, Some(CvSinkConfig::Stdout))
    {
        return Err(CliError::config(
            "--format json and --cv-stdout are incompatible — JSON iteration records and raw CV values would corrupt stdout; use --log for machine-readable telemetry",
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

    let mut pid_config = resolve_pid_config(&common.pid_flags, common.state_path.as_deref())?;

    // Default max_dt is 3×interval or 60.0 if interval is very large.
    let interval_secs = interval.as_secs_f64();

    // Set tt_upper_bound from interval (plan §Anti-Windup: auto Tt clamped to [dt, 100×interval]).
    // Only set if the user did not explicitly provide --anti-windup-tt (which bypasses auto-Tt entirely).
    if common.pid_flags.anti_windup_tt.is_none() {
        pid_config.tt_upper_bound = Some(100.0 * interval_secs);
    }
    let max_dt_default = (interval_secs * 3.0).min(60.0);
    // Ensure max_dt_default is never below min_dt default (0.01).
    let max_dt_default = max_dt_default.max(0.01);

    let pv_stdin_timeout = common.pv_stdin_timeout.unwrap_or(interval);
    // Default cmd-timeout: min(interval, 30s) per plan §Command Timeouts.
    let effective_cmd_timeout = common
        .cmd_timeout
        .unwrap_or_else(|| interval.min(Duration::from_secs(30)));
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
        verbose: common.verbose,
        explicit_max_dt: common.explicit_max_dt,
        explicit_min_dt: common.explicit_min_dt,
        explicit_pv_stdin_timeout: common.explicit_pv_stdin_timeout,
        explicit_state_write_interval: common.explicit_state_write_interval,
        socket_path: common.socket_path,
        socket_mode: common.socket_mode.unwrap_or(0o600),
    })
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

pub(crate) fn parse_common_args(
    args: &[String],
    command_kind: CommandKind,
) -> Result<CommonArgs, CliError> {
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

pub(crate) fn handle_loop_only_option(
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

pub(crate) fn handle_pid_option(
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
            parsed.pid_flags.setpoint_ramp =
                Some(parse_f64_flag("--setpoint-ramp", args, index)?);
        }
        "--slew-rate" => {
            parsed.pid_flags.slew_rate = Some(parse_f64_flag("--slew-rate", args, index)?);
        }
        // --ramp-rate is a plan-documented alias for --slew-rate (plan §Safety uses --ramp-rate).
        "--ramp-rate" => {
            parsed.pid_flags.slew_rate = Some(parse_f64_flag("--ramp-rate", args, index)?);
        }
        "--pv-filter" => {
            parsed.pid_flags.pv_filter_alpha =
                Some(parse_f64_flag("--pv-filter", args, index)?);
        }
        "--anti-windup" => {
            let val = next_value("--anti-windup", args, index)?;
            let strategy = match val.as_str() {
                "back-calc" | "back-calculation" => AntiWindupStrategy::BackCalculation,
                "clamp" => AntiWindupStrategy::Clamp,
                "none" => AntiWindupStrategy::None,
                other => {
                    return Err(CliError::config(format!(
                        "--anti-windup must be `back-calc`, `clamp`, or `none`, got `{other}`"
                    )));
                }
            };
            parsed.pid_flags.anti_windup = Some(strategy);
        }
        "--anti-windup-tt" => {
            parsed.pid_flags.anti_windup_tt =
                Some(parse_f64_flag("--anti-windup-tt", args, index)?);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

pub(crate) fn handle_common_option(
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
        "--verbose" => {
            parsed.verbose = true;
        }
        "--state-fail-after" => {
            parsed.state_fail_after = Some(parse_u32_flag("--state-fail-after", args, index)?);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

pub(crate) fn handle_cv_option(
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

pub(crate) fn handle_pv_option(
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

pub(crate) fn set_cv_sink(
    current: &mut Option<CvSinkConfig>,
    next: CvSinkConfig,
) -> Result<(), CliError> {
    if current.is_some() {
        return Err(CliError::config("only one CV sink may be specified"));
    }

    *current = Some(next);
    Ok(())
}

pub(crate) fn set_pv_source(
    current: &mut Option<PvSourceConfig>,
    next: PvSourceConfig,
) -> Result<(), CliError> {
    if current.is_some() {
        return Err(CliError::config("only one PV source may be specified"));
    }

    *current = Some(next);
    Ok(())
}

pub(crate) fn resolve_pv(source: &PvSourceConfig, cmd_timeout: Duration) -> io::Result<f64> {
    match source {
        PvSourceConfig::Literal(v) => Ok(*v),
        PvSourceConfig::File(path) => FilePvSource::new(path.clone()).read_pv(),
        PvSourceConfig::Cmd(cmd) => CmdPvSource::new(cmd.clone(), cmd_timeout).read_pv(),
        PvSourceConfig::Stdin => unreachable!("--pv-stdin is only valid for loop, not once"),
    }
}

pub(crate) fn parse_output_format(
    args: &[String],
    index: &mut usize,
) -> Result<OutputFormat, CliError> {
    let value = next_value("--format", args, index)?;
    match value.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => Err(CliError::config(format!(
            "--format must be `text` or `json`, got `{other}`"
        ))),
    }
}

pub(crate) fn parse_f64_flag(
    flag: &str,
    args: &[String],
    index: &mut usize,
) -> Result<f64, CliError> {
    let value = next_value(flag, args, index)?;
    parse_f64_value(flag, &value)
}

pub(crate) fn parse_f64_value(flag: &str, value: &str) -> Result<f64, CliError> {
    value.parse::<f64>().map_err(|error| {
        CliError::config(format!("{flag} expects a float, got `{value}`: {error}"))
    })
}

pub(crate) fn parse_u32_flag(
    flag: &str,
    args: &[String],
    index: &mut usize,
) -> Result<u32, CliError> {
    let value = next_value(flag, args, index)?;
    value.parse::<u32>().map_err(|error| {
        CliError::config(format!(
            "{flag} expects a non-negative integer, got `{value}`: {error}"
        ))
    })
}

pub(crate) fn parse_path_flag(
    flag: &str,
    args: &[String],
    index: &mut usize,
) -> Result<PathBuf, CliError> {
    next_value(flag, args, index).map(PathBuf::from)
}

pub(crate) fn parse_string_flag(
    flag: &str,
    args: &[String],
    index: &mut usize,
) -> Result<String, CliError> {
    next_value(flag, args, index)
}

pub(crate) fn next_value(flag: &str, args: &[String], index: &mut usize) -> Result<String, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError::config(format!("{flag} requires a value")))
}

// ---------------------------------------------------------------------------
// Helper functions used by parsing
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
