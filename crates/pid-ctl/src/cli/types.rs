use pid_ctl::app::{SessionConfig, StateStore};
use pid_ctl_core::PidConfig;
use std::path::PathBuf;
use std::time::Duration;

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
    pub(crate) anti_windup: Option<pid_ctl_core::AntiWindupStrategy>,
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
