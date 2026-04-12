use pid_ctl::adapters::{CmdPvSource, FilePvSource, PvSource};
use pid_ctl::app::{SessionConfig, StateStore};
use pid_ctl_core::{AntiWindupStrategy, PidConfig};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::CliError;

// ---------------------------------------------------------------------------
// Enums / structs (unchanged public API consumed by main.rs)
// ---------------------------------------------------------------------------

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
    #[cfg(unix)]
    pub(crate) socket_path: Option<PathBuf>,
    #[cfg(unix)]
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

#[cfg(unix)]
pub(crate) struct SetArgs {
    pub(crate) socket_path: PathBuf,
    pub(crate) param: String,
    pub(crate) value: f64,
}

pub(crate) struct StatusFlags {
    pub(crate) state_path: Option<PathBuf>,
    #[cfg(unix)]
    pub(crate) socket_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Value parsers for clap
// ---------------------------------------------------------------------------

/// Clap `value_parser` for durations: `"500ms"`, `"2s"`, `"0.5"` (bare number = seconds).
fn parse_duration_value(s: &str) -> Result<Duration, String> {
    parse_duration_flag("--duration", s).map_err(|e| e.message)
}

/// Clap `value_parser` for `--socket-mode` (octal string like `0600`).
fn parse_octal_mode(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s, 8)
        .map_err(|_| format!("expected an octal value like 0600 or 0660, got `{s}`"))
}

// ---------------------------------------------------------------------------
// Output format value enum for clap
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormatArg {
    Text,
    Json,
}

