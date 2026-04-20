use crate::cli::user_set::UserSet;
use pid_ctl::app::loop_runtime::LoopControls;
use pid_ctl::app::{SessionConfig, StateStore};
use pid_ctl_core::PidConfig;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) use pid_ctl::app::adapters_build::{CvSinkConfig, PvSourceConfig};

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
    pub(crate) min_dt: UserSet<f64>,
    pub(crate) max_dt: UserSet<f64>,
    pub(crate) dt_clamp: bool,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) pv_stdin_timeout: UserSet<Duration>,
    pub(crate) verify_cv: bool,
    pub(crate) state_write_interval: UserSet<Option<Duration>>,
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
            flush_interval: *self.state_write_interval.value(),
            state_fail_after: self.state_fail_after,
        }
    }
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

impl LoopControls for LoopArgs {
    fn interval(&self) -> Duration {
        self.interval
    }

    fn set_interval(&mut self, d: Duration) {
        self.interval = d;
    }

    fn max_dt(&self) -> f64 {
        *self.max_dt.value()
    }

    fn set_max_dt_unless_explicit(&mut self, v: f64) {
        self.max_dt.set_if_default(v);
    }

    fn pv_stdin_timeout(&self) -> Duration {
        *self.pv_stdin_timeout.value()
    }

    fn set_pv_stdin_timeout_unless_explicit(&mut self, d: Duration) {
        self.pv_stdin_timeout.set_if_default(d);
    }

    fn state_write_interval(&self) -> Option<Duration> {
        *self.state_write_interval.value()
    }

    fn set_state_write_interval_unless_explicit(&mut self, d: Option<Duration>) {
        self.state_write_interval.set_if_default(d);
    }
}
