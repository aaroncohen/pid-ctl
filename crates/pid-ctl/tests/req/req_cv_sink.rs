//! CV sink adapter tests — `--cv-cmd`.
//! Covers bead pid-ctl-8vb.21.

use assert_cmd::Command;
use predicates::str::contains;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// `once --cv-cmd` executes the command with `{cv}` substituted.
/// pv=50.0, sp=55.0, kp=1.0 → CV=5.00; command writes that to a file.
#[test]
fn once_cv_cmd_substitutes_cv() {
    let dir = tempdir().expect("temporary directory");
    let cv_path = dir.path().join("cv.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        &format!("echo {{cv}} > {}", cv_path.display()),
    ]);

    cmd.assert().success();

    let written = std::fs::read_to_string(&cv_path).expect("cv file");
    assert_eq!(written.trim(), "5.00");
}

/// `once --cv-cmd` respects `--cv-precision` for the `{cv}` substitution.
#[test]
fn once_cv_cmd_respects_precision() {
    let dir = tempdir().expect("temporary directory");
    let cv_path = dir.path().join("cv.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-precision",
        "4",
        "--cv-cmd",
        &format!("echo {{cv}} > {}", cv_path.display()),
    ]);

    cmd.assert().success();

    let written = std::fs::read_to_string(&cv_path).expect("cv file");
    assert_eq!(written.trim(), "5.0000");
}

/// `once --cv-cmd` with `{cv:url}` percent-encodes the value.
/// A negative CV like -5.00 contains only unreserved chars, so no encoding needed,
/// but we verify substitution still works correctly.
#[test]
fn once_cv_cmd_url_substitution() {
    let dir = tempdir().expect("temporary directory");
    let cv_path = dir.path().join("cv.txt");

    // pv=60, sp=55, kp=1 → error=-5 → CV=-5.00
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        &format!("echo {{cv:url}} > {}", cv_path.display()),
    ]);

    cmd.assert().success();

    let written = std::fs::read_to_string(&cv_path).expect("cv file");
    // -5.00: minus and digits are unreserved, no percent-encoding expected
    assert_eq!(written.trim(), "-5.00");
}

/// `once --cv-cmd` exits with code 5 when the command fails (non-zero exit).
#[test]
fn once_cv_cmd_failure_exits_5() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "exit 1",
    ]);

    cmd.assert().code(5);
}

/// Failed `--cv-cmd` surfaces subprocess stderr in the error (for debugging paths, sim errors, etc.).
#[test]
fn once_cv_cmd_failure_includes_stderr_hint() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "echo simulated_failure >&2; exit 1",
    ]);

    cmd.assert()
        .code(5)
        .stderr(contains("stderr:"))
        .stderr(contains("simulated_failure"));
}

/// `once --cv-cmd` with `--cv-cmd-timeout` enforces the timeout on slow commands.
#[cfg(unix)]
#[test]
fn once_cv_cmd_timeout_enforced() {
    let start = Instant::now();

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "sleep 10",
        "--cv-cmd-timeout",
        "0.3",
    ]);

    // Should fail (exit 5 — write failed) well before the 10-second sleep completes.
    cmd.assert().code(5);

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "expected timeout well before sleep 10s, took {:?}",
        start.elapsed()
    );
}

/// `pipe --cv-cmd` is rejected with exit code 3.
#[test]
fn pipe_rejects_cv_cmd() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "pipe",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "echo {cv}",
    ]);
    cmd.write_stdin("50.0\n");

    cmd.assert().code(3);
}

/// Specifying both `--cv-cmd` and `--cv-stdout` is rejected with exit code 3.
#[test]
fn multiple_cv_sinks_rejected() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "echo {cv}",
        "--cv-stdout",
    ]);

    cmd.assert().code(3).stderr(contains("only one CV sink"));
}

/// `CmdCvSink` enforces a wall-clock bound: a long-running command returns
/// `io::ErrorKind::TimedOut` promptly.
#[cfg(unix)]
#[test]
fn cv_cmd_sink_timeout_via_adapter() {
    use pid_ctl::adapters::{CmdCvSink, CvSink};

    let mut sink = CmdCvSink::new("sleep 5".into(), Duration::from_millis(200), 2);
    let start = Instant::now();
    let err = sink.write_cv(1.0).expect_err("expected timeout");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "expected failure well before sleep 5s, took {:?}",
        start.elapsed()
    );
}
