//! Controller session scaffolding and persistence primitives for the application layer.

pub mod state_store;

use crate::adapters::CvSink;
use pid_ctl_core::{ConfigError, PidConfig, PidController, StepInput, StepResult};
use serde::Serialize;
use std::error::Error;
use std::fmt;

pub use state_store::{
    STATE_SCHEMA_VERSION, StateLock, StateSnapshot, StateStore, StateStoreError,
};

/// Configuration for a controller session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionConfig {
    pub name: Option<String>,
    pub pid: PidConfig,
    pub state_store: Option<StateStore>,
    pub reset_accumulator: bool,
}

/// Orchestrates PID core state, persisted snapshots, and confirmed CV writes.
#[derive(Debug)]
pub struct ControllerSession {
    controller: PidController,
    snapshot: StateSnapshot,
    state_store: Option<StateStore>,
    _state_lock: Option<StateLock>,
}

impl ControllerSession {
    /// Creates a session, optionally restoring runtime continuity from a state
    /// snapshot while keeping PID math delegated to `pid-ctl-core`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionInitError`] when configuration validation, state loading,
    /// or lock acquisition fails.
    pub fn new(config: SessionConfig) -> Result<Self, SessionInitError> {
        let (snapshot, state_store, state_lock) = if let Some(store) = config.state_store {
            let state_lock = store.acquire_lock()?;
            let snapshot = store.load()?.unwrap_or_default();
            (snapshot, Some(store), Some(state_lock))
        } else {
            (StateSnapshot::default(), None, None)
        };

        let mut controller = PidController::new(config.pid)?;
        controller.restore_state(&snapshot.runtime_state());

        if config.reset_accumulator {
            controller.reset_integral();
        }

        let mut snapshot = snapshot;
        snapshot.name = Some(resolve_controller_name(
            config.name,
            state_store.as_ref(),
            snapshot.name.clone(),
        ));

        Ok(Self {
            controller,
            snapshot,
            state_store,
            _state_lock: state_lock,
        })
    }

    #[must_use]
    pub fn config(&self) -> &PidConfig {
        self.controller.config()
    }

    /// Processes a single PV value, writes the resulting CV to the provided
    /// sink, and persists the updated snapshot when configured.
    ///
    /// # Errors
    ///
    /// Returns [`TickError`] when the CV sink write fails. State persistence
    /// failures are surfaced on the success path so callers can apply
    /// mode-specific policies.
    pub fn process_pv(
        &mut self,
        pv: f64,
        dt: f64,
        cv_sink: &mut dyn CvSink,
    ) -> Result<TickOutcome, TickError> {
        let prev_applied_cv = self.snapshot.last_cv.unwrap_or(0.0);
        let step = self.controller.step(StepInput {
            pv,
            dt,
            prev_applied_cv,
        });

        if let Err(source) = cv_sink.write_cv(step.cv) {
            self.snapshot = self.build_snapshot(&step, prev_applied_cv);
            let _ = self.persist_snapshot();
            return Err(TickError::CvWrite { source });
        }

        self.snapshot = self.build_snapshot(&step, step.cv);
        let state_write_failed = self.persist_snapshot().err();

        Ok(TickOutcome {
            record: IterationRecord::new(
                self.snapshot.name.clone(),
                self.snapshot.iter,
                pv,
                dt,
                self.controller.config(),
                &step,
                self.controller.last_error().unwrap_or(0.0),
            ),
            state_write_failed,
        })
    }

    fn build_snapshot(&self, step: &StepResult, confirmed_applied_cv: f64) -> StateSnapshot {
        StateSnapshot {
            schema_version: STATE_SCHEMA_VERSION,
            name: self.snapshot.name.clone(),
            kp: Some(self.controller.config().kp),
            ki: Some(self.controller.config().ki),
            kd: Some(self.controller.config().kd),
            setpoint: Some(self.controller.config().setpoint),
            out_min: finite_value(self.controller.config().out_min),
            out_max: finite_value(self.controller.config().out_max),
            effective_sp: self
                .controller
                .config()
                .setpoint_ramp
                .map(|_| step.effective_sp),
            target_sp: self
                .controller
                .config()
                .setpoint_ramp
                .map(|_| self.controller.config().setpoint),
            i_acc: step.i_acc,
            last_error: self.controller.last_error(),
            last_cv: Some(confirmed_applied_cv),
            last_pv: self.controller.last_pv(),
            iter: self.snapshot.iter.saturating_add(1),
            created_at: self
                .snapshot
                .created_at
                .clone()
                .or_else(|| Some(now_iso8601())),
            updated_at: Some(now_iso8601()),
        }
    }

