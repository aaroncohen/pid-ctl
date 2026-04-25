//! Flag validation and incompatible combination enforcement — `pid-ctl_plan.md`
//! (Incompatible Flag Combinations, PV Source, Scaling & Filtering).

use assert_cmd::Command;
use predicates::str::contains;

// --- CLI validation: plan-required rejections enforced at parse time ---

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

// --- PV/CV formatting: --scale and --cv-precision (pid-ctl-7q1) ---

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

/// pid-ctl-8vb.7: pipe rejects --cv-file — pipe always writes CV to stdout.
#[test]
fn pipe_rejects_cv_file() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--cv-file", "/tmp/x"]);
    cmd.write_stdin("54.0\n");

    cmd.assert()
        .code(3)
        .stderr(contains("pipe always writes CV to stdout"));
}

/// pid-ctl-8vb.7: specifying both --cv-stdout and --cv-file is rejected with exit 3.
#[test]
fn multiple_cv_sinks_rejected() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--cv-stdout",
        "--cv-file",
        "/tmp/x",
    ]);

    cmd.assert().code(3).stderr(contains("only one CV sink"));
}

/// pid-ctl-8vb.7: pipe rejects --dry-run with exit 3.
#[test]
fn pipe_rejects_dry_run() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--dry-run"]);
    cmd.write_stdin("54.0\n");

    cmd.assert()
        .code(3)
        .stderr(contains("not meaningful with pipe"));
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

/// pid-ctl-s0a: --anti-windup clamp is accepted.
#[test]
fn anti_windup_clamp_accepted() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ki",
        "0.1",
        "--anti-windup",
        "clamp",
        "--cv-stdout",
    ]);

    cmd.assert().success();
}

/// pid-ctl-s0a: --anti-windup none is accepted.
#[test]
fn anti_windup_none_accepted() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--anti-windup",
        "none",
        "--cv-stdout",
    ]);

    cmd.assert().success();
}

/// pid-ctl-s0a: --anti-windup back-calc is accepted.
#[test]
fn anti_windup_back_calc_accepted() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--anti-windup",
        "back-calc",
        "--cv-stdout",
    ]);

    cmd.assert().success();
}

/// pid-ctl-s0a: --anti-windup with invalid value is rejected with exit 3.
#[test]
fn anti_windup_invalid_value_rejected() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--anti-windup",
        "invalid",
        "--cv-stdout",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("--anti-windup must be"));
}

/// pid-ctl-s0a: --anti-windup-tt sets explicit Tt value.
#[test]
fn anti_windup_tt_accepted() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ki",
        "0.1",
        "--anti-windup-tt",
        "2.5",
        "--cv-stdout",
    ]);

    cmd.assert().success();
}

/// pid-ctl-1e3: --ramp-rate is accepted as alias for --slew-rate.
#[test]
fn ramp_rate_accepted_as_slew_rate_alias() {
    // --ramp-rate 5 limits CV change to 5/s; with dt=1 and error=10 the unclamped
    // CV would be 10 but slew-rate caps it at 5 (first iteration from prev_cv=0).
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "45.0",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ramp-rate",
        "5.0",
        "--cv-stdout",
        "--cv-precision",
        "1",
    ]);

    cmd.assert().success().stdout("5.0\n");
}

/// pid-ctl-i02: --verbose flag is accepted without error (loop with dry-run).
#[test]
fn verbose_flag_accepted() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--pv-file",
        "/dev/null",
        "--cv-stdout",
        "--setpoint",
        "55.0",
        "--interval",
        "1s",
        "--verbose",
        "--dry-run",
        "--fail-after",
        "1",
    ]);

    // Will fail after 1 PV read failure (exit 2), but should not exit 3 (bad flag).
    cmd.assert().code(2);
}
