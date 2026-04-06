//! `--dry-run` mode: PID computation proceeds but CV is not written to the sink.
//! State is still saved when `--state` is configured.
//! Covers issue pid-ctl-8vb.22.

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use tempfile::tempdir;

/// `--dry-run` with `once` exits 0 and does not write the CV file.
#[test]
fn once_dry_run_does_not_write_cv_file() {
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
        "--dry-run",
    ]);

    cmd.assert().success();

    assert!(
        !cv_path.exists(),
        "CV file should NOT be created when --dry-run is set"
    );
}

/// `--dry-run` with `once` still saves state when `--state` is configured.
///
/// The iteration counter and integral accumulator advance; `last_cv` is the
/// computed (but not physically written) CV — stored for mathematical continuity
/// on subsequent ticks.
#[test]
fn once_dry_run_saves_state() {
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
        "--dry-run",
        "--state",
    ]);
    cmd.arg(&state_path);

    cmd.assert().success();

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    assert_eq!(snapshot.iter, 1, "iter should advance to 1 after one tick");
    // State is saved including the computed CV for PID continuity.
    assert!(
        snapshot.last_cv.is_some(),
        "last_cv should be saved for continuity even in dry-run mode"
    );
    let last_cv = snapshot.last_cv.unwrap();
    assert!(
        (last_cv - 5.0).abs() < 1e-9,
        "last_cv should be the computed CV (5.0), got {last_cv}"
    );
}

/// `--dry-run` with `once` and `--format json` emits iteration record to stdout,
/// including the computed CV value.
#[test]
fn once_dry_run_emits_json_with_computed_cv() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--dry-run",
        "--format",
        "json",
    ]);

    let output = cmd.assert().success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).expect("valid UTF-8 stdout");

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");

    let cv = value["cv"].as_f64().expect("cv field present");
    assert!(
        (cv - 5.0).abs() < 1e-9,
        "computed cv should be 5.0 (sp - pv = 5.0 with kp=1.0), got {cv}"
    );
}

/// `--dry-run` with `once` does not require a CV sink to be specified.
#[test]
fn once_dry_run_does_not_require_cv_sink() {
    // No --cv-stdout or --cv-file given — should succeed.
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--dry-run",
    ]);

    cmd.assert().success();
}

/// `--dry-run` with `pipe` is rejected with exit 3.
#[test]
fn pipe_dry_run_rejected() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--dry-run"]);
    cmd.write_stdin("50.0\n");

    cmd.assert().code(3).stderr(predicates::str::contains(
        "--dry-run is not meaningful with pipe",
    ));
}
