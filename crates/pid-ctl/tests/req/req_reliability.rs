//! Reliability & Operational Safety — `pid-ctl_plan.md` (numbered principles).

use assert_cmd::Command;
use std::time::Duration;
use tempfile::tempdir;

/// Plan reliability §3: on PV read failure in `loop`, write `--safe-cv` when set; otherwise
/// do not emit CV (actuator holds last).
#[test]
fn pv_read_failure_invokes_safe_cv_or_hold_last_per_plan() {
    let dir = tempdir().expect("temporary directory");
    let cv_safe = dir.path().join("cv_safe.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "false"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_safe);
    cmd.args(["--safe-cv", "12.34"]);

    cmd.timeout(Duration::from_millis(500));
    let _ = cmd.output();

    let content = std::fs::read_to_string(&cv_safe).expect("read cv file after PV failure");
    assert!(
        content.contains("12.34"),
        "expected safe CV written on PV read failure; got {content:?}"
    );

    let dir_hold = tempdir().expect("temporary directory");
    let cv_hold = dir_hold.path().join("cv_hold.txt");
    std::fs::write(&cv_hold, "88.88\n").expect("seed cv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "false"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_hold);

    cmd.timeout(Duration::from_millis(500));
    let _ = cmd.output();

    let held = std::fs::read_to_string(&cv_hold).expect("read cv file");
    assert_eq!(
        held.trim(),
        "88.88",
        "without --safe-cv, PV failure must not overwrite CV (hold last)"
    );
}

#[test]
#[ignore = "no tick pipeline yet"]
fn cv_write_failure_policy_matches_mode_once_loop_pipe() {
    todo!("item 17: exit codes, last_cv confirmed-applied semantics");
}
