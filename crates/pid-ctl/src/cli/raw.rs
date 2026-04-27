use super::parse::parse_duration_flag;
use super::types::{CvSinkConfig, LoopPvSource, OncePvSource, PidFlags};
use crate::CliError;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pid_ctl::autotune::TuningRule;
use pid_ctl_core::AntiWindupStrategy;
use std::path::PathBuf;
use std::time::Duration;

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
pub(super) enum OutputFormatArg {
    Text,
    Json,
}

// ---------------------------------------------------------------------------
// Tuning rule value enum for clap (autotune subcommand)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, ValueEnum)]
pub(super) enum TuningRuleArg {
    /// Ziegler–Nichols PI
    Pi,
    /// Ziegler–Nichols PID
    Pid,
    /// Tyreus–Luyben
    Tl,
}

impl From<TuningRuleArg> for TuningRule {
    fn from(a: TuningRuleArg) -> Self {
        match a {
            TuningRuleArg::Pi => Self::Pi,
            TuningRuleArg::Pid => Self::Pid,
            TuningRuleArg::Tl => Self::Tl,
        }
    }
}

impl From<OutputFormatArg> for super::types::OutputFormat {
    fn from(a: OutputFormatArg) -> Self {
        match a {
            OutputFormatArg::Text => super::types::OutputFormat::Text,
            OutputFormatArg::Json => super::types::OutputFormat::Json,
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
    /// Åström–Hägglund relay autotune: identify Ku/Tu and suggest PID gains
    Autotune(AutotuneRawArgs),
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
pub(super) struct PidRawArgs {
    /// Setpoint (target value)
    #[arg(long)]
    pub(super) setpoint: Option<f64>,

    /// Proportional gain
    #[arg(long)]
    pub(super) kp: Option<f64>,

    /// Integral gain
    #[arg(long)]
    pub(super) ki: Option<f64>,

    /// Derivative gain
    #[arg(long)]
    pub(super) kd: Option<f64>,

    /// Output minimum clamp
    #[arg(long)]
    pub(super) out_min: Option<f64>,

    /// Output maximum clamp
    #[arg(long)]
    pub(super) out_max: Option<f64>,

    /// Deadband (ignore errors smaller than this)
    #[arg(long)]
    pub(super) deadband: Option<f64>,

    /// Setpoint ramp rate (units/sec)
    #[arg(long)]
    pub(super) setpoint_ramp: Option<f64>,

    /// CV slew rate limiter (units/sec)
    #[arg(long, alias = "ramp-rate")]
    pub(super) slew_rate: Option<f64>,

    /// PV low-pass filter alpha (0.0–1.0)
    #[arg(long)]
    pub(super) pv_filter: Option<f64>,

    /// Anti-windup strategy (back-calc, back-calculation, clamp, none)
    #[arg(long)]
    pub(super) anti_windup: Option<String>,

    /// Back-calculation tracking time constant (seconds)
    #[arg(long)]
    pub(super) anti_windup_tt: Option<f64>,

    /// Feed-forward gain (scales the raw FF value; 0.0 = no FF)
    #[arg(long)]
    pub(super) ff_gain: Option<f64>,
}

impl PidRawArgs {
    pub(super) fn to_pid_flags(&self) -> Result<PidFlags, CliError> {
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
            feedforward_gain: self.ff_gain,
        })
    }
}

// ---------------------------------------------------------------------------
// Feed-forward source args (shared by once and loop)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
pub(super) struct FfRawArgs {
    /// Literal feed-forward value (once only)
    #[arg(long = "ff-value")]
    pub(super) value: Option<f64>,

    /// Read FF from this file each tick
    #[arg(long = "ff-from-file")]
    pub(super) from_file: Option<PathBuf>,

