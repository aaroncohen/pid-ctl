//! Stdout / `--format json` incompatibility — `pid-ctl_plan.md` (Output & Logging, Incompatible Flag Combinations).

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn pipe_rejects_format_json() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--format", "json", "--setpoint", "55.0"]);
    cmd.write_stdin("54.2\n");

    cmd.assert().code(3).stderr(contains(
        "--format json writes to stdout, which conflicts with pipe's CV output — use --log for machine-readable telemetry",
    ));
}

#[test]
fn cv_stdout_rejects_format_json() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "54.2",
        "--setpoint",
        "55.0",
        "--cv-stdout",
        "--format",
        "json",
    ]);

    cmd.assert().code(3).stderr(contains(
        "--format json writes to stdout, which conflicts with --cv-stdout — use --log for machine-readable telemetry",
    ));
}

/// pid-ctl-4x5: loop --format json --cv-stdout must be rejected (exit 3).
/// Both write to stdout causing corrupted output.
#[test]
fn loop_cv_stdout_rejects_format_json() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--pv-file",
        "/dev/null",
        "--cv-stdout",
        "--format",
        "json",
        "--setpoint",
        "55.0",
        "--interval",
        "1s",
    ]);

    cmd.assert().code(3).stderr(contains(
        "--format json and --cv-stdout are incompatible",
    ));
}
