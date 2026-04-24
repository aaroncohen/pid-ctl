//! Reliability & Operational Safety — `pid-ctl_plan.md` (numbered principles).

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use std::time::Duration;
use tempfile::tempdir;

/// Plan reliability §3: on PV read failure in `loop`, write `--safe-cv` when set; otherwise
/// do not emit CV (actuator holds last).
#[test]
fn pv_read_failure_invokes_safe_cv_or_hold_last_per_plan() {
    let dir = tempdir().expect("temporary directory");
    let cv_safe = dir.path().join("cv_safe.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "false"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_safe);
    cmd.args(["--safe-cv", "12.34"]);

    cmd.timeout(Duration::from_secs(3));
    let _ = cmd.output();

    let content = std::fs::read_to_string(&cv_safe).expect("read cv file after PV failure");
    assert!(
        content.contains("12.34"),
        "expected safe CV written on PV read failure; got {content:?}"
    );

    let dir_hold = tempdir().expect("temporary directory");
    let cv_hold = dir_hold.path().join("cv_hold.txt");
    std::fs::write(&cv_hold, "88.88\n").expect("seed cv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "false"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_hold);

    cmd.timeout(Duration::from_secs(3));
    let _ = cmd.output();

    let held = std::fs::read_to_string(&cv_hold).expect("read cv file");
    assert_eq!(
        held.trim(),
        "88.88",
        "without --safe-cv, PV failure must not overwrite CV (hold last)"
    );
}

/// Plan reliability item 17: CV write failure policy is mode-dependent — `once` exits 5 and
/// keeps `last_cv` at last confirmed-applied; `loop` exits 2 after `--cv-fail-after` consecutive
/// failures. (`pipe` follows Unix stdout/SIGPIPE conventions; not duplicated here.)
#[test]
fn cv_write_failure_policy_matches_mode_once_loop_pipe() {
    let dir = tempdir().expect("temporary directory");
    let state_path = dir.path().join("fan.json");
    let bad_cv = dir.path().join("missing").join("cv.txt");

    std::fs::write(
        &state_path,
        r#"{"schema_version":1,"last_cv":41.0,"iter":0}"#,
    )
    .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--kp",
        "2.0",
        "--out-min",
        "0.0",
        "--out-max",
        "100.0",
        "--cv-file",
    ]);
    cmd.arg(&bad_cv);
    cmd.arg("--state");
    cmd.arg(&state_path);

    cmd.assert().code(5);

    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("state json");
    assert_eq!(persisted["last_cv"].as_f64(), Some(41.0));

    let dir_loop = tempdir().expect("temporary directory");
    let pv_path = dir_loop.path().join("pv.txt");
    let cv_path = dir_loop.path().join("no_dir").join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_path);
    cmd.args(["--cv-fail-after", "2"]);
    cmd.timeout(Duration::from_secs(5));

    cmd.assert().code(2);
}

/// Beads pid-ctl-8vb.17: anomalous `dt` skips advance `updated_at` and persist `mark_dt_skipped`
/// semantics without advancing `iter`.
#[test]
fn loop_dt_skip_updates_state_timestamp_without_advancing_iter() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let state_path = dir.path().join("state.json");
    std::fs::write(&pv_path, "50.0\n").expect("write pv file");
    std::fs::write(&state_path, r#"{"schema_version":1,"iter":7,"i_acc":0.0}"#)
        .expect("seed state");

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
        "--min-dt",
        "1e9",
        "--cv-file",
        "/dev/null",
        "--state",
    ]);
    cmd.arg(&state_path);

    cmd.timeout(Duration::from_millis(400));
    let output = cmd.output().expect("run loop");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("skipping tick") && stderr.contains("min_dt"),
        "expected dt skip stderr; got: {stderr:?}"
    );

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");
    assert_eq!(
        snapshot.iter, 7,
        "iter must not advance when every tick is an out-of-range dt skip"
    );
    assert!(
        snapshot.updated_at.is_some(),
        "updated_at should be set after dt skips with --state"
    );
}

/// Beads pid-ctl-8vb.18 / plan §17: after PV failure, a successful `--safe-cv` write updates
/// persisted `last_cv` to the confirmed safe value.
#[test]
fn loop_pv_failure_safe_cv_updates_last_cv_in_state() {
    let dir = tempdir().expect("temporary directory");
    let state_path = dir.path().join("ctrl.json");
    let cv_path = dir.path().join("cv.txt");

    std::fs::write(
        &state_path,
        r#"{"schema_version":1,"last_cv":5.0,"iter":2,"i_acc":0.0}"#,
    )
    .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-cmd", "false"]);
    cmd.args(["--setpoint", "55.0", "--kp", "1.0"]);
    cmd.args(["--interval", "50ms"]);
    cmd.args(["--cv-file"]);
    cmd.arg(&cv_path);
    cmd.args(["--safe-cv", "12.34"]);
    cmd.arg("--state");
    cmd.arg(&state_path);

    cmd.timeout(Duration::from_millis(500));
    let _ = cmd.output();

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");
    assert_eq!(snapshot.last_cv, Some(12.34));
    assert_eq!(snapshot.iter, 2, "PV failure path must not advance iter");
}

/// Plan reliability §1: `once` emits a `cv_write_failed` JSON event to `--log` when CV write
/// fails (exit 5). Nothing fails quietly.
#[test]
fn once_cv_write_failure_emits_cv_write_failed_json_event() {
    use crate::helpers::assert_json_ts_iso8601_utc;

    let dir = tempdir().expect("temporary directory");
    let log_path = dir.path().join("events.ndjson");
    let bad_cv = dir.path().join("missing").join("cv.txt");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--kp",
        "2.0",
        "--cv-file",
    ]);
    cmd.arg(&bad_cv);
    cmd.arg("--log");
    cmd.arg(&log_path);

    cmd.assert().code(5);

    let log_contents = std::fs::read_to_string(&log_path).expect("read log file");
    let event_line = log_contents
        .lines()
        .find(|line| line.contains("\"cv_write_failed\""))
        .expect("expected a cv_write_failed JSON event line in log");

    let event: serde_json::Value =
        serde_json::from_str(event_line).expect("cv_write_failed line is valid JSON");

    assert_eq!(event["event"].as_str(), Some("cv_write_failed"));
    assert_eq!(event["consecutive_failures"].as_u64(), Some(1));
    assert_json_ts_iso8601_utc(&event);
}

/// Plan D-on-measurement: when `--kd > 0` and there is no prior PV (first tick),
/// a `d_term_skipped` structured event is emitted to `--log` with `reason:"no_pv_prev"`.
#[test]
fn d_term_skipped_no_pv_prev_emitted_to_log_on_first_tick() {
    let dir = tempdir().expect("temporary directory");
    let log_path = dir.path().join("events.ndjson");
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
        "--kd",
        "0.5",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);
    cmd.arg("--log");
    cmd.arg(&log_path);

    cmd.assert().success();

    let log_content = std::fs::read_to_string(&log_path).expect("log file should exist");
    let event: serde_json::Value = log_content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|v| v.get("event").and_then(|e| e.as_str()) == Some("d_term_skipped"))
        .expect("d_term_skipped event not found in log");

    assert_eq!(event["reason"].as_str(), Some("no_pv_prev"));
    assert_eq!(event["event"].as_str(), Some("d_term_skipped"));
    assert!(event.get("ts").and_then(|v| v.as_str()).is_some());
    assert_eq!(event["schema_version"].as_u64(), Some(1));
}
