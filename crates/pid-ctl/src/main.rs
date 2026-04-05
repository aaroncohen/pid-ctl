use pid_ctl::adapters::{CvSink, FileCvSink, StdoutCvSink};
use pid_ctl::app::{ControllerSession, SessionConfig, StateStore};
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::env;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process;

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
        return Err(CliError::config("usage: pid-ctl <once|pipe> [OPTIONS]"));
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
        other => Err(CliError::config(format!(
            "unknown subcommand `{other}`; expected `once` or `pipe`"
        ))),
    }
}

fn run_once(args: &OnceArgs) -> Result<(), CliError> {
    let mut session = ControllerSession::new(args.session_config())
        .map_err(|error| CliError::config(error.to_string()))?;
    let mut sink = build_cv_sink(&args.cv_sink);

    match session.process_pv(args.pv, args.dt, sink.as_mut()) {
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
    let mut sink = StdoutCvSink;

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| CliError::new(1, format!("stdin read failed: {error}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let pv = parse_f64_value("--stdin", trimmed)?;
        let outcome = session
            .process_pv(pv, args.dt, &mut sink)
            .map_err(|error| CliError::new(1, error.to_string()))?;

        if let Some(error) = outcome.state_write_failed {
            eprintln!("state persistence failed: {error}");
        }
    }

    Ok(())
}

fn build_cv_sink(cv_sink: &CvSinkConfig) -> Box<dyn CvSink> {
    match cv_sink {
        CvSinkConfig::Stdout => Box::new(StdoutCvSink),
        CvSinkConfig::File(path) => Box::new(FileCvSink::new(path.clone())),
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
    let pv = common
        .pv
        .ok_or_else(|| CliError::config("once requires --pv <float>"))?;
    let cv_sink = common.cv_sink.ok_or_else(|| {
        CliError::config("once requires exactly one CV sink: --cv-stdout or --cv-file <path>")
    })?;

    if matches!(common.output_format, OutputFormat::Json) && matches!(cv_sink, CvSinkConfig::Stdout)
    {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with --cv-stdout — use --log for machine-readable telemetry",
        ));
    }

    Ok(OnceArgs {
        pv,
        dt: common.dt,
        output_format: common.output_format,
        cv_sink,
        pid_config: common.pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
    })
}

fn parse_pipe(args: &[String]) -> Result<PipeArgs, CliError> {
    let common = parse_common_args(args, CommandKind::Pipe)?;

    if matches!(common.output_format, OutputFormat::Json) {
        return Err(CliError::config(
            "--format json writes to stdout, which conflicts with pipe's CV output — use --log for machine-readable telemetry",
        ));
    }

    Ok(PipeArgs {
        dt: common.dt,
        pid_config: common.pid_config,
        state_path: common.state_path,
        name: common.name,
        reset_accumulator: common.reset_accumulator,
    })
}

fn parse_common_args(args: &[String], command_kind: CommandKind) -> Result<CommonArgs, CliError> {
    let mut parsed = CommonArgs {
        pid_config: PidConfig {
            anti_windup: AntiWindupStrategy::BackCalculation,
            ..PidConfig::default()
        },
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

        match args[index].as_str() {
            "--cv-stdout" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "pipe always writes CV to stdout in v1 — move actuator side effects to the next shell stage",
                    ));
                }

                set_cv_sink(&mut parsed.cv_sink, CvSinkConfig::Stdout)?;
            }
            "--cv-file" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "pipe always writes CV to stdout in v1 — move actuator side effects to the next shell stage",
                    ));
                }

                let path = parse_path_flag("--cv-file", args, &mut index)?;
                set_cv_sink(&mut parsed.cv_sink, CvSinkConfig::File(path))?;
            }
            "--cv-cmd" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "pipe always writes CV to stdout in v1 — move actuator side effects to the next shell stage",
                    ));
                }

                return Err(CliError::config(
                    "--cv-cmd is not implemented yet in this slice",
                ));
            }
            "--pv-file" | "--pv-cmd" | "--pv-stdin" => {
                if matches!(command_kind, CommandKind::Pipe) {
                    return Err(CliError::config(
                        "pipe reads PV from stdin intrinsically — PV source flags are not accepted",
                    ));
                }

                return Err(CliError::config(format!(
                    "{} is not implemented yet in this slice",
                    args[index]
                )));
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
        "--pv" => {
            parsed.pv = Some(parse_f64_flag("--pv", args, index)?);
        }
        "--setpoint" => {
            parsed.pid_config.setpoint = parse_f64_flag("--setpoint", args, index)?;
        }
        "--kp" => {
            parsed.pid_config.kp = parse_f64_flag("--kp", args, index)?;
        }
        "--ki" => {
            parsed.pid_config.ki = parse_f64_flag("--ki", args, index)?;
        }
        "--kd" => {
            parsed.pid_config.kd = parse_f64_flag("--kd", args, index)?;
        }
        "--out-min" => {
            parsed.pid_config.out_min = parse_f64_flag("--out-min", args, index)?;
        }
        "--out-max" => {
            parsed.pid_config.out_max = parse_f64_flag("--out-max", args, index)?;
        }
        "--deadband" => {
            parsed.pid_config.deadband = parse_f64_flag("--deadband", args, index)?;
        }
        "--setpoint-ramp" => {
            parsed.pid_config.setpoint_ramp = Some(parse_f64_flag("--setpoint-ramp", args, index)?);
        }
        "--slew-rate" => {
            parsed.pid_config.slew_rate = Some(parse_f64_flag("--slew-rate", args, index)?);
        }
        "--pv-filter" => {
            parsed.pid_config.pv_filter_alpha = parse_f64_flag("--pv-filter", args, index)?;
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

#[derive(Clone, Debug, Default, PartialEq)]
struct CommonArgs {
    pv: Option<f64>,
    pid_config: PidConfig,
    output_format: OutputFormat,
    dt: f64,
    cv_sink: Option<CvSinkConfig>,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct OnceArgs {
    pv: f64,
    dt: f64,
    output_format: OutputFormat,
    cv_sink: CvSinkConfig,
    pid_config: PidConfig,
    state_path: Option<PathBuf>,
    name: Option<String>,
    reset_accumulator: bool,
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
