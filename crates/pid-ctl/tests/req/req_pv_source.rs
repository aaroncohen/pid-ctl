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
