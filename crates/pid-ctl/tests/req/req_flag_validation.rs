//! Flag validation and incompatible combination enforcement — `pid-ctl_plan.md`
//! (Incompatible Flag Combinations, PV Source, Scaling & Filtering).

use assert_cmd::Command;
use predicates::str::contains;

// --- Bugs: these test plan-required rejections that the CLI currently fails to enforce ---

/// pid-ctl-s4z: pipe + --pv must exit 3.
/// Plan: "pipe reads PV from stdin intrinsically — PV source flags are not accepted"
/// lists --pv alongside --pv-file, --pv-cmd, --pv-stdin.
#[test]
fn pipe_rejects_pv_literal_flag() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--pv", "50.0", "--setpoint", "55.0"]);
    cmd.write_stdin("54.2\n");

    cmd.assert().code(3).stderr(contains(
        "pipe reads PV from stdin intrinsically — PV source flags are not accepted",
    ));
}

/// pid-ctl-yao: duplicate --pv flags must exit 3.
/// Plan: "Specifying more than one PV source is exit 3: only one PV source may be specified"
#[test]
fn duplicate_pv_flags_rejected() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--cv-stdout",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("only one PV source may be specified"));
}

// --- Unimplemented: --scale and --cv-precision (pid-ctl-7q1) ---

/// pid-ctl-7q1: --scale multiplies raw PV before filtering or PID.
#[test]
fn scale_multiplies_raw_pv_before_pid() {
    // --pv 50000 --scale 0.001 => effective PV 50.0, error = 55.0 - 50.0 = 5.0
    // kp=1.0 => CV = 5.0
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50000",
        "--scale",
        "0.001",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--cv-stdout",
    ]);

    cmd.assert()
        .success()
        .stdout(predicates::str::starts_with("5"));
}

/// pid-ctl-7q1: --cv-precision controls decimal places in CV output.
#[test]
fn cv_precision_controls_output_decimal_places() {
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
        "--cv-precision",
        "0",
    ]);

    cmd.assert().success().stdout("5\n");
}
