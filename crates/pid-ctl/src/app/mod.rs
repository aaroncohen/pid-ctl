//! Controller session scaffolding and persistence primitives for the application layer.

pub mod state_store;

use crate::adapters::CvSink;
use pid_ctl_core::{ConfigError, PidConfig, PidController, StepInput, StepResult};
use serde::Serialize;
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;

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
    state_store: Option<StateStore>,
    _state_lock: Option<StateLock>,
    /// Maximum interval between disk flushes. `None` = flush every call.
    flush_interval: Option<Duration>,
    /// Monotonic instant of the last successful (or attempted) flush.
    last_flush: Option<Instant>,
    /// Consecutive state write failures since last success.
    state_fail_count: u32,
    /// Threshold after which failures escalate to a prominent per-cycle warning.
    state_fail_after: u32,
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
            flush_interval: config.flush_interval,
            last_flush: None,
            state_fail_count: 0,
            state_fail_after: config.state_fail_after,
        })
    }

    #[must_use]
    pub fn config(&self) -> &PidConfig {
        self.controller.config()
    }

    /// Last CV confirmed applied to the actuator (from the persisted snapshot).
    #[must_use]
    pub fn last_applied_cv(&self) -> Option<f64> {
        self.snapshot.last_cv
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
    }

    /// Updates coalesced state flush cadence (used when `--interval` changes at runtime).
    pub fn set_flush_interval(&mut self, flush_interval: Option<Duration>) {
        self.flush_interval = flush_interval;
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

    /// Writes the current snapshot to disk, respecting `flush_interval` coalescing.
    ///
    /// When `flush_interval` is set, skips the write if not enough time has
    /// elapsed since the last flush. In-memory state is always current regardless.
    ///
    /// Updates `state_fail_count` on failure / success for escalation tracking.
    fn persist_snapshot(&mut self) -> Result<(), StateStoreError> {
        let Some(state_store) = &self.state_store else {
            return Ok(());
        };

        // Coalescing: skip disk write if within the flush interval.
        if let (Some(interval), Some(last)) = (self.flush_interval, self.last_flush)
            && last.elapsed() < interval
        {
            return Ok(());
        }

        let result = state_store.save(&self.snapshot);
        match &result {
            Ok(()) => {
                self.state_fail_count = 0;
                self.last_flush = Some(Instant::now());
            }
            Err(_) => {
                self.state_fail_count = self.state_fail_count.saturating_add(1);
            }
        }
        result
    }

    /// Forces a disk flush regardless of `flush_interval`.
    ///
    /// Used at shutdown to ensure the final in-memory state is persisted.
    /// Updates `state_fail_count` on failure / success.
    pub fn force_flush(&mut self) -> Option<StateStoreError> {
        let Some(state_store) = &self.state_store else {
            return None;
        };
        match state_store.save(&self.snapshot) {
            Ok(()) => {
                self.state_fail_count = 0;
                self.last_flush = Some(Instant::now());
                None
            }
            Err(e) => {
                self.state_fail_count = self.state_fail_count.saturating_add(1);
                Some(e)
            }
        }
    }

    /// Returns `true` when the number of consecutive state write failures has
    /// reached the escalation threshold (`--state-fail-after`).
    #[must_use]
    pub fn state_fail_escalated(&self) -> bool {
        self.state_fail_count >= self.state_fail_after && self.state_fail_after > 0
    }

    /// Returns the current consecutive state write failure count.
    #[must_use]
    pub fn state_fail_count(&self) -> u32 {
        self.state_fail_count
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
