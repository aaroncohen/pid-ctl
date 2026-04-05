//! Flag precedence — `pid-ctl_plan.md` (PID Parameters).
//!
//! "CLI flags always win over state file values. If a flag is omitted and the
//! state file contains a value, the state file value is used. --setpoint is
//! required only when absent from both CLI and state file (first run)."

use assert_cmd::Command;
use pid_ctl::app::{StateSnapshot, StateStore};
use predicates::str::contains;
use tempfile::tempdir;

// --- Bugs: the CLI currently ignores state file gains/setpoint (pid-ctl-tbs) ---

/// pid-ctl-tbs: state file gains used when CLI flags omitted.
/// Seed kp=5.0, setpoint=55.0. Run once --pv 50.0 with no --kp or --setpoint.
/// error = 55.0 - 50.0 = 5.0, kp=5.0 => CV = 25.0
#[test]
fn state_file_gains_used_when_cli_flags_omitted() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("fan.json");
    let store = StateStore::new(&state_path);

    store
        .save(&StateSnapshot {
            kp: Some(5.0),
            ki: Some(0.0),
            kd: Some(0.0),
            setpoint: Some(55.0),
            ..StateSnapshot::default()
        })
        .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv", "50.0", "--cv-stdout", "--state"]);
    cmd.arg(&state_path);

    cmd.assert().success().stdout(contains("25"));
}

/// pid-ctl-tbs: CLI flags override state file values.
/// Seed kp=5.0 in state, pass --kp 1.0 on CLI. CLI wins.
/// error = 55.0 - 50.0 = 5.0, kp=1.0 => CV = 5.0
#[test]
fn cli_flags_override_state_file_values() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("fan.json");
    let store = StateStore::new(&state_path);

    store
        .save(&StateSnapshot {
            kp: Some(5.0),
            ki: Some(0.0),
            kd: Some(0.0),
            setpoint: Some(55.0),
            ..StateSnapshot::default()
        })
        .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--kp",
        "1.0",
        "--cv-stdout",
        "--state",
    ]);
    cmd.arg(&state_path);

    cmd.assert().success().stdout(contains("5"));
}

/// pid-ctl-tbs: --setpoint required when absent from both CLI and state file.
#[test]
fn setpoint_required_when_absent_from_cli_and_state() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv", "50.0", "--cv-stdout"]);

    cmd.assert().code(3).stderr(contains("setpoint"));
}
