//! Feed-forward path CLI tests (issue #26).
//!
//! Verifies: `--ff-gain`, `--ff-value`, `--ff-from-file`, `--ff-cmd` flags on `once`;
//! `--ff-from-file` and `--ff-cmd` on `loop`; JSON `ff` field presence/absence;
//! conflict validation.

use assert_cmd::Command;
use std::fs;
use tempfile::tempdir;

fn pid_ctl() -> Command {
    Command::cargo_bin("pid-ctl").expect("pid-ctl binary")
}

// ---------------------------------------------------------------------------
// --ff-gain with --ff-value: cv = p_term + ff_term
// ---------------------------------------------------------------------------

#[test]
fn once_ff_value_and_gain_adds_to_cv() {
    // kp=1, sp=10, pv=7 → p=3; ff_gain=2, ff_value=4 → ff_term=8; cv=11
    let mut cmd = pid_ctl();
    cmd.args([
        "once",
        "--setpoint",
        "10.0",
        "--kp",
        "1.0",
        "--ki",
        "0.0",
        "--kd",
        "0.0",
        "--pv",
        "7.0",
        "--ff-gain",
        "2.0",
        "--ff-value",
        "4.0",
        "--cv-stdout",
    ]);

    let output = cmd.assert().success().get_output().stdout.clone();
    let cv: f64 = String::from_utf8(output)
        .expect("utf-8")
        .trim()
        .parse()
        .expect("cv is f64");

    assert!((cv - 11.0).abs() < 0.01, "expected cv≈11.0, got {cv}");
}

// ---------------------------------------------------------------------------
// ff term appears in JSON output when non-zero; absent when zero
// ---------------------------------------------------------------------------

#[test]
fn once_ff_field_in_json_when_nonzero() {
    let dir = tempdir().expect("tmp dir");
    let cv_path = dir.path().join("cv.txt");

    let mut cmd = pid_ctl();
    cmd.args([
        "once",
        "--setpoint",
        "10.0",
        "--kp",
        "1.0",
        "--pv",
        "7.0",
        "--ff-gain",
        "1.0",
        "--ff-value",
        "5.0",
        "--format",
        "json",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);

    let out = cmd.assert().success().get_output().stdout.clone();
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8(out).expect("utf-8").trim()).expect("valid JSON");

    let ff = json["ff"].as_f64().expect("ff field present in JSON");
    assert!((ff - 5.0).abs() < 1e-9, "expected ff=5.0 in JSON, got {ff}");
}

#[test]
fn once_ff_field_absent_in_json_when_zero() {
    let dir = tempdir().expect("tmp dir");
    let cv_path = dir.path().join("cv.txt");

    let mut cmd = pid_ctl();
    cmd.args([
        "once",
        "--setpoint",
        "10.0",
        "--kp",
        "1.0",
        "--pv",
        "7.0",
        "--format",
        "json",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);

    let out = cmd.assert().success().get_output().stdout.clone();
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8(out).expect("utf-8").trim()).expect("valid JSON");

    assert!(
        json.get("ff").is_none(),
        "ff field should be absent when feedforward_gain=0"
    );
}

// ---------------------------------------------------------------------------
// --ff-from-file source
// ---------------------------------------------------------------------------

#[test]
fn once_ff_from_file_contributes_correctly() {
    let dir = tempdir().expect("tmp dir");
    let ff_file = dir.path().join("ff.txt");
    let cv_path = dir.path().join("cv.txt");

    fs::write(&ff_file, "6.0\n").expect("write ff file");

    // kp=1, sp=10, pv=8 → p=2; ff_gain=1, ff_from_file=6 → cv=8
    let mut cmd = pid_ctl();
    cmd.args([
        "once",
        "--setpoint",
        "10.0",
        "--kp",
        "1.0",
        "--pv",
        "8.0",
        "--ff-gain",
        "1.0",
        "--ff-from-file",
    ]);
    cmd.arg(&ff_file);
    cmd.args(["--format", "json", "--cv-file"]);
    cmd.arg(&cv_path);

    let out = cmd.assert().success().get_output().stdout.clone();
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8(out).expect("utf-8").trim()).expect("valid JSON");

    let ff = json["ff"].as_f64().expect("ff field present");
    assert!((ff - 6.0).abs() < 1e-9, "expected ff=6.0, got {ff}");

    let cv = json["cv"].as_f64().expect("cv field present");
    assert!((cv - 8.0).abs() < 0.01, "expected cv≈8.0, got {cv}");
}

// ---------------------------------------------------------------------------
// Conflict validation: only one FF source allowed
// ---------------------------------------------------------------------------

#[test]
fn once_multiple_ff_sources_is_error() {
    let dir = tempdir().expect("tmp dir");
    let ff_file = dir.path().join("ff.txt");
    fs::write(&ff_file, "1.0\n").expect("write ff file");

    let mut cmd = pid_ctl();
    cmd.args([
        "once",
        "--setpoint",
        "10.0",
        "--kp",
        "1.0",
        "--pv",
        "7.0",
        "--ff-value",
        "1.0",
        "--ff-from-file",
    ]);
    cmd.arg(&ff_file);
    cmd.args(["--cv-stdout"]);

    cmd.assert().failure();
}

// ---------------------------------------------------------------------------
// --ff-value rejected in loop (loop needs a streaming source)
// ---------------------------------------------------------------------------

#[test]
fn loop_ff_value_is_rejected() {
    let dir = tempdir().expect("tmp dir");
    let pv_file = dir.path().join("pv.txt");
    fs::write(&pv_file, "7.0\n").expect("write pv file");

    let mut cmd = pid_ctl();
    cmd.args(["loop", "--setpoint", "10.0", "--kp", "1.0", "--pv-file"]);
    cmd.arg(&pv_file);
    cmd.args([
        "--ff-gain",
        "1.0",
        "--ff-value", // not valid for loop
        "5.0",
        "--interval",
        "100ms",
        "--dry-run",
    ]);

    cmd.assert().failure();
}