    /// Run this command to read FF each tick
    #[arg(long = "ff-cmd")]
    pub(super) cmd: Option<String>,
}

// ---------------------------------------------------------------------------
// Common args shared across once/loop/pipe
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct CommonRawArgs {
    /// Fixed time-step in seconds (default: wall-clock elapsed)
    #[arg(long)]
    pub(super) dt: Option<f64>,

    /// Clamp dt to [min-dt, max-dt] instead of skipping out-of-range ticks
    #[arg(long)]
    pub(super) dt_clamp: bool,

    /// Output format
    #[arg(long, value_enum, default_value = "text")]
    pub(super) format: OutputFormatArg,

    /// Path to state file for persistence
    #[arg(long)]
    pub(super) state: Option<PathBuf>,

    /// Controller instance name (stored in state file)
    #[arg(long)]
    pub(super) name: Option<String>,

    /// Reset integral accumulator on start
    #[arg(long)]
    pub(super) reset_accumulator: bool,

    /// Scale PV readings by this factor before processing
    #[arg(long)]
    pub(super) scale: Option<f64>,

    /// CV output decimal precision
    #[arg(long)]
    pub(super) cv_precision: Option<u32>,

    /// Default command timeout in seconds
    #[arg(long)]
    pub(super) cmd_timeout: Option<f64>,

    /// CV command timeout in seconds
    #[arg(long)]
    pub(super) cv_cmd_timeout: Option<f64>,

    /// PV command timeout in seconds
    #[arg(long)]
    pub(super) pv_cmd_timeout: Option<f64>,

    /// Safe CV value to emit on shutdown or PV failure
    #[arg(long)]
    pub(super) safe_cv: Option<f64>,

    /// Exit after this many consecutive CV write failures
    #[arg(long)]
    pub(super) cv_fail_after: Option<u32>,

    /// Exit after this many consecutive PV read failures
    #[arg(long)]
    pub(super) fail_after: Option<u32>,

    /// Minimum valid measured dt (seconds); ticks below this are skipped
    #[arg(long)]
    pub(super) min_dt: Option<f64>,

    /// Maximum valid measured dt (seconds); ticks above this are skipped
    #[arg(long)]
    pub(super) max_dt: Option<f64>,

    /// JSONL log file path
    #[arg(long)]
    pub(super) log: Option<PathBuf>,

    /// How often to flush state to disk (e.g. 500ms, 2s)
    #[arg(long, value_parser = parse_duration_value)]
    pub(super) state_write_interval: Option<Duration>,

    /// Max consecutive state write failures before escalating
    #[arg(long)]
    pub(super) state_fail_after: Option<u32>,

    /// PV units label (used in tune dashboard)
    #[arg(long)]
    pub(super) units: Option<String>,

    /// Suppress non-fatal warnings to stderr
    #[arg(long)]
    pub(super) quiet: bool,

    /// Print per-tick PV/CV/error to stderr
    #[arg(long)]
    pub(super) verbose: bool,
}

// ---------------------------------------------------------------------------
// CV sink args (once + loop; not pipe)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
pub(super) struct CvSinkRawArgs {
    /// Write CV to stdout
    #[arg(long)]
    pub(super) cv_stdout: bool,

    /// Write CV to this file
    #[arg(long)]
    pub(super) cv_file: Option<PathBuf>,

    /// Pipe CV to this shell command
    #[arg(long)]
    pub(super) cv_cmd: Option<String>,

