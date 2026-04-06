//! CLI integration tests for `pid-ctl-sim` (init / print-pv / apply-cv).

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use pid_ctl_sim::{load_state, Plant, SimState, SCHEMA_VERSION};
use tempfile::tempdir;

#[test]
fn init_print_apply_roundtrip_first_order() {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("p.json");

    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "init",
            "--state",
            state.to_str().expect("utf8"),
            "--plant",
            "first-order",
            "--param",
            "tau=1.0",
            "--param",
            "gain=2.0",
            "--param",
            "x=0.0",
        ])
        .assert()
        .success();

    let s = load_state(&state).expect("load");
    assert_eq!(s.schema_version, SCHEMA_VERSION);
    assert!(matches!(s.plant, Plant::FirstOrder { .. }));

    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "print-pv",
            "--state",
            state.to_str().expect("utf8"),
        ])
        .assert()
        .success()
        .stdout("0\n");

    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "apply-cv",
            "--state",
            state.to_str().expect("utf8"),
            "--dt",
            "0.1",
            "--cv",
            "1.0",
        ])
        .assert()
        .success();

    let out = Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "print-pv",
            "--state",
            state.to_str().expect("utf8"),
        ])
        .output()
        .expect("print-pv");
    assert!(out.status.success());
    let pv: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("pv float");
    assert!(pv > 0.15 && pv < 0.25, "unexpected pv {pv}");
}

/// Negative CV must parse after `--cv` (pid-ctl substitutes `{cv}` → e.g. `-0.03` for shell/clap).
#[test]
fn apply_cv_accepts_negative_value() {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("neg.json");
    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "init",
            "--state",
            state.to_str().expect("utf8"),
            "--plant",
            "first-order",
        ])
        .assert()
        .success();

    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "apply-cv",
            "--state",
            state.to_str().expect("utf8"),
            "--dt",
            "0.5",
            "--cv",
            "-0.03",
        ])
        .assert()
        .success();
}

#[test]
fn rejects_bad_schema_version() {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("bad.json");
    let sim = SimState {
        schema_version: 999,
        plant: Plant::FirstOrder {
            params: pid_ctl_sim::FirstOrderParams { tau: 1.0, gain: 1.0 },
            x: 0.0,
        },
    };
    pid_ctl_sim::save_state(&state, &sim).expect("save");

    Command::new(cargo_bin("pid-ctl-sim"))
        .args([
            "print-pv",
            "--state",
            state.to_str().expect("utf8"),
        ])
        .assert()
        .failure();
}