    fn persist_snapshot(&self) -> Result<(), StateStoreError> {
        if let Some(state_store) = &self.state_store {
            state_store.save(&self.snapshot)?;
        }

        Ok(())
    }
}

/// Successful tick output plus any non-fatal state persistence issue.
#[derive(Debug)]
pub struct TickOutcome {
    pub record: IterationRecord,
    pub state_write_failed: Option<StateStoreError>,
}

/// Stable iteration record for JSON output.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IterationRecord {
    pub schema_version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub iter: u64,
    pub pv: f64,
    pub sp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_sp: Option<f64>,
    pub err: f64,
    pub p: f64,
    pub i: f64,
    pub d: f64,
    pub cv: f64,
    pub i_acc: f64,
    pub dt: f64,
}

impl IterationRecord {
    fn new(
        name: Option<String>,
        iter: u64,
        pv: f64,
        dt: f64,
        config: &PidConfig,
        step: &StepResult,
        error: f64,
    ) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            name,
            iter,
            pv,
            sp: config.setpoint,
            effective_sp: config.setpoint_ramp.map(|_| step.effective_sp),
            err: error,
            p: step.p_term,
            i: step.i_term,
            d: step.d_term,
            cv: step.cv,
            i_acc: step.i_acc,
            dt,
        }
    }
}

/// Session creation failures.
#[derive(Debug)]
pub enum SessionInitError {
    Config(ConfigError),
    State(StateStoreError),
}

impl fmt::Display for SessionInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(source) => write!(f, "{source}"),
            Self::State(source) => write!(f, "{source}"),
        }
    }
}

impl Error for SessionInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(source) => Some(source),
            Self::State(source) => Some(source),
        }
    }
}

impl From<ConfigError> for SessionInitError {
    fn from(source: ConfigError) -> Self {
        Self::Config(source)
    }
}

impl From<StateStoreError> for SessionInitError {
    fn from(source: StateStoreError) -> Self {
        Self::State(source)
    }
}

/// Tick execution failures.
#[derive(Debug)]
pub enum TickError {
    CvWrite { source: std::io::Error },
}

impl fmt::Display for TickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CvWrite { source } => write!(f, "CV write failed: {source}"),
        }
    }
}

impl Error for TickError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CvWrite { source } => Some(source),
        }
    }
}

fn finite_value(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day, hour, min, sec) = timestamp_to_utc(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Converts Unix seconds to (year, month, day, hour, min, sec) in UTC.
fn timestamp_to_utc(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = secs % 60;
    let mins = secs / 60;
    let min = mins % 60;
    let hours = mins / 60;
    let hour = hours % 24;
    let days = hours / 24;

    let mut year = 1970u64;
    let mut remaining_days = days;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let month_days: [u64; 12] = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &days_in_month in &month_days {
        if remaining_days < days_in_month {
            break;
        }
        remaining_days -= days_in_month;
        month += 1;
    }
    let day = remaining_days + 1;

    (year, month, day, hour, min, sec)
}

fn is_leap_year(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn resolve_controller_name(
    cli_name: Option<String>,
    state_store: Option<&StateStore>,
    snapshot_name: Option<String>,
) -> String {
    if let Some(name) = cli_name {
        return name;
    }

    if let Some(name) = snapshot_name {
        return name;
    }

    if let Some(state_store) = state_store
        && let Some(stem) = state_store
            .path()
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
    {
        return stem.to_owned();
    }

    String::from("pid-ctl")
}
