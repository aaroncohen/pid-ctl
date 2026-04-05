use pid_ctl::adapters::{CmdPvSource, CvSink, FileCvSink, FilePvSource, PvSource, StdoutCvSink};
use pid_ctl::app::{self, ControllerSession, SessionConfig, StateSnapshot, StateStore};
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::env;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let exit_code = match run(&args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            error.exit_code
        }
    };

    process::exit(exit_code);
}

fn run(args: &[String]) -> Result<(), CliError> {
    let Some((command, rest)) = args.split_first() else {
        return Err(CliError::config(
            "usage: pid-ctl <once|pipe|status|purge|init> [OPTIONS]",
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
        "status" => {
            let state_path = parse_state_flag(rest, "status")?;
            run_status(&state_path)
        }
        "purge" => {
            let state_path = parse_state_flag(rest, "purge")?;
            run_purge(&state_path)
        }
        "init" => {
            let state_path = parse_state_flag(rest, "init")?;
            run_init(&state_path)
        }
        other => Err(CliError::config(format!(
            "unknown subcommand `{other}`; expected `once`, `pipe`, `status`, `purge`, or `init`"
        ))),
    }
}

fn run_once(args: &OnceArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = build_cv_sink(&args.cv_sink, args.cv_precision);

    let raw_pv = resolve_pv(&args.pv_source, args.cmd_timeout)
        .map_err(|error| CliError::new(1, format!("failed to read PV: {error}")))?;
    let scaled_pv = raw_pv * args.scale;
    match session.process_pv(scaled_pv, args.dt, sink.as_mut()) {
        Ok(outcome) => {
            if matches!(args.output_format, OutputFormat::Json) {
                print_iteration_json(&outcome.record)?;
            }

            if let Some(error) = outcome.state_write_failed {
                return Err(CliError::new(
                    4,
                    format!("state persistence failed after CV was emitted: {error}"),
                ));
            }

            Ok(())
        }
        Err(error) => Err(CliError::new(5, error.to_string())),
    }
}

fn run_pipe(args: &PipeArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = StdoutCvSink {
        precision: args.cv_precision,
    };

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

        if let Some(error) = outcome.state_write_failed {
            eprintln!("state persistence failed: {error}");
        }
    }

    Ok(())
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

fn build_cv_sink(cv_sink: &CvSinkConfig, precision: usize) -> Box<dyn CvSink> {
    match cv_sink {
        CvSinkConfig::Stdout => Box::new(StdoutCvSink { precision }),
        CvSinkConfig::File(path) => {
            let mut sink = FileCvSink::new(path.clone());
            sink.precision = precision;
            Box::new(sink)
        }
    }
}

fn print_iteration_json(record: &pid_ctl::app::IterationRecord) -> Result<(), CliError> {
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
    let cv_sink = common.cv_sink.ok_or_else(|| {
        CliError::config("once requires exactly one CV sink: --cv-stdout or --cv-file <path>")
    })?;

    if matches!(common.output_format, OutputFormat::Json) && matches!(cv_sink, CvSinkConfig::Stdout)
    {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with --cv-stdout — use --log for machine-readable telemetry",
        ));
    }

    let pid_config = resolve_pid_config(&common.pid_flags, common.state_path.as_deref())?;

    Ok(OnceArgs {
        pv_source,
        cmd_timeout: common.cmd_timeout.unwrap_or(Duration::from_secs(5)),
        dt: common.dt,
        output_format: common.output_format,
        cv_sink,
        pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
        scale: common.scale.unwrap_or(1.0),
        cv_precision: common.cv_precision.unwrap_or(2) as usize,
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

    Ok(PipeArgs {
        dt: common.dt,
        pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
        scale: common.scale.unwrap_or(1.0),
        cv_precision: common.cv_precision.unwrap_or(2) as usize,
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
        ..CommonArgs::default()
    };

    let mut index = 0;
    while index < args.len() {
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

        match args[index].as_str() {
            "--cmd-timeout" => {
                let secs = parse_f64_flag("--cmd-timeout", args, &mut index)?;
                parsed.cmd_timeout = Some(Duration::from_secs_f64(secs));
            }
            "--dry-run" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "--dry-run is not meaningful with pipe — pipe has no side effects to suppress",
                    ));
                }

                return Err(CliError::config(
                    "--dry-run is not implemented yet in this slice",
                ));
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
            set_cv_sink(&mut parsed.cv_sink, CvSinkConfig::File(path))?;
        }
        "--cv-cmd" => {
            if matches!(command_kind, CommandKind::Pipe) {
                return Err(pipe_err());
            }
            return Err(CliError::config(
                "--cv-cmd is not implemented yet in this slice",
            ));
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
            return Err(CliError::config(
                "--pv-stdin is not implemented yet in this slice",
            ));
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OutputFormat {
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
struct CommonArgs {
    pv_source: Option<PvSourceConfig>,
    pid_flags: PidFlags,
    output_format: OutputFormat,
    dt: f64,
    cv_sink: Option<CvSinkConfig>,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
    scale: Option<f64>,
    cv_precision: Option<u32>,
    cmd_timeout: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
struct OnceArgs {
    pv_source: PvSourceConfig,
    cmd_timeout: Duration,
    dt: f64,
    output_format: OutputFormat,
    cv_sink: CvSinkConfig,
    pid_config: PidConfig,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
    scale: f64,
    cv_precision: usize,
}

/// Which PV source was specified on the CLI.
#[derive(Clone, Debug, PartialEq)]
enum PvSourceConfig {
    Literal(f64),
    File(PathBuf),
    Cmd(String),
}

impl OnceArgs {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            name: self.name.clone(),
            pid: self.pid_config.clone(),
            state_store: self.state_path.clone().map(StateStore::new),
            reset_accumulator: self.reset_accumulator,
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
}

impl PipeArgs {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            name: self.name.clone(),
            pid: self.pid_config.clone(),
            state_store: self.state_path.clone().map(StateStore::new),
            reset_accumulator: self.reset_accumulator,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum CvSinkConfig {
    Stdout,
    File(PathBuf),
}

#[derive(Debug)]
struct CliError {
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
