//! Shared tick driver consumed by both `run_loop` and `tune::run`.
//!
//! Both paths call [`step`] with a [`TickContext`] and a [`TickObserver`].
//! Common logic (`process_pv`, d-term event, iteration log, state-failure report,
//! cv-write-failed event, safe-cv write on exhaustion) lives here; mode-specific
//! side-effects (stdout JSON, verbose stderr, TUI state update) are delegated to
//! the observer.

use crate::adapters::CvSink;
use crate::app::logger::Logger;
use crate::app::loop_runtime::{emit_state_write_failure, write_safe_cv};
use crate::app::{ControllerSession, TickError, TickOutcome};
use crate::json_events;
use std::path::Path;

/// All inputs needed for one PID tick.
pub struct TickContext<'a> {
    pub scaled_pv: f64,
    pub dt: f64,
    pub session: &'a mut ControllerSession,
    pub cv_sink: &'a mut dyn CvSink,
    pub logger: &'a mut Logger,
    pub state_path: Option<&'a Path>,
    pub cv_fail_after: u32,
    pub safe_cv: Option<f64>,
    pub quiet: bool,
}

/// Observer for divergent per-caller behaviour in a shared tick.
///
/// `on_success` is called after the common success-path bookkeeping (d-term log,
/// iteration line write, state-failure report). Callers use it for output-format
/// printing (JSON to stdout) and mode-specific diagnostics (verbose eprintln, TUI update).
///
/// `on_cv_fail` is called before the shared JSON event; callers use it to emit
/// their per-mode failure message (eprintln for `loop`, silence for `--tune`).
pub trait TickObserver {
    fn on_success(&mut self, outcome: &TickOutcome);
    fn on_cv_fail(&mut self, error: &TickError, consecutive: u32);
}

/// Result of one shared tick step.
pub enum TickStepResult {
    /// Tick succeeded; CV was written and the outcome is available.
    Ok(TickOutcome),
    /// CV write failed but the failure limit has not been reached.
    CvFailTransient { consecutive: u32 },
    /// CV write failures exhausted the configured limit; caller should exit with code 2.
    CvFailExhausted(String),
}

