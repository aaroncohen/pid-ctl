//! Controller session scaffolding and persistence primitives for the application layer.

pub mod adapters_build;
pub mod logger;
pub mod loop_runtime;
pub mod snapshot_persister;
#[cfg(unix)]
pub mod socket_dispatch;
pub mod state_store;
pub mod ticker;

use crate::adapters::CvSink;
use pid_ctl_core::{ConfigError, PidConfig, PidController, StepInput, StepResult};
use serde::Serialize;
use std::error::Error;
use std::fmt;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;

pub use snapshot_persister::SnapshotPersister;
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
    /// Maximum frequency of state snapshot flushes to disk.
    ///
    /// `None` means write on every tick (used for `once`).
    /// For `loop` the default is `max(tick_interval, 100ms)`.
    /// For `pipe` the default is `1s`.
    pub flush_interval: Option<Duration>,
    /// Number of consecutive state write failures before escalating to a
    /// prominent per-cycle stderr warning. Default: 10.
    pub state_fail_after: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            name: None,
            pid: PidConfig::default(),
            state_store: None,
            reset_accumulator: false,
            flush_interval: None,
            state_fail_after: 10,
        }
    }
}

/// Output of a hold tick: actuator held at `held_cv`, PV read for display only (no PID step).
#[derive(Debug)]
pub struct HoldTickOutcome {
    pub pv: f64,
    pub state_write_failed: Option<StateStoreError>,
}

/// Orchestrates PID core state, persisted snapshots, and confirmed CV writes.
#[derive(Debug)]
pub struct ControllerSession {
    controller: PidController,
    snapshot: StateSnapshot,
    persister: SnapshotPersister,
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

        let persister = SnapshotPersister::new(
            state_store,
            state_lock,
            config.flush_interval,
            config.state_fail_after,
        );

        Ok(Self {
            controller,
            snapshot,
            persister,
        })
    }

    #[must_use]
    pub const fn config(&self) -> &PidConfig {
        self.controller.config()
    }

    /// Last CV confirmed applied to the actuator (from the persisted snapshot).
    #[must_use]
    pub const fn last_applied_cv(&self) -> Option<f64> {
        self.snapshot.last_cv
    }

    /// Current iteration count from the snapshot.
    #[must_use]
    #[allow(clippy::iter_not_returning_iterator)]
    pub const fn iter(&self) -> u64 {
        self.snapshot.iter
    }

    /// Last filtered PV value used in PID computation.
    #[must_use]
    pub const fn last_pv(&self) -> Option<f64> {
        self.snapshot.last_pv
    }

    /// Last computed error (setpoint - PV).
    #[must_use]
    pub const fn last_error(&self) -> Option<f64> {
        self.snapshot.last_error
    }

    /// Current integral accumulator value.
    #[must_use]
    pub const fn i_acc(&self) -> f64 {
        self.snapshot.i_acc
    }

    /// Updates Kp/Ki/Kd in the live controller and mirrors into the in-memory snapshot for persistence.
    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) {
        self.controller.set_gains(kp, ki, kd);
        self.sync_pid_fields_from_controller();
    }

    /// Updates the target setpoint in the live controller and mirrors into the snapshot.
    pub fn set_setpoint(&mut self, setpoint: f64) {
        self.controller.set_setpoint(setpoint);
        self.sync_pid_fields_from_controller();
    }

    /// Clears the integral accumulator and marks the next tick for D-term protection (`post_reset`).
    pub fn reset_integral(&mut self) {
        self.controller.reset_integral();
        self.snapshot.i_acc = 0.0;
    }

    /// Updates coalesced state flush cadence (used when `--interval` changes at runtime).
    pub fn set_flush_interval(&mut self, flush_interval: Option<Duration>) {
        self.persister.set_flush_interval(flush_interval);
    }

    fn sync_pid_fields_from_controller(&mut self) {
        let c = self.controller.config();
        self.snapshot.kp = Some(c.kp);
        self.snapshot.ki = Some(c.ki);
        self.snapshot.kd = Some(c.kd);
        self.snapshot.setpoint = Some(c.setpoint);
        self.snapshot.out_min = finite_value(c.out_min);
        self.snapshot.out_max = finite_value(c.out_max);
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
            d_term_skipped: step.d_term_skipped,
            state_write_failed,
        })
    }

    /// Marks a skipped tick (anomalous `dt`): derivative is unreliable on the next successful step.
    /// Updates `updated_at` and persists when `--state` is set; does **not** advance `iter`.
    pub fn on_dt_skipped(&mut self) -> Option<StateStoreError> {
        self.controller.mark_dt_skipped();
        self.snapshot.updated_at = Some(now_iso8601());
        self.persist_snapshot().err()
    }

    /// Elapsed wall-clock seconds since `snapshot.updated_at` (RFC 3339 UTC), if present and parseable.
    #[must_use]
    pub fn wall_clock_dt_since_state_update(&self) -> Option<f64> {
        let ts = self.snapshot.updated_at.as_ref()?;
        let past = time::OffsetDateTime::parse(ts, &Rfc3339).ok()?;
        let now = time::OffsetDateTime::now_utc();
        let delta = now - past;
        Some(delta.as_seconds_f64().max(0.0))
    }

    /// Records a CV value confirmed written to the actuator without advancing `iter`
    /// (e.g. `--safe-cv` after PV loss).
    pub fn record_confirmed_cv(&mut self, cv: f64) -> Option<StateStoreError> {
        self.snapshot.last_cv = Some(cv);
        self.snapshot.updated_at = Some(now_iso8601());
        self.persist_snapshot().err()
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

    fn persist_snapshot(&mut self) -> Result<(), StateStoreError> {
        self.persister.persist(&self.snapshot)
    }

    /// Forces a disk flush regardless of `flush_interval`.
    ///
    /// Used at shutdown to ensure the final in-memory state is persisted.
    pub fn force_flush(&mut self) -> Option<StateStoreError> {
        self.persister.force_flush(&self.snapshot)
    }

    /// Returns `true` when the number of consecutive state write failures has
    /// reached the escalation threshold (`--state-fail-after`).
    #[must_use]
    pub fn state_fail_escalated(&self) -> bool {
        self.persister.fail_escalated()
    }

    /// Returns the current consecutive state write failure count.
    #[must_use]
    pub fn state_fail_count(&self) -> u32 {
        self.persister.fail_count()
    }

    /// Returns `true` if this session was created with a `--state` path (i.e. persistence is active).
    #[must_use]
    pub fn has_state_store(&self) -> bool {
        self.persister.has_store()
    }

    /// Holds the last CV at the actuator without advancing PID state or `iter` (operator hold).
    ///
    /// # Errors
    ///
    /// Returns [`TickError::CvWrite`] when the CV sink rejects the write.
    pub fn hold_tick_write(
        &mut self,
        scaled_pv: f64,
        held_cv: f64,
        cv_sink: &mut dyn CvSink,
    ) -> Result<HoldTickOutcome, TickError> {
        cv_sink
            .write_cv(held_cv)
            .map_err(|source| TickError::CvWrite { source })?;
        self.snapshot.updated_at = Some(now_iso8601());
        let state_write_failed = self.persist_snapshot().err();
        Ok(HoldTickOutcome {
            pv: scaled_pv,
            state_write_failed,
        })
    }
}

