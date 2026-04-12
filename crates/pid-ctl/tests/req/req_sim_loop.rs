//! Closed-loop smoke: `pid-ctl loop` drives `pid-ctl-sim` via `--pv-cmd` / `--cv-cmd`.
//!
//! Requires `cargo build -p pid-ctl-sim` (or `cargo build --workspace`) so `target/debug/pid-ctl-sim` exists.

use assert_cmd::Command;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

/// `target/debug/<name>` — same directory as this test binary and `pid-ctl`.
fn target_debug_bin(name: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // deps
    p.pop(); // debug
    p.join(name)
}

#[test]
fn loop_pid_ctl_sim_smoke_no_crash() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    assert!(
        sim_bin.exists(),
        "build pid-ctl-sim first so {} exists (cargo build -p pid-ctl-sim)",
        sim_bin.display()
    );

    let dir = tempdir().expect("tempdir");
    let plant = dir.path().join("plant.json");

    Command::new(&sim_bin)
        .args([
            "init",
            "--state",
            plant.to_str().expect("utf8"),
            "--plant",
            "first-order",
            "--param",
            "tau=0.5",
            "--param",
            "gain=1.0",
        ])
        .assert()
        .success();

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl");
    cmd.args(["loop", "--pv-cmd"]);
    cmd.arg(format!(
        "{} print-pv --state {}",
        sim_bin.display(),
        plant.display()
    ));
    cmd.args(["--cv-cmd"]);
    cmd.arg(format!(
        "{} apply-cv --state {} --dt 0.2 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    ));
    cmd.args([
        "--interval",
        "200ms",
        "--setpoint",
        "1.0",
        "--kp",
        "0.8",
        "--ki",
        "0.2",
        "--kd",
        "0.0",
    ]);

    cmd.timeout(Duration::from_secs(2));
    let output = cmd.output().expect("spawn loop");
    // Killed by timeout — expect non-success exit on some platforms.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("PV read failed") && !stderr.contains("CV write failed"),
        "unexpected I/O errors: stderr={stderr:?}"
    );
}