/// Executes one PID tick, delegating output and mode-specific side-effects to `observer`.
///
/// Common logic: `process_pv`, d-term event, iteration log, state-failure report,
/// `emit_cv_write_failed`, `write_safe_cv` on exhaustion.
///
/// Returns [`TickStepResult`] which the caller converts to its own error type at
/// the binary boundary.
pub fn step(
    ctx: TickContext<'_>,
    cv_fail_count: &mut u32,
    observer: &mut dyn TickObserver,
) -> TickStepResult {
    let TickContext {
        scaled_pv,
        dt,
        session,
        cv_sink,
        logger,
        state_path,
        cv_fail_after,
        safe_cv,
        quiet,
    } = ctx;

    match session.process_pv(scaled_pv, dt, cv_sink) {
        Ok(outcome) => {
            *cv_fail_count = 0;

            if let Some(reason) = outcome.d_term_skipped {
                json_events::emit_d_term_skipped(logger, reason, outcome.record.iter);
            }

            logger.write_iteration_line(&outcome.record);

            if let Some(ref error) = outcome.state_write_failed {
                emit_state_write_failure(session, state_path, logger, error, quiet);
            }

            observer.on_success(&outcome);
            TickStepResult::Ok(outcome)
        }
        Err(error) => {
            *cv_fail_count += 1;
            observer.on_cv_fail(&error, *cv_fail_count);
            json_events::emit_cv_write_failed(logger, error.to_string(), *cv_fail_count);
            if *cv_fail_count >= cv_fail_after {
                write_safe_cv(safe_cv, cv_sink, session);
                TickStepResult::CvFailExhausted(format!(
                    "exiting after {cv_fail_count} consecutive CV write failures: {error}"
                ))
            } else {
                TickStepResult::CvFailTransient {
                    consecutive: *cv_fail_count,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ControllerSession, SessionConfig};
    use pid_ctl_core::PidConfig;

    struct OkSink;
    impl CvSink for OkSink {
        fn write_cv(&mut self, _cv: f64) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailSink;
    impl CvSink for FailSink {
        fn write_cv(&mut self, _cv: f64) -> std::io::Result<()> {
            Err(std::io::Error::other("fake CV failure"))
        }
    }

    #[derive(Default)]
    struct Rec {
        successes: u32,
        d_term_skips: u32,
        cv_fails: Vec<u32>,
    }

    impl TickObserver for Rec {
        fn on_success(&mut self, outcome: &TickOutcome) {
            self.successes += 1;
            if outcome.d_term_skipped.is_some() {
                self.d_term_skips += 1;
            }
        }
        fn on_cv_fail(&mut self, _error: &TickError, consecutive: u32) {
            self.cv_fails.push(consecutive);
        }
    }

    fn basic_session() -> ControllerSession {
        ControllerSession::new(SessionConfig {
            pid: PidConfig {
                kp: 1.0,
                setpoint: 10.0,
                ..PidConfig::default()
            },
            ..SessionConfig::default()
        })
        .expect("session")
    }

    #[test]
    fn happy_path_notifies_observer() {
        let mut session = basic_session();
        let mut sink = OkSink;
        let mut logger = Logger::none();
        let mut rec = Rec::default();
        let mut fail_count = 0u32;

        let ctx = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 3,
            safe_cv: None,
            quiet: true,
        };
        let result = step(ctx, &mut fail_count, &mut rec);
        assert!(matches!(result, TickStepResult::Ok(_)));
        assert_eq!(rec.successes, 1);
        assert_eq!(rec.cv_fails.len(), 0);
        assert_eq!(fail_count, 0);
    }

    #[test]
    fn cv_fail_transient_increments_counter() {
        let mut session = basic_session();
        let mut sink = FailSink;
        let mut logger = Logger::none();
        let mut rec = Rec::default();
        let mut fail_count = 0u32;

        let ctx = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 3,
            safe_cv: None,
            quiet: true,
        };
        let result = step(ctx, &mut fail_count, &mut rec);
        assert!(matches!(
            result,
            TickStepResult::CvFailTransient { consecutive: 1 }
        ));
        assert_eq!(rec.cv_fails, vec![1]);
        assert_eq!(fail_count, 1);
    }

    #[test]
    fn cv_fail_exhausted_at_threshold() {
        let mut session = basic_session();
        let mut sink = FailSink;
        let mut logger = Logger::none();
        let mut rec = Rec::default();
        let mut fail_count = 0u32;

        let ctx = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 1,
            safe_cv: None,
            quiet: true,
        };
        let result = step(ctx, &mut fail_count, &mut rec);
        assert!(matches!(result, TickStepResult::CvFailExhausted(_)));
        assert_eq!(fail_count, 1);
    }

    #[test]
    fn d_term_skip_surfaces_in_observer() {
        // kd must be non-zero: the D-term early-returns None when kd == 0.
        let mut session = ControllerSession::new(SessionConfig {
            pid: PidConfig {
                kp: 1.0,
                kd: 1.0,
                setpoint: 10.0,
                ..PidConfig::default()
            },
            ..SessionConfig::default()
        })
        .expect("session");
        let mut sink = OkSink;
        let mut logger = Logger::none();
        let mut rec = Rec::default();
        let mut fail_count = 0u32;

        // First tick seeds last_pv.
        session.process_pv(5.0, 1.0, &mut sink).unwrap();
        // Mark dt skipped so next tick has D-term skip.
        session.on_dt_skipped();

        let ctx = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 3,
            safe_cv: None,
            quiet: true,
        };
        let result = step(ctx, &mut fail_count, &mut rec);
        assert!(matches!(result, TickStepResult::Ok(_)));
        assert_eq!(rec.d_term_skips, 1);
    }

    #[test]
    fn cv_fail_count_resets_on_success() {
        let mut session = basic_session();
        let mut logger = Logger::none();
        let mut rec = Rec::default();
        let mut fail_count = 2u32;

        // One more fail (total 3, threshold 5 → transient).
        let mut fail_sink = FailSink;
        let ctx = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut fail_sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 5,
            safe_cv: None,
            quiet: true,
        };
        step(ctx, &mut fail_count, &mut rec);
        assert_eq!(fail_count, 3);

        // Succeed — count resets.
        let mut ok_sink = OkSink;
        let ctx2 = TickContext {
            scaled_pv: 5.0,
            dt: 1.0,
            session: &mut session,
            cv_sink: &mut ok_sink,
            logger: &mut logger,
            state_path: None,
            cv_fail_after: 5,
            safe_cv: None,
            quiet: true,
        };
        step(ctx2, &mut fail_count, &mut rec);
        assert_eq!(fail_count, 0);
    }
}