/// Successful tick output plus any non-fatal state persistence issue.
#[derive(Debug)]
pub struct TickOutcome {
    pub record: IterationRecord,
    /// Set when the D term was zeroed on this tick due to an unreliable
    /// derivative condition (first tick, post-dt-skip, or post-reset).
    pub d_term_skipped: Option<pid_ctl_core::DTermSkipReason>,
    pub state_write_failed: Option<StateStoreError>,
}

/// Stable iteration record for JSON output.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IterationRecord {
    pub schema_version: u64,
    /// Wall-clock timestamp when the record was produced (ISO 8601 UTC, second precision).
    pub ts: String,
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
            ts: now_iso8601(),
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

#[must_use]
pub fn now_iso8601() -> String {
    use time::OffsetDateTime;
    use time::format_description::FormatItem;
    use time::macros::format_description;
    // Produce YYYY-MM-DDTHH:MM:SSZ (no sub-seconds, Z suffix).
    const FMT: &[FormatItem<'_>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    OffsetDateTime::now_utc()
        .format(FMT)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
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

#[cfg(test)]
mod tests {
    use super::{ControllerSession, SessionConfig};
    use crate::adapters::CvSink;
    use pid_ctl_core::PidConfig;

    struct OkSink;

    impl CvSink for OkSink {
        fn write_cv(&mut self, _cv: f64) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn post_dt_skip_zeros_d_on_next_tick() {
        let cfg = PidConfig {
            kd: 2.0,
            ki: 0.0,
            setpoint: 0.0,
            ..PidConfig::default()
        };

        let session_cfg = SessionConfig {
            pid: cfg,
            ..SessionConfig::default()
        };

        let mut session = ControllerSession::new(session_cfg).expect("session");
        let mut sink = OkSink;

        session.process_pv(1.0, 1.0, &mut sink).expect("tick1");
        let r2 = session.process_pv(2.0, 1.0, &mut sink).expect("tick2");
        assert!(
            (r2.record.d - (-2.0)).abs() < 1e-9,
            "expected D=-kd*delta_pv, got {}",
            r2.record.d
        );

        session.on_dt_skipped();
        let r3 = session.process_pv(3.0, 1.0, &mut sink).expect("tick3");
        assert!(
            r3.record.d.abs() < f64::EPSILON,
            "D should be zero after dt skip"
        );
    }

    #[test]
    fn post_dt_skip_reason_surfaces_in_tick_outcome() {
        use pid_ctl_core::DTermSkipReason;

        let cfg = PidConfig {
            kd: 1.0,
            setpoint: 50.0,
            ..PidConfig::default()
        };

        let session_cfg = SessionConfig {
            pid: cfg,
            ..SessionConfig::default()
        };

        let mut session = ControllerSession::new(session_cfg).expect("session");
        let mut sink = OkSink;

        // First tick seeds last_pv.
        let _ = session.process_pv(45.0, 1.0, &mut sink).expect("tick1");

        session.on_dt_skipped();
        let outcome = session.process_pv(46.0, 1.0, &mut sink).expect("tick2");
        assert_eq!(outcome.d_term_skipped, Some(DTermSkipReason::PostDtSkip));
    }

    #[test]
    fn no_d_term_skip_on_normal_tick() {
        let cfg = PidConfig {
            kd: 1.0,
            setpoint: 50.0,
            ..PidConfig::default()
        };

        let session_cfg = SessionConfig {
            pid: cfg,
            ..SessionConfig::default()
        };

        let mut session = ControllerSession::new(session_cfg).expect("session");
        let mut sink = OkSink;

        let _ = session.process_pv(45.0, 1.0, &mut sink).expect("tick1");
        let outcome = session.process_pv(46.0, 1.0, &mut sink).expect("tick2");
        assert_eq!(outcome.d_term_skipped, None);
    }
}
