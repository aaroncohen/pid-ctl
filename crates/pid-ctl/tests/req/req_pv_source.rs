//! PV source adapter tests — `--pv-file` and `--pv-cmd`.
//! Covers bead pid-ctl-nvs.

#[cfg(unix)]
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::str::starts_with;
use tempfile::tempdir;

/// `once --pv-file` reads the value from the file and computes the correct CV.
/// pv=50.5, sp=55.0, kp=1.0 → error=4.5 → CV=4.50
#[test]
fn once_pv_file_reads_value() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.5\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0", "--cv-stdout"]);

    cmd.assert().success().stdout(starts_with("4.50"));
}

/// `--pv-file` trims leading/trailing whitespace before parsing.
#[test]
fn once_pv_file_trims_whitespace() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "  50.5  \n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0", "--cv-stdout"]);

    cmd.assert().success().stdout(starts_with("4.50"));
}

/// `CmdPvSource` enforces a wall-clock bound: a long-running command returns
/// [`std::io::ErrorKind::TimedOut`] quickly (Reliability: bounded waits).
#[cfg(unix)]
#[test]
fn pv_cmd_read_respects_cmd_timeout() {
    use pid_ctl::adapters::{CmdPvSource, PvSource};

    let mut src = CmdPvSource::new("sleep 2".into(), Duration::from_millis(200));
    let start = Instant::now();
    let err = src.read_pv().expect_err("expected timeout");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "expected failure well before sleep 2s, took {:?}",
        start.elapsed()
    );
}

/// `once --pv-cmd` executes a command and uses stdout as PV.
/// `echo 50.5` → pv=50.5, sp=55.0, kp=1.0 → CV=4.50
#[test]
fn once_pv_cmd_reads_value() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-cmd",
        "echo 50.5",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    cmd.assert().success().stdout(starts_with("4.50"));
}

/// `once --pv-file` with a missing file exits with code 1.
#[test]
fn once_pv_file_missing_exits_1() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("nonexistent.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0", "--cv-stdout"]);

    cmd.assert().code(1);
}

/// `--pv-file` combined with `--scale` applies scaling before PID.
/// `raw_pv=50500`, `scale=0.001` → `pv=50.5`, `sp=55.0`, `kp=1.0` → `CV=4.50`
#[test]
fn pv_file_with_scale() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50500\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--scale",
        "0.001",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    cmd.assert().success().stdout(starts_with("4.50"));
}

/// `pipe --pv-file` is rejected with exit code 3 because pipe reads PV from stdin.
#[test]
fn pipe_rejects_pv_file() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.5\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.write_stdin("50.0\n");

    cmd.assert().code(3);
}

/// `once --pv-cmd-timeout` overrides the timeout for `--pv-cmd`.
/// A `sleep 10` command with a 300ms timeout should fail quickly (before 5s).
#[cfg(unix)]
#[test]
fn once_pv_cmd_timeout_overrides_cmd_timeout() {
    let start = Instant::now();

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-cmd",
        "sleep 10",
        "--pv-cmd-timeout",
        "0.3",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    // Should fail (PV read failed → exit 1) well before the 10-second sleep completes.
    cmd.assert().code(1);

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "expected timeout well before sleep 10s, took {:?}",
        start.elapsed()
    );
}

/// Without `--pv-cmd-timeout`, PV commands use `--cmd-timeout`.
/// A `sleep 10` with `--cmd-timeout 0.3` should fail quickly.
#[cfg(unix)]
#[test]
fn once_pv_cmd_uses_cmd_timeout_when_pv_cmd_timeout_absent() {
    let start = Instant::now();

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-cmd",
        "sleep 10",
        "--cmd-timeout",
        "0.3",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    // Should fail (PV read failed → exit 1) well before the 10-second sleep completes.
    cmd.assert().code(1);

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "expected timeout well before sleep 10s, took {:?}",
        start.elapsed()
    );
}

/// `--pv-cmd-timeout` with an invalid value exits with code 3.
#[test]
fn pv_cmd_timeout_invalid_value_exits_3() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-cmd",
        "echo 50.0",
        "--pv-cmd-timeout",
        "not-a-number",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    cmd.assert().code(3);
}

/// `loop --pv-cmd-timeout` is accepted by the parser (not rejected as unknown flag).
/// This verifies the flag is wired up for loop mode — end-to-end behavior is covered
/// by the once-mode tests since loop retries PV failures rather than exiting.
#[test]
fn loop_pv_cmd_timeout_flag_accepted() {
    use tempfile::tempdir;
    let dir = tempdir().expect("temporary directory");
    let cv_path = dir.path().join("cv.txt");

    // A valid float PV cmd with short timeout: flag should be accepted (no exit 3).
    // We kill the loop early via timeout — the important thing is it doesn't exit 3.
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--pv-cmd",
        "echo 50.0",
        "--pv-cmd-timeout",
        "2.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "100ms",
        "--cv-file",
        cv_path.to_str().unwrap(),
    ]);
    cmd.timeout(Duration::from_millis(400));

    // Process will be killed by timeout — that's expected.
    // The key assertion is that exit is NOT 3 (which would mean "unrecognized option").
    let output = cmd.output().expect("command ran");
    assert_ne!(
        output.status.code(),
        Some(3),
        "--pv-cmd-timeout should be recognized (not exit 3 'unrecognized option')"
    );
}

/// `--pv-cmd-timeout` does not affect `--cv-cmd-timeout` — they are independent.
/// CV command with a generous timeout and PV command that is fast: both should succeed.
#[cfg(unix)]
#[test]
fn pv_cmd_timeout_independent_of_cv_cmd_timeout() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-cmd",
        "echo 50.0",
        "--pv-cmd-timeout",
        "5.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-cmd",
        "echo {cv}",
        "--cv-cmd-timeout",
        "5.0",
    ]);

    cmd.assert().success();
}

/// Specifying both `--pv` and `--pv-file` is rejected with exit code 3.
#[test]
fn multiple_pv_sources_rejected() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    std::fs::write(&pv_path, "50.5\n").expect("write pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["once", "--pv", "50.0", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0", "--cv-stdout"]);

    cmd.assert()
        .code(3)
        .stderr(predicates::str::contains("only one PV source"));
}
