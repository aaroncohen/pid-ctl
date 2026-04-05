//! Output formatting and stdout/stderr behavior for `once` and `pipe` CLI modes.
//! Covers bead pid-ctl-8vb.7: "Output formatting and stdout/stderr behavior are covered by requirement tests."

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use predicates::str::{contains, starts_with};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// once mode
// ---------------------------------------------------------------------------

/// Basic PV computation: pv=50.0, sp=55.0, kp=1.0 → CV = 5.00.
#[test]
fn once_basic_pv_computation() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    cmd.assert().success().stdout(starts_with("5.00"));
}

/// `--format json` with `--cv-file` writes valid JSON to stdout containing `"cv"` and `"sp"`.
#[test]
fn once_format_json_with_cv_file() {
    let tempdir = tempdir().expect("temporary directory");
    let cv_path = tempdir.path().join("cv.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);
    cmd.args(["--format", "json"]);

    let output = cmd.assert().success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).expect("valid UTF-8 stdout");

    // Must be parseable JSON
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");

    assert!(
        (value["cv"].as_f64().expect("cv field present") - 5.0).abs() < 1e-9,
        "cv should be 5.0, got {}",
        value["cv"]
    );
    assert!(
        (value["sp"].as_f64().expect("sp field present") - 55.0).abs() < 1e-9,
        "sp should be 55.0, got {}",
        value["sp"]
    );
}

/// `once --state` creates the state file with expected fields (iter=1, setpoint, `last_cv`).
#[test]
fn once_state_persistence() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ctrl.json");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
        "--state",
    ]);
    cmd.arg(&state_path);

    cmd.assert().success();

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    assert_eq!(snapshot.iter, 1, "iter should be 1 after first run");
    assert!(
        snapshot.setpoint.is_some(),
        "setpoint should be persisted in state"
    );
    assert!(
        snapshot.last_cv.is_some(),
        "last_cv should be persisted in state"
    );
    let cv = snapshot.last_cv.expect("last_cv present");
    assert!((cv - 5.0).abs() < 1e-9, "last_cv should be ~5.0, got {cv}");
}

/// `--name` appears in JSON output as `"name":"test-ctrl"`.
#[test]
fn once_name_appears_in_json_output() {
    let tempdir = tempdir().expect("temporary directory");
    let cv_path = tempdir.path().join("cv.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--name",
        "test-ctrl",
        "--format",
        "json",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);

    cmd.assert()
        .success()
        .stdout(contains(r#""name":"test-ctrl""#));
}

// ---------------------------------------------------------------------------
// pipe mode
// ---------------------------------------------------------------------------

/// Basic stream: three PV values produce three CV lines starting with the correct values.
#[test]
fn pipe_basic_stream() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--kp", "1.0"]);
    cmd.write_stdin("50.0\n51.0\n52.0\n");

    let output = cmd.assert().success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).expect("valid UTF-8 stdout");
    let lines: Vec<&str> = stdout.lines().collect();

    assert_eq!(
        lines.len(),
        3,
        "expected 3 output lines, got {}",
        lines.len()
    );
    assert!(
        lines[0].starts_with("5.00"),
        "first CV should start with '5.00', got '{}'",
        lines[0]
    );
    assert!(
        lines[1].starts_with("4.00"),
        "second CV should start with '4.00', got '{}'",
        lines[1]
    );
    assert!(
        lines[2].starts_with("3.00"),
        "third CV should start with '3.00', got '{}'",
        lines[2]
    );
}

/// Blank lines in stdin are skipped — only real PV values produce output lines.
#[test]
fn pipe_blank_lines_skipped() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--kp", "1.0"]);
    cmd.write_stdin("50.0\n\n51.0\n");

    let output = cmd.assert().success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).expect("valid UTF-8 stdout");
    let lines: Vec<&str> = stdout.lines().collect();

    assert_eq!(
        lines.len(),
        2,
        "blank lines should be skipped; expected 2 output lines, got {}",
        lines.len()
    );
}

/// `--scale` multiplies the raw stdin PV before PID: 50000 * 0.001 = 50.0, CV = 5.00.
#[test]
fn pipe_with_scale() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "pipe",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--scale",
        "0.001",
    ]);
    cmd.write_stdin("50000\n");

    cmd.assert().success().stdout(starts_with("5.00"));
}
