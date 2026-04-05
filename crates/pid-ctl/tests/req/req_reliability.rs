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

/// Plan reliability item 17: CV write failure policy is mode-dependent — `once` exits 5 and
/// keeps `last_cv` at last confirmed-applied; `loop` exits 2 after `--cv-fail-after` consecutive
/// failures. (`pipe` follows Unix stdout/SIGPIPE conventions; not duplicated here.)
#[test]
fn cv_write_failure_policy_matches_mode_once_loop_pipe() {
    let dir = tempdir().expect("temporary directory");
    let state_path = dir.path().join("fan.json");
    let bad_cv = dir.path().join("missing").join("cv.txt");

    std::fs::write(
        &state_path,
        r#"{"schema_version":1,"last_cv":41.0,"iter":0}"#,
    )
    .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--kp",
        "2.0",
        "--out-min",
        "0.0",
        "--out-max",
        "100.0",
        "--cv-file",
    ]);
    cmd.arg(&bad_cv);
    cmd.arg("--state");
    cmd.arg(&state_path);

    cmd.assert().code(5);

    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("state json");
    assert_eq!(persisted["last_cv"].as_f64(), Some(41.0));

    let dir_loop = tempdir().expect("temporary directory");
    let pv_path = dir_loop.path().join("pv.txt");
    let cv_path = dir_loop.path().join("no_dir").join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_path);
    cmd.args(["--cv-fail-after", "2"]);
    cmd.timeout(Duration::from_secs(5));

    cmd.assert().code(2);
}
