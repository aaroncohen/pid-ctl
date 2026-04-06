//! `--tune` flag validation — `pid-ctl_plan.md` (Incompatible Flag Combinations, Tuning Dashboard).

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn minimal_loop_tune_args(dir: &std::path::Path) -> Vec<String> {
    let pv = dir.join("pv.txt");
    std::fs::write(&pv, "50.0\n").expect("pv file");
    vec![
        "loop".into(),
        "--pv-file".into(),
        pv.to_string_lossy().into_owned(),
        "--setpoint".into(),
        "55.0".into(),
        "--kp".into(),
        "1.0".into(),
        "--ki".into(),
        "0.0".into(),
        "--kd".into(),
        "0.0".into(),
        "--interval".into(),
        "10s".into(),
        "--dry-run".into(),
        "--tune".into(),
    ]
}

#[test]
fn tune_rejects_format_json() {
    let dir = tempdir().expect("tempdir");
    let mut args = minimal_loop_tune_args(dir.path());
    args.push("--format".into());
    args.push("json".into());

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(&args);

    cmd.assert()
        .code(3)
        .stderr(contains("--tune and --format json are incompatible"));
}

#[test]
fn tune_rejects_quiet() {
    let dir = tempdir().expect("tempdir");
    let mut args = minimal_loop_tune_args(dir.path());
    args.push("--quiet".into());

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(&args);

    cmd.assert()
        .code(3)
        .stderr(contains("--tune and --quiet are incompatible"));
}

#[test]
fn tune_rejects_pv_stdin() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "loop",
        "--pv-stdin",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ki",
        "0.0",
        "--kd",
        "0.0",
        "--interval",
        "10s",
        "--dry-run",
        "--tune",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("--tune cannot be used with --pv-stdin"));
}

#[test]
fn tune_rejected_on_once_with_plan_message() {
    let dir = tempdir().expect("tempdir");
    let pv = dir.path().join("pv.txt");
    std::fs::write(&pv, "50.0\n").expect("pv");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv-file",
        pv.to_str().expect("utf8"),
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ki",
        "0.0",
        "--kd",
        "0.0",
        "--dry-run",
        "--tune",
    ]);

    cmd.assert()
        .code(3)
        .stderr(contains("--tune requires loop"));
}

#[test]
fn tune_rejected_on_pipe_with_plan_message() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "pipe",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--ki",
        "0.0",
        "--kd",
        "0.0",
        "--tune",
    ]);

    cmd.assert().code(3).stderr(contains(
        "--tune is unavailable with pipe — pipe is a pure stdin→stdout transformer in v1",
    ));
}

#[test]
fn tune_history_flag_parses() {
    let dir = tempdir().expect("tempdir");
    let mut args = minimal_loop_tune_args(dir.path());
    let _ = args.pop(); // drop trailing `--tune`
    args.extend(["--tune-history".into(), "120".into(), "--tune".into()]);

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(&args);
    cmd.timeout(std::time::Duration::from_millis(200));

    // Without a TTY the dashboard exits immediately after validation with the TTY error.
    cmd.assert()
        .code(3)
        .stderr(contains("--tune requires a TTY"));
}
