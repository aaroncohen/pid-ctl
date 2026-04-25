//! Requirement tests for the `loop` subcommand — bead pid-ctl-cv1.

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use predicates::str::contains;

use crate::helpers::assert_json_ts_iso8601_utc;
use std::time::Duration;
use tempfile::tempdir;

/// pid-ctl-cv1: loop runs continuously, advances iter, and writes state.
///
/// Uses `--max-iterations 3` (a hidden test hook) so the child exits on its own
/// after three completed PID ticks. This replaces the earlier kill-after-N-ms
/// approach, which was inherently racy on slow/loaded CI machines.
#[test]
fn loop_basic_iterations() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let state_path = dir.path().join("ctrl.json");

    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
        "/dev/null",
        "--max-iterations",
        "3",
        "--state",
    ]);
    cmd.arg(&state_path);

    // Generous ceiling so the slowest CI runners still get a clean self-exit.
    cmd.timeout(Duration::from_secs(10));
    cmd.assert().success();

    assert!(
        state_path.exists(),
        "state file should exist after loop ran"
    );
    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    assert!(
        snapshot.iter >= 3,
        "expected iter >= 3 after --max-iterations 3, got {}",
        snapshot.iter
    );
}

/// pid-ctl-cv1: loop rejects `--pv <literal>` with exit 3.
#[test]
fn loop_rejects_literal_pv() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--pv",
        "50.0",
        "--interval",
        "1s",
        "--setpoint",
        "55",
        "--cv-stdout",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("loop requires --pv-file or --pv-cmd"));
}

/// pid-ctl-cv1: loop requires `--interval`.
#[test]
fn loop_requires_interval() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55", "--cv-file", "/dev/null"]);

    cmd.assert()
        .code(3)
        .stderr(contains("loop requires --interval"));
}

/// pid-ctl-cv1: loop requires a PV source.
#[test]
fn loop_requires_pv_source() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--interval",
        "1s",
        "--setpoint",
        "55",
        "--cv-file",
        "/dev/null",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("loop requires a PV source"));
}

/// pid-ctl-cv1: loop requires a CV sink.
#[test]
fn loop_requires_cv_sink() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--interval", "1s", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55"]);

    cmd.assert()
        .code(3)
        .stderr(contains("loop requires a CV sink"));
}

/// pid-ctl-qde: loop exits with code 2 after --cv-fail-after consecutive CV write failures.
///
/// Uses --cv-file pointing to a path inside a nonexistent directory so every
/// write fails immediately.
#[test]
fn loop_cv_fail_after_exits_2() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    // cv path is inside a nonexistent subdirectory — every write will fail.
    let cv_path = dir.path().join("nonexistent_subdir").join("cv.txt");

    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "echo 50.0"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_path);
    cmd.args(["--cv-fail-after", "3"]);

    cmd.timeout(Duration::from_secs(5));

    cmd.assert().code(2);
}

/// pid-ctl-qde: loop appends NDJSON lines to the log file.
///
/// Marked `#[ignore]` because timing-dependent process-kill approach is racy on
/// slow machines.  Run manually with
/// `cargo test -- --ignored loop_log_appends_ndjson`.
#[test]
#[ignore = "timing-sensitive: kill after N ms may not produce stable log line count"]
fn loop_log_appends_ndjson() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let log_path = dir.path().join("loop.ndjson");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
        "/dev/null",
        "--log",
    ]);
    cmd.arg(&log_path);

    cmd.timeout(Duration::from_millis(400));
    let _ = cmd.output();

    // Log file should exist and contain at least one valid JSON line.
    assert!(log_path.exists(), "log file should exist after loop ran");
    let content = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "log file should have at least one line");
    // Each line should be valid JSON.
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("log line is not valid JSON: {line}"));
        assert_json_ts_iso8601_utc(&value);
    }
}

/// pid-ctl-qde: --safe-cv flag is accepted without parse error.
#[test]
fn loop_accepts_safe_cv_flag() {
    // We just verify this doesn't exit with code 3 (config error).
    // The loop itself is killed immediately via timeout.
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
        "/dev/null",
        "--safe-cv",
        "0.0",
    ]);

    cmd.timeout(Duration::from_millis(200));
    let output = cmd.output().expect("run cmd");
    // Must not exit with config error 3.
    assert_ne!(
        output.status.code(),
        Some(3),
        "--safe-cv caused config error"
    );
}

/// pid-ctl-qde: --log flag is accepted without parse error.
#[test]
fn loop_accepts_log_flag() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let log_path = dir.path().join("loop.ndjson");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
        "/dev/null",
        "--log",
    ]);
    cmd.arg(&log_path);

    cmd.timeout(Duration::from_millis(200));
    let output = cmd.output().expect("run cmd");
    // Must not exit with config error 3.
    assert_ne!(output.status.code(), Some(3), "--log caused config error");
}

/// pid-ctl-gpl: --quiet suppresses stderr diagnostic output.
/// Verify the flag is accepted and does not cause exit 3.
#[test]
fn loop_quiet_flag_accepted() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
        "/dev/null",
        "--quiet",
    ]);

    cmd.timeout(Duration::from_millis(200));
    let output = cmd.output().expect("run cmd");
    assert_ne!(output.status.code(), Some(3), "--quiet caused config error");
}
