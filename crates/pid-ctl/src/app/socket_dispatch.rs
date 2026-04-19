//! Socket request dispatcher for the live PID loop (Unix only).
//!
//! Handles [`Request`] messages arriving on the Unix socket during a running
//! `loop`, modifying session gains/setpoint and producing [`Response`] messages.
//! Side effects (hold, resume, interval change) are signalled via [`SocketSideEffect`].

use crate::app::ControllerSession;
use crate::app::loop_runtime::LoopControls;
use crate::json_events;
use crate::socket::{Request, Response};

/// Side effects that a socket command may request the loop to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketSideEffect {
    None,
    Hold,
    Resume,
    IntervalChanged,
}

/// Dispatches a socket [`Request`] against the live controller session and
/// returns a JSON [`Response`] plus any side effect for the loop to apply.
pub fn handle_socket_request(
    req: &Request,
    session: &mut ControllerSession,
    controls: &mut dyn LoopControls,
    log_file: &mut Option<std::fs::File>,
) -> (Response, SocketSideEffect) {
    match req {
        Request::Status => {
            let cfg = session.config();
            (
                Response::Status {
                    ok: true,
                    iter: session.iter(),
                    pv: session.last_pv().unwrap_or(0.0),
                    sp: cfg.setpoint,
                    err: session.last_error().unwrap_or(0.0),
                    kp: cfg.kp,
                    ki: cfg.ki,
                    kd: cfg.kd,
                    cv: session.last_applied_cv().unwrap_or(0.0),
                    i_acc: session.i_acc(),
                },
                SocketSideEffect::None,
            )
        }
        Request::Set { param, value } => {
            handle_socket_set(param, *value, session, controls, log_file)
        }
        Request::Reset => {
            let i_acc_before = session.i_acc();
            session.reset_integral();
            json_events::emit_integral_reset(log_file, i_acc_before, session.iter(), "socket");
            (
                Response::Reset {
                    ok: true,
                    i_acc_before,
                },
                SocketSideEffect::None,
            )
        }
        Request::Hold => (
            Response::Ack {
                ok: true,
                error: None,
            },
            SocketSideEffect::Hold,
        ),
        Request::Resume => (
            Response::Ack {
                ok: true,
                error: None,
            },
            SocketSideEffect::Resume,
        ),
        Request::Save => {
            if !session.has_state_store() {
                return (
                    Response::Ack {
                        ok: false,
                        error: Some(String::from(
                            "no state store: loop was not started with --state",
                        )),
                    },
                    SocketSideEffect::None,
                );
            }
            if let Some(err) = session.force_flush() {
                (
                    Response::Ack {
                        ok: false,
                        error: Some(format!("save failed: {err}")),
                    },
                    SocketSideEffect::None,
                )
            } else {
                (
                    Response::Ack {
                        ok: true,
                        error: None,
                    },
                    SocketSideEffect::None,
                )
            }
        }
    }
}

/// Apply a single gain parameter (kp/ki/kd) to the session and emit the change event.
/// Returns the old value. Gains are ordered [kp, ki, kd] throughout.
fn apply_gain_param(
    param: &str,
    value: f64,
    session: &mut ControllerSession,
    log_file: &mut Option<std::fs::File>,
) -> f64 {
    let idx = match param {
        "kp" => 0usize,
        "ki" => 1,
        "kd" => 2,
        _ => unreachable!("apply_gain_param called with non-gain param: {param}"),
    };
    let cfg = session.config();
    let mut gains = [cfg.kp, cfg.ki, cfg.kd];
    let old = gains[idx];
    gains[idx] = value;
    session.set_gains(gains[0], gains[1], gains[2]);
    json_events::emit_gains_changed(
        log_file,
        session.config().kp,
        session.config().ki,
        session.config().kd,
        session.config().setpoint,
        session.iter(),
        "socket",
    );
    old
}

fn handle_socket_set(
    param: &str,
    value: f64,
    session: &mut ControllerSession,
    controls: &mut dyn LoopControls,
    log_file: &mut Option<std::fs::File>,
) -> (Response, SocketSideEffect) {
    use crate::app::loop_runtime::apply_runtime_interval;
    use std::time::Duration;

    let settable = || {
        vec![
            String::from("kp"),
            String::from("ki"),
            String::from("kd"),
            String::from("sp"),
            String::from("interval"),
        ]
    };

    match param {
        "kp" | "ki" | "kd" => {
            let old = apply_gain_param(param, value, session, log_file);
            (
                Response::Set {
                    ok: true,
                    param: param.to_string(),
                    old,
                    new: value,
                },
                SocketSideEffect::None,
            )
        }
        "sp" => {
            let old = session.config().setpoint;
            session.set_setpoint(value);
            json_events::emit_gains_changed(
                log_file,
                session.config().kp,
                session.config().ki,
                session.config().kd,
                value,
                session.iter(),
                "socket",
            );
            (
                Response::Set {
                    ok: true,
                    param: String::from("sp"),
                    old,
                    new: value,
                },
                SocketSideEffect::None,
            )
        }
        "interval" => {
            let old = controls.interval().as_secs_f64();
            let new_interval = Duration::from_secs_f64(value);
            apply_runtime_interval(session, controls, new_interval);
            (
                Response::Set {
                    ok: true,
                    param: String::from("interval"),
                    old,
                    new: value,
                },
                SocketSideEffect::IntervalChanged,
            )
        }
        _ => (
            Response::ErrorUnknownParam {
                ok: false,
                error: format!("unknown parameter: {param}"),
                settable: settable(),
            },
            SocketSideEffect::None,
        ),
    }
}