impl From<OutputFormatArg> for OutputFormat {
    fn from(a: OutputFormatArg) -> Self {
        match a {
            OutputFormatArg::Text => OutputFormat::Text,
            OutputFormatArg::Json => OutputFormat::Json,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level clap CLI (used by main.rs)
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "pid-ctl",
    about = "PID loop controller",
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: SubCommand,
}

#[derive(Subcommand)]
pub(crate) enum SubCommand {
    /// Run one PID tick and exit
    Once(OnceRawArgs),
    /// Run a continuous PID control loop
    Loop(LoopRawArgs),
    /// Read PV lines from stdin and emit CV to stdout
    Pipe(PipeRawArgs),
    /// Show controller status
    Status(StatusRawArgs),
    /// Send a set command via socket
    #[cfg(unix)]
    Set(SetRawArgs),
    /// Send a hold command via socket
    #[cfg(unix)]
    Hold(SocketOnlyArgs),
    /// Send a resume command via socket
    #[cfg(unix)]
    Resume(SocketOnlyArgs),
    /// Send a reset command via socket
    #[cfg(unix)]
    Reset(SocketOnlyArgs),
    /// Send a save command via socket
    #[cfg(unix)]
    Save(SocketOnlyArgs),
    /// Purge persisted state file
    Purge(StateOnlyArgs),
    /// Initialise a new state file
    Init(StateOnlyArgs),
}

// ---------------------------------------------------------------------------
// Shared PID flag args (reused by Once, Loop, Pipe)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
struct PidRawArgs {
    /// Setpoint (target value)
    #[arg(long)]
    setpoint: Option<f64>,

    /// Proportional gain
    #[arg(long)]
    kp: Option<f64>,

    /// Integral gain
    #[arg(long)]
    ki: Option<f64>,

    /// Derivative gain
    #[arg(long)]
    kd: Option<f64>,

    /// Output minimum clamp
    #[arg(long)]
    out_min: Option<f64>,

    /// Output maximum clamp
    #[arg(long)]
    out_max: Option<f64>,

    /// Deadband (ignore errors smaller than this)
    #[arg(long)]
    deadband: Option<f64>,

    /// Setpoint ramp rate (units/sec)
    #[arg(long)]
    setpoint_ramp: Option<f64>,

    /// CV slew rate limiter (units/sec)
    #[arg(long, alias = "ramp-rate")]
    slew_rate: Option<f64>,

    /// PV low-pass filter alpha (0.0–1.0)
    #[arg(long)]
    pv_filter: Option<f64>,

    /// Anti-windup strategy (back-calc, back-calculation, clamp, none)
    #[arg(long)]
    anti_windup: Option<String>,

    /// Back-calculation tracking time constant (seconds)
    #[arg(long)]
    anti_windup_tt: Option<f64>,
}

impl PidRawArgs {
    fn to_pid_flags(&self) -> Result<PidFlags, CliError> {
        let anti_windup = self
            .anti_windup
            .as_deref()
            .map(|v| match v {
                "back-calc" | "back-calculation" => Ok(AntiWindupStrategy::BackCalculation),
                "clamp" => Ok(AntiWindupStrategy::Clamp),
                "none" => Ok(AntiWindupStrategy::None),
                other => Err(CliError::config(format!(
                    "--anti-windup must be `back-calc`, `clamp`, or `none`, got `{other}`"
                ))),
            })
            .transpose()?;
        Ok(PidFlags {
            setpoint: self.setpoint,
            kp: self.kp,
            ki: self.ki,
            kd: self.kd,
            out_min: self.out_min,
            out_max: self.out_max,
            deadband: self.deadband,
            setpoint_ramp: self.setpoint_ramp,
            slew_rate: self.slew_rate,
            pv_filter_alpha: self.pv_filter,
            anti_windup,
            anti_windup_tt: self.anti_windup_tt,
        })
    }
}

// ---------------------------------------------------------------------------
// Common args shared across once/loop/pipe
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct CommonRawArgs {
    /// Fixed time-step in seconds (default: wall-clock elapsed)
    #[arg(long)]
    dt: Option<f64>,

    /// Clamp dt to [min-dt, max-dt] instead of skipping out-of-range ticks
    #[arg(long)]
    dt_clamp: bool,

    /// Output format
    #[arg(long, value_enum, default_value = "text")]
    format: OutputFormatArg,

    /// Path to state file for persistence
    #[arg(long)]
    state: Option<PathBuf>,

    /// Controller instance name (stored in state file)
    #[arg(long)]
    name: Option<String>,

    /// Reset integral accumulator on start
    #[arg(long)]
    reset_accumulator: bool,

    /// Scale PV readings by this factor before processing
    #[arg(long)]
    scale: Option<f64>,

    /// CV output decimal precision
    #[arg(long)]
    cv_precision: Option<u32>,

    /// Default command timeout in seconds
    #[arg(long)]
    cmd_timeout: Option<f64>,

    /// CV command timeout in seconds
    #[arg(long)]
    cv_cmd_timeout: Option<f64>,

    /// PV command timeout in seconds
    #[arg(long)]
    pv_cmd_timeout: Option<f64>,

    /// Safe CV value to emit on shutdown or PV failure
    #[arg(long)]
    safe_cv: Option<f64>,

    /// Exit after this many consecutive CV write failures
    #[arg(long)]
    cv_fail_after: Option<u32>,

    /// Exit after this many consecutive PV read failures
    #[arg(long)]
    fail_after: Option<u32>,

    /// Minimum valid measured dt (seconds); ticks below this are skipped
    #[arg(long)]
    min_dt: Option<f64>,

    /// Maximum valid measured dt (seconds); ticks above this are skipped
    #[arg(long)]
    max_dt: Option<f64>,

    /// JSONL log file path
    #[arg(long)]
    log: Option<PathBuf>,

    /// How often to flush state to disk (e.g. 500ms, 2s)
    #[arg(long, value_parser = parse_duration_value)]
    state_write_interval: Option<Duration>,

    /// Max consecutive state write failures before escalating
    #[arg(long)]
    state_fail_after: Option<u32>,

    /// PV units label (used in tune dashboard)
    #[arg(long)]
    units: Option<String>,

    /// Suppress non-fatal warnings to stderr
    #[arg(long)]
    quiet: bool,

    /// Print per-tick PV/CV/error to stderr
    #[arg(long)]
    verbose: bool,
}

// ---------------------------------------------------------------------------
// CV sink args (once + loop; not pipe)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
struct CvSinkRawArgs {
    /// Write CV to stdout
    #[arg(long)]
    cv_stdout: bool,

    /// Write CV to this file
    #[arg(long)]
    cv_file: Option<PathBuf>,

    /// Pipe CV to this shell command
    #[arg(long)]
    cv_cmd: Option<String>,

    /// Verify CV was applied (re-read after write)
    #[arg(long)]
    verify_cv: bool,
}

impl CvSinkRawArgs {
    /// Returns the CV sink config if any CV sink flag was set, and an error if multiple were set.
    fn to_cv_sink_config(&self) -> Result<Option<CvSinkConfig>, CliError> {
        let mut count = 0u32;
        if self.cv_stdout {
            count += 1;
        }
        if self.cv_file.is_some() {
            count += 1;
        }
        if self.cv_cmd.is_some() {
            count += 1;
        }

        if count > 1 {
            return Err(CliError::config("only one CV sink may be specified"));
        }

        if self.cv_stdout {
            Ok(Some(CvSinkConfig::Stdout))
        } else if let Some(ref path) = self.cv_file {
            Ok(Some(CvSinkConfig::File {
                path: path.clone(),
                verify: false,
            }))
        } else {
            Ok(self.cv_cmd.as_ref().map(|cmd| CvSinkConfig::Cmd {
                command: cmd.clone(),
                timeout: None,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// PV source args for once (literal/file/cmd; no stdin)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
struct OncePvRawArgs {
    /// Literal PV value
    #[arg(long, num_args = 1, action = clap::ArgAction::Append)]
    pv: Vec<f64>,

    /// Read PV from this file
    #[arg(long)]
    pv_file: Option<PathBuf>,

    /// Run this command to read PV
    #[arg(long)]
    pv_cmd: Option<String>,

    /// (loop-only) Read PV from stdin — not valid for once
    #[arg(long, hide = true)]
    pv_stdin: bool,
}

impl OncePvRawArgs {
    fn to_pv_source_config(&self) -> Result<Option<PvSourceConfig>, CliError> {
        // Reject flags invalid for once.
        if self.pv_stdin {
            return Err(CliError::config(
                "--pv-stdin is only supported with loop — use pipe for externally-timed stdin reads",
            ));
        }

        let mut count = 0u32;
        if !self.pv.is_empty() {
            count += 1;
        }
        if self.pv_file.is_some() {
            count += 1;
        }
        if self.pv_cmd.is_some() {
            count += 1;
        }

        // Multiple --pv values or multiple source types both mean "only one PV source".
        if self.pv.len() > 1 || count > 1 {
            return Err(CliError::config("only one PV source may be specified"));
        }

        if let Some(&v) = self.pv.first() {
            Ok(Some(PvSourceConfig::Literal(v)))
        } else if let Some(ref path) = self.pv_file {
            Ok(Some(PvSourceConfig::File(path.clone())))
        } else {
            Ok(self
                .pv_cmd
                .as_ref()
                .map(|cmd| PvSourceConfig::Cmd(cmd.clone())))
        }
    }
}

// ---------------------------------------------------------------------------
// PV source args for loop (file/cmd/stdin; no literal)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
struct LoopPvRawArgs {
    /// (once-only) Literal PV value — not valid for loop
    #[arg(long, hide = true)]
    pv: Option<f64>,

    /// Read PV from this file each tick
    #[arg(long)]
    pv_file: Option<PathBuf>,

    /// Run this command to read PV each tick
    #[arg(long)]
    pv_cmd: Option<String>,

    /// Read PV from stdin (one line per tick)
    #[arg(long)]
    pv_stdin: bool,

    /// Timeout waiting for a PV stdin line (e.g. 500ms, 2s)
    #[arg(long, value_parser = parse_duration_value)]
    pv_stdin_timeout: Option<Duration>,
}

impl LoopPvRawArgs {
    fn to_pv_source_config(&self) -> Result<Option<PvSourceConfig>, CliError> {
        // loop does not accept --pv <literal>
        if self.pv.is_some() {
            return Err(CliError::config(
                "loop requires --pv-file or --pv-cmd for PV source — use once for literal PV values",
            ));
        }

        let mut count = 0u32;
        if self.pv_file.is_some() {
            count += 1;
        }
        if self.pv_cmd.is_some() {
            count += 1;
        }
        if self.pv_stdin {
            count += 1;
        }

        if count > 1 {
            return Err(CliError::config("only one PV source may be specified"));
        }

        if let Some(ref path) = self.pv_file {
            Ok(Some(PvSourceConfig::File(path.clone())))
        } else if let Some(ref cmd) = self.pv_cmd {
            Ok(Some(PvSourceConfig::Cmd(cmd.clone())))
        } else if self.pv_stdin {
            Ok(Some(PvSourceConfig::Stdin))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Pipe-rejected args (for giving plan-documented error messages)
// ---------------------------------------------------------------------------

/// Flags that pipe does not accept (hidden for `--help`; validated in `parse_pipe`).
#[derive(Args, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct PipeRejectedArgs {
    /// (not accepted by pipe) PV source flags
    #[arg(long, hide = true)]
    pv: Option<f64>,
    #[arg(long, hide = true)]
    pv_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    pv_cmd: Option<String>,
    #[arg(long, hide = true)]
    pv_stdin: bool,
    /// (not accepted by pipe) CV sink flags
    #[arg(long, hide = true)]
    cv_stdout: bool,
    #[arg(long, hide = true)]
    cv_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    cv_cmd: Option<String>,
    #[arg(long, hide = true)]
    verify_cv: bool,
    #[arg(long, hide = true)]
    dry_run: bool,
    /// (not accepted by pipe) --tune flag
    #[arg(long, hide = true)]
    tune: bool,
}

impl PipeRejectedArgs {
    fn check(&self) -> Result<(), CliError> {
        let pv_err = || {
            CliError::config(
                "pipe reads PV from stdin intrinsically — PV source flags are not accepted",
            )
        };
        let cv_err = || {
            CliError::config(
                "pipe always writes CV to stdout in v1 — move actuator side effects to the next shell stage",
            )
        };

        if self.pv.is_some() || self.pv_file.is_some() || self.pv_cmd.is_some() || self.pv_stdin {
            return Err(pv_err());
        }
        if self.cv_stdout || self.cv_file.is_some() || self.cv_cmd.is_some() || self.verify_cv {
            return Err(cv_err());
        }
        if self.dry_run {
            return Err(CliError::config(
                "--dry-run is not meaningful with pipe — pipe has no side effects to suppress",
            ));
        }
        if self.tune {
            return Err(CliError::config(
                "--tune is unavailable with pipe — pipe is a pure stdin→stdout transformer in v1",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Once-rejected args (for giving plan-documented error messages)
// ---------------------------------------------------------------------------

/// Flags that once does not accept (hidden for `--help`; validated in `parse_once`).
#[derive(Args, Clone, Debug)]
struct OnceRejectedArgs {
    /// (loop-only) --tune flag — not valid for once
    #[arg(long, hide = true)]
    tune: bool,
}

impl OnceRejectedArgs {
    fn check(&self) -> Result<(), CliError> {
        if self.tune {
            return Err(CliError::config("--tune requires loop"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Raw clap structs for each subcommand
// ---------------------------------------------------------------------------

/// `once` — run one PID tick and exit.
#[derive(Args, Clone, Debug)]
pub(crate) struct OnceRawArgs {
    #[command(flatten)]
    pid: PidRawArgs,

    #[command(flatten)]
    common: CommonRawArgs,

    #[command(flatten)]
    pv: OncePvRawArgs,

    #[command(flatten)]
    cv: CvSinkRawArgs,

    /// Suppress CV output (dry run)
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    rejected: OnceRejectedArgs,
}

/// `loop` — run a continuous PID control loop.
#[derive(Args, Clone, Debug)]
pub(crate) struct LoopRawArgs {
    #[command(flatten)]
    pid: PidRawArgs,

    #[command(flatten)]
    common: CommonRawArgs,

    #[command(flatten)]
    pv: LoopPvRawArgs,

    #[command(flatten)]
    cv: CvSinkRawArgs,

    /// Tick interval (e.g. 500ms, 2s, 0.5)
    #[arg(long, value_parser = parse_duration_value)]
    interval: Option<Duration>,

    /// Suppress CV output (dry run)
    #[arg(long)]
    dry_run: bool,

    /// Launch interactive tuning dashboard (requires TTY)
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune: bool,

    /// Number of history samples shown in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune_history: Option<usize>,

    /// Step size for Kp in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune_step_kp: Option<f64>,

    /// Step size for Ki in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune_step_ki: Option<f64>,

    /// Step size for Kd in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune_step_kd: Option<f64>,

    /// Step size for setpoint in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    tune_step_sp: Option<f64>,

    /// Unix socket path for remote control
    #[cfg(unix)]
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Unix permissions for socket file (octal, e.g. 0600)
    #[cfg(unix)]
    #[arg(long, value_parser = parse_octal_mode)]
    socket_mode: Option<u32>,
}

/// `pipe` — read PV from stdin, emit CV to stdout.
#[derive(Args, Clone, Debug)]
pub(crate) struct PipeRawArgs {
    #[command(flatten)]
    pid: PidRawArgs,

    #[command(flatten)]
    common: CommonRawArgs,

    #[command(flatten)]
    rejected: PipeRejectedArgs,
}

/// `status` — show controller status.
#[derive(Args, Clone, Debug)]
pub(crate) struct StatusRawArgs {
    /// Path to state file
    #[arg(long)]
    pub(crate) state: Option<PathBuf>,

    /// Unix socket path to query
    #[cfg(unix)]
    #[arg(long)]
    pub(crate) socket: Option<PathBuf>,
}

/// `set` — send a set command via socket (unix only).
#[cfg(unix)]
#[derive(Args, Clone, Debug)]
pub(crate) struct SetRawArgs {
    /// Unix socket path
    #[arg(long, required = true)]
    pub(crate) socket: PathBuf,

    /// Parameter name (kp, ki, kd, sp, interval)
    #[arg(long, required = true)]
    pub(crate) param: String,

    /// New value
    #[arg(long, required = true)]
    pub(crate) value: f64,
}

/// `hold`/`resume`/`reset`/`save` — socket-only commands (unix only).
#[cfg(unix)]
#[derive(Args, Clone, Debug)]
pub(crate) struct SocketOnlyArgs {
    /// Unix socket path
    #[arg(long, required = true)]
    pub(crate) socket: PathBuf,
}

/// `purge`/`init` — state-file-only commands.
#[derive(Args, Clone, Debug)]
pub(crate) struct StateOnlyArgs {
    /// Path to state file
    #[arg(long)]
    pub(crate) state: Option<PathBuf>,
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

    let pv_stdin_timeout = raw.pv.pv_stdin_timeout.unwrap_or(interval);
    let explicit_pv_stdin_timeout = raw.pv.pv_stdin_timeout.is_some();

    // Default cmd-timeout: min(interval, 30s).
    let effective_cmd_timeout = raw.common.cmd_timeout.map_or_else(
        || interval.min(Duration::from_secs(30)),
        Duration::from_secs_f64,
    );
    let pv_cmd_timeout = raw
        .common
        .pv_cmd_timeout
        .map_or(effective_cmd_timeout, Duration::from_secs_f64);

    // explicit_* track whether user explicitly set these so runtime interval changes don't override.
    let explicit_min_dt = raw.common.min_dt.is_some();
    let explicit_max_dt = raw.common.max_dt.is_some();
    let explicit_state_write_interval = raw.common.state_write_interval.is_some();

    // Default state_write_interval for loop: max(tick_interval, 100ms).
    let min_flush = Duration::from_millis(100);
    let default_state_write_interval = interval.max(min_flush);
    let state_write_interval = Some(
        raw.common
            .state_write_interval
            .unwrap_or(default_state_write_interval),
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
        min_dt: raw.common.min_dt.unwrap_or(0.01),
        max_dt: raw.common.max_dt.unwrap_or(max_dt_default),
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
        explicit_max_dt,
        explicit_min_dt,
        explicit_pv_stdin_timeout,
        explicit_state_write_interval,
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
pub(crate) fn parse_set_args(raw: &SetRawArgs) -> SetArgs {
    SetArgs {
        socket_path: raw.socket.clone(),
        param: raw.param.clone(),
        value: raw.value,
    }
}

#[cfg(unix)]
pub(crate) fn get_socket_path(raw: &SocketOnlyArgs) -> PathBuf {
    raw.socket.clone()
}

pub(crate) fn get_state_path(raw: &StateOnlyArgs) -> Result<PathBuf, CliError> {
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