    /// Verify CV was applied (re-read after write)
    #[arg(long)]
    pub(super) verify_cv: bool,
}

impl CvSinkRawArgs {
    /// Returns the CV sink config if any CV sink flag was set, and an error if multiple were set.
    pub(super) fn to_cv_sink_config(&self) -> Result<Option<CvSinkConfig>, CliError> {
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
pub(super) struct OncePvRawArgs {
    /// Literal PV value
    #[arg(long, num_args = 1, action = clap::ArgAction::Append)]
    pub(super) pv: Vec<f64>,

    /// Read PV from this file
    #[arg(long)]
    pub(super) pv_file: Option<PathBuf>,

    /// Run this command to read PV
    #[arg(long)]
    pub(super) pv_cmd: Option<String>,

    /// (loop-only) Read PV from stdin — not valid for once
    #[arg(long, hide = true)]
    pub(super) pv_stdin: bool,
}

impl OncePvRawArgs {
    pub(super) fn to_pv_source_config(&self) -> Result<Option<OncePvSource>, CliError> {
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
            Ok(Some(OncePvSource::Literal(v)))
        } else if let Some(ref path) = self.pv_file {
            Ok(Some(OncePvSource::File(path.clone())))
        } else {
            Ok(self
                .pv_cmd
                .as_ref()
                .map(|cmd| OncePvSource::Cmd(cmd.clone())))
        }
    }
}

// ---------------------------------------------------------------------------
// PV source args for loop (file/cmd/stdin; no literal)
// ---------------------------------------------------------------------------

#[derive(Args, Clone, Debug)]
pub(super) struct LoopPvRawArgs {
    /// (once-only) Literal PV value — not valid for loop
    #[arg(long, hide = true)]
    pub(super) pv: Option<f64>,

    /// Read PV from this file each tick
    #[arg(long)]
    pub(super) pv_file: Option<PathBuf>,

    /// Run this command to read PV each tick
    #[arg(long)]
    pub(super) pv_cmd: Option<String>,

    /// Read PV from stdin (one line per tick)
    #[arg(long)]
    pub(super) pv_stdin: bool,

