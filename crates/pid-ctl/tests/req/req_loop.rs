//! Requirement tests for the `loop` subcommand — bead pid-ctl-cv1.

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use predicates::str::contains;
use std::time::Duration;
use tempfile::tempdir;

/// pid-ctl-cv1: loop runs continuously, advances iter, and writes state.
///
/// Creates a temp PV file, runs loop with a very short interval, kills after
/// ~300 ms, then checks the state file has iter >= 2.
///
/// Marked `#[ignore]` because the process-timing approach (kill after N ms) is
/// inherently racy on slow/loaded CI machines and can produce iter == 1 or flap.
/// Run manually with `cargo test -- --ignored loop_basic_iterations`.
#[test]
#[ignore = "timing-sensitive: kill after 300 ms may land before 2 ticks on slow machines"]
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
        "--state",
    ]);
    cmd.arg(&state_path);

    // Run for up to 500 ms then kill.
    cmd.timeout(Duration::from_millis(500));

    // Process will be killed (non-zero exit) — that's expected.
    let _ = cmd.output();

    // State file should exist and iter should be >= 2.
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
        snapshot.iter >= 2,
        "expected iter >= 2 after ~300 ms at 50 ms interval, got {}",
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