    /// Timeout waiting for a PV stdin line (e.g. 500ms, 2s)
    #[arg(long, value_parser = parse_duration_value)]
    pub(super) pv_stdin_timeout: Option<Duration>,
}

impl LoopPvRawArgs {
    pub(super) fn to_pv_source_config(&self) -> Result<Option<LoopPvSource>, CliError> {
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
            Ok(Some(LoopPvSource::File(path.clone())))
        } else if let Some(ref cmd) = self.pv_cmd {
            Ok(Some(LoopPvSource::Cmd(cmd.clone())))
        } else if self.pv_stdin {
            Ok(Some(LoopPvSource::Stdin))
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
pub(super) struct PipeRejectedArgs {
    /// (not accepted by pipe) PV source flags
    #[arg(long, hide = true)]
    pub(super) pv: Option<f64>,
    #[arg(long, hide = true)]
    pub(super) pv_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(super) pv_cmd: Option<String>,
    #[arg(long, hide = true)]
    pub(super) pv_stdin: bool,
    /// (not accepted by pipe) CV sink flags
    #[arg(long, hide = true)]
    pub(super) cv_stdout: bool,
    #[arg(long, hide = true)]
    pub(super) cv_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(super) cv_cmd: Option<String>,
    #[arg(long, hide = true)]
    pub(super) verify_cv: bool,
    #[arg(long, hide = true)]
    pub(super) dry_run: bool,
    /// (not accepted by pipe) --tune flag
    #[arg(long, hide = true)]
    pub(super) tune: bool,
}

impl PipeRejectedArgs {
    pub(super) fn check(&self) -> Result<(), CliError> {
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
pub(super) struct OnceRejectedArgs {
    /// (loop-only) --tune flag — not valid for once
    #[arg(long, hide = true)]
    pub(super) tune: bool,
}

impl OnceRejectedArgs {
    pub(super) fn check(&self) -> Result<(), CliError> {
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
    pub(super) pid: PidRawArgs,

    #[command(flatten)]
    pub(super) common: CommonRawArgs,

    #[command(flatten)]
    pub(super) pv: OncePvRawArgs,

    #[command(flatten)]
    pub(super) ff: FfRawArgs,

    #[command(flatten)]
    pub(super) cv: CvSinkRawArgs,

    /// Suppress CV output (dry run)
    #[arg(long)]
    pub(super) dry_run: bool,

    #[command(flatten)]
    pub(super) rejected: OnceRejectedArgs,
}

/// `loop` — run a continuous PID control loop.
#[derive(Args, Clone, Debug)]
pub(crate) struct LoopRawArgs {
    #[command(flatten)]
    pub(super) pid: PidRawArgs,

    #[command(flatten)]
    pub(super) common: CommonRawArgs,

    #[command(flatten)]
    pub(super) pv: LoopPvRawArgs,

    #[command(flatten)]
    pub(super) ff: FfRawArgs,

    #[command(flatten)]
    pub(super) cv: CvSinkRawArgs,

    /// Tick interval (e.g. 500ms, 2s, 0.5)
    #[arg(long, value_parser = parse_duration_value)]
    pub(super) interval: Option<Duration>,

    /// Suppress CV output (dry run)
    #[arg(long)]
    pub(super) dry_run: bool,

    /// Launch interactive tuning dashboard (requires TTY)
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune: bool,

    /// Number of history samples shown in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune_history: Option<usize>,

    /// Step size for Kp in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune_step_kp: Option<f64>,

    /// Step size for Ki in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune_step_ki: Option<f64>,

    /// Step size for Kd in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune_step_kd: Option<f64>,

    /// Step size for setpoint in tune dashboard
    #[cfg(feature = "tui")]
    #[arg(long)]
    pub(super) tune_step_sp: Option<f64>,

    /// Unix socket path for remote control
    #[cfg(unix)]
    #[arg(long)]
    pub(super) socket: Option<PathBuf>,

    /// Unix permissions for socket file (octal, e.g. 0600)
    #[cfg(unix)]
    #[arg(long, value_parser = parse_octal_mode)]
    pub(super) socket_mode: Option<u32>,

    /// Exit cleanly after this many successful PID ticks (test hook; hidden).
    #[arg(long, hide = true)]
    pub(super) max_iterations: Option<u64>,
}

/// `pipe` — read PV from stdin, emit CV to stdout.
#[derive(Args, Clone, Debug)]
pub(crate) struct PipeRawArgs {
    #[command(flatten)]
    pub(super) pid: PidRawArgs,

    #[command(flatten)]
    pub(super) common: CommonRawArgs,

    #[command(flatten)]
    pub(super) rejected: PipeRejectedArgs,
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

/// `autotune` — Åström–Hägglund relay feedback autotune.
#[derive(Args, Clone, Debug)]
pub(crate) struct AutotuneRawArgs {
    /// Shell command to read PV (run via `sh -c`)
    #[arg(long, required = true)]
    pub(super) pv_cmd: String,

    /// Shell command to write CV (use `{cv}` placeholder; run via `sh -c`)
    #[arg(long, required = true)]
    pub(super) cv_cmd: String,

    /// CV operating point; relay toggles between bias±amp
    #[arg(long, required = true)]
    pub(super) bias: f64,

    /// Relay half-amplitude (must be > 0)
    #[arg(long, required = true)]
    pub(super) amp: f64,

    /// Test duration (e.g. 5m, 300s, 2.5m)
    #[arg(long, required = true, value_parser = parse_duration_value)]
    pub(super) duration: Duration,

    /// Tuning rule to apply to identified (Ku, Tu)
    #[arg(long, value_enum, default_value = "pid")]
    pub(super) rule: TuningRuleArg,

    /// Output minimum clamp
    #[arg(long, default_value = "0")]
    pub(super) out_min: f64,

    /// Output maximum clamp
    #[arg(long, default_value = "100")]
    pub(super) out_max: f64,

    /// Tick interval (e.g. 500ms, 1s)
    #[arg(long, value_parser = parse_duration_value, default_value = "1s")]
    pub(super) interval: Duration,

    /// Command timeout in seconds (PV and CV commands)
    #[arg(long)]
    pub(super) cmd_timeout: Option<f64>,

    /// CV output decimal precision
    #[arg(long, default_value = "2")]
    pub(super) cv_precision: u32,

    /// Write suggested gains to this state file
    #[arg(long)]
    pub(super) state: Option<PathBuf>,
}
