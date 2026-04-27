//! Integration tests for `pid-ctl replay`.
//!
//! Tests verify the CLI contracts (exit codes, JSON output, round-trip
//! determinism, diff summary) without inspecting PID core internals.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::PathBuf;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn target_debug_bin(name: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // deps
    p.pop(); // debug
    p.join(name)
}

/// Build a minimal NDJSON iteration-record log in a string.
fn make_log(records: &[(u64, f64, f64, f64)]) -> String {
    // Each element: (iter, pv, sp, dt)
    records
        .iter()
        .map(|&(iter, pv, sp, dt)| {
            format!(
                r#"{{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":{iter},"pv":{pv},"sp":{sp},"err":{},"p":0.0,"i":0.0,"d":0.0,"cv":0.0,"i_acc":0.0,"dt":{dt}}}"#,
                sp - pv,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Validation: reject misconfig (exit 3)
// ---------------------------------------------------------------------------

#[test]
fn replay_rejects_missing_log() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args(["replay", "--kp", "1.0", "--ki", "0.0", "--kd", "0.0"])
        .assert()
        .code(3);
}

#[test]
fn replay_rejects_missing_kp() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    std::fs::write(&log, make_log(&[(1, 1.0, 2.0, 0.5)])).expect("write log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .code(3);
}

#[test]
fn replay_rejects_diff_with_output_log() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    let out = dir.path().join("out.ndjson");
    std::fs::write(&log, make_log(&[(1, 1.0, 2.0, 0.5)])).expect("write log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
            "--diff",
            "--output-log",
            out.to_str().expect("utf8"),
        ])
        .assert()
        .code(3)
        .stderr(contains("mutually exclusive"));
}

#[test]
fn replay_rejects_nonexistent_log() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            "/tmp/does_not_exist_pid_ctl_test.ndjson",
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .code(1)
        .stderr(contains("cannot open log"));
}

#[test]
fn replay_rejects_malformed_json_in_log() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    std::fs::write(&log, "this is not json\n").expect("write log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .code(1)
        .stderr(contains("not valid JSON"));
}

#[test]
fn replay_rejects_iteration_record_missing_pv() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    // Has "iter" but no "pv" — must be flagged as invalid.
    std::fs::write(
        &log,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":1,"sp":1.0,"dt":0.5,"cv":0.0,"i_acc":0.0}"#,
    )
    .expect("write log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .code(1)
        .stderr(contains("missing `pv`"));
}

#[test]
fn replay_rejects_iteration_record_missing_dt() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    std::fs::write(
        &log,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":1,"pv":1.0,"sp":1.0,"cv":0.0,"i_acc":0.0}"#,
    )
    .expect("write log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .code(1)
        .stderr(contains("missing `dt`"));
}

// ---------------------------------------------------------------------------
// Skipping: non-iteration event lines are silently ignored
// ---------------------------------------------------------------------------

#[test]
fn replay_skips_event_lines() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    // Mix of event lines (with "event" field) and one valid iteration record.
    let content = [
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","event":"socket_ready","path":"/tmp/ctl.sock"}"#,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:01Z","iter":1,"pv":1.0,"sp":2.0,"err":1.0,"p":1.0,"i":0.0,"d":0.0,"cv":1.0,"i_acc":0.0,"dt":1.0}"#,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:02Z","event":"gains_changed","kp":1.0,"ki":0.0,"kd":0.0,"sp":2.0,"iter":1,"source":"socket"}"#,
    ]
    .join("\n");
    std::fs::write(&log, &content).expect("write log");

    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("utf8");
    let lines: Vec<_> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected exactly 1 replayed record");
    let v: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    assert_eq!(v["iter"], 1);
}

// ---------------------------------------------------------------------------
// Round-trip: same gains → numerically identical CV stream
// ---------------------------------------------------------------------------

/// Produce a log via `pid-ctl-sim`, then replay with the same gains and assert
/// the replayed CV stream matches the original within floating-point tolerance.
#[test]
fn replay_roundtrip_same_gains_matches_original() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    if !sim_bin.exists() {
        eprintln!("skip: build pid-ctl-sim first ({})", sim_bin.display());
        return;
    }

    let dir = tempdir().expect("tempdir");
    let plant = dir.path().join("plant.json");
    let log_path = dir.path().join("run.ndjson");
    let replay_log = dir.path().join("replayed.ndjson");

    // Initialise a fast first-order plant.
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

    let pv_cmd = format!("{} print-pv --state {}", sim_bin.display(), plant.display());
    let cv_cmd = format!(
        "{} apply-cv --state {} --dt 0.1 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    );

    // Run 10 iterations with kp=0.8, ki=0.1, kd=0.0 and capture the log.
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "loop",
            "--pv-cmd",
            &pv_cmd,
            "--cv-cmd",
            &cv_cmd,
            "--interval",
            "100ms",
            "--setpoint",
            "1.0",
            "--kp",
            "0.8",
            "--ki",
            "0.1",
            "--kd",
            "0.0",
            "--max-iterations",
            "10",
            "--log",
            log_path.to_str().expect("utf8"),
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    assert!(log_path.exists(), "log file not created");

    // Replay with exactly the same gains, writing to a file.
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log_path.to_str().expect("utf8"),
            "--kp",
            "0.8",
            "--ki",
            "0.1",
            "--kd",
            "0.0",
            "--output-log",
            replay_log.to_str().expect("utf8"),
        ])
        .assert()
        .success();

    // Parse both logs and compare CV values.
    let original_cvs = parse_cv_values(&log_path);
    let replayed_cvs = parse_cv_values(&replay_log);

    assert_eq!(
        original_cvs.len(),
        replayed_cvs.len(),
        "record count mismatch: original={}, replayed={}",
        original_cvs.len(),
        replayed_cvs.len()
    );

    for (i, (orig, replay)) in original_cvs.iter().zip(replayed_cvs.iter()).enumerate() {
        let diff = (orig - replay).abs();
        assert!(
            diff < 1e-9,
            "CV mismatch at iter {i}: original={orig}, replayed={replay}, diff={diff}"
        );
    }
}

// ---------------------------------------------------------------------------
// Different gains: deterministic, different output
// ---------------------------------------------------------------------------

#[test]
fn replay_different_gains_produces_different_cv() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    if !sim_bin.exists() {
        eprintln!("skip: build pid-ctl-sim first ({})", sim_bin.display());
        return;
    }

    let dir = tempdir().expect("tempdir");
    let plant = dir.path().join("plant.json");
    let log_path = dir.path().join("run.ndjson");

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

    let pv_cmd = format!("{} print-pv --state {}", sim_bin.display(), plant.display());
    let cv_cmd = format!(
        "{} apply-cv --state {} --dt 0.1 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    );

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "loop",
            "--pv-cmd",
            &pv_cmd,
            "--cv-cmd",
            &cv_cmd,
            "--interval",
            "100ms",
            "--setpoint",
            "1.0",
            "--kp",
            "0.8",
            "--ki",
            "0.1",
            "--kd",
            "0.0",
            "--max-iterations",
            "10",
            "--log",
            log_path.to_str().expect("utf8"),
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    // Replay with higher kp — CVs should differ from the original.
    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log_path.to_str().expect("utf8"),
            "--kp",
            "2.0",
            "--ki",
            "0.1",
            "--kd",
            "0.0",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let replay_cvs: Vec<f64> = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["cv"].as_f64())
        .collect();

    let original_cvs = parse_cv_values(&log_path);

    // With different gains, at least one CV should differ.
    let any_differs = original_cvs
        .iter()
        .zip(replay_cvs.iter())
        .any(|(a, b)| (a - b).abs() > 1e-9);
    assert!(
        any_differs,
        "replayed CVs should differ from original when gains change"
    );
}

// ---------------------------------------------------------------------------
// --diff mode
// ---------------------------------------------------------------------------

#[test]
fn replay_diff_mode_prints_json_summary() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");

    // Craft a log with known cv values so we can verify the diff stats.
    // Two records: sp=2.0, pv=1.0, cv=1.0 (with kp=1.0)
    let content = [
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":1,"pv":1.0,"sp":2.0,"err":1.0,"p":1.0,"i":0.0,"d":0.0,"cv":1.0,"i_acc":0.0,"dt":1.0}"#,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:01Z","iter":2,"pv":1.0,"sp":2.0,"err":1.0,"p":1.0,"i":0.0,"d":0.0,"cv":1.0,"i_acc":0.0,"dt":1.0}"#,
    ]
    .join("\n");
    std::fs::write(&log, &content).expect("write log");

    // Replay with kp=2.0 — replayed cv = 2.0, original cv = 1.0, diff = 1.0.
    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "2.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
            "--diff",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("diff output is JSON");
    assert_eq!(v["n"].as_u64().expect("n"), 2);
    let max_diff = v["max_cv_diff"].as_f64().expect("max_cv_diff");
    assert!(
        (max_diff - 1.0).abs() < 1e-9,
        "expected max_cv_diff≈1.0, got {max_diff}"
    );
    let rms_diff = v["rms_cv_diff"].as_f64().expect("rms_cv_diff");
    assert!(
        (rms_diff - 1.0).abs() < 1e-9,
        "expected rms_cv_diff≈1.0, got {rms_diff}"
    );
}

#[test]
fn replay_diff_mode_zero_diff_for_same_gains() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");

    // With kp=1.0, ki=0.0, kd=0.0 and pv=1.0, sp=2.0, dt=1.0:
    // cv = kp * (sp - pv) = 1.0 * 1.0 = 1.0
    let content = r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":1,"pv":1.0,"sp":2.0,"err":1.0,"p":1.0,"i":0.0,"d":0.0,"cv":1.0,"i_acc":0.0,"dt":1.0}"#;
    std::fs::write(&log, content).expect("write log");

    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
            "--diff",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("utf8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("diff output is JSON");
    assert_eq!(v["n"].as_u64().expect("n"), 1);
    let max_diff = v["max_cv_diff"].as_f64().expect("max_cv_diff");
    assert!(
        max_diff < 1e-9,
        "expected max_cv_diff≈0 for same gains, got {max_diff}"
    );
}

// ---------------------------------------------------------------------------
// Output format: replayed records have the expected NDJSON fields
// ---------------------------------------------------------------------------

#[test]
fn replay_output_has_required_fields() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");

    std::fs::write(
        &log,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":42,"pv":3.0,"sp":5.0,"err":2.0,"p":2.0,"i":0.0,"d":0.0,"cv":2.0,"i_acc":0.0,"dt":0.5}"#,
    )
    .expect("write log");

    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.5",
            "--kd",
            "0.0",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("utf8");
    let line = stdout.trim();
    assert!(!line.is_empty(), "expected a replayed record");
    let v: serde_json::Value = serde_json::from_str(line).expect("replayed record is JSON");

    // Required fields.
    assert!(v["schema_version"].is_number(), "schema_version missing");
    assert!(v["ts"].is_string(), "ts missing");
    assert_eq!(v["iter"], 42, "iter should be preserved from original log");
    assert!((v["pv"].as_f64().expect("pv") - 3.0).abs() < 1e-9, "pv");
    assert!((v["sp"].as_f64().expect("sp") - 5.0).abs() < 1e-9, "sp");
    assert!(v["cv"].is_number(), "cv missing");
    assert!(v["i_acc"].is_number(), "i_acc missing");
    assert!(v["dt"].is_number(), "dt missing");
    assert!(v["err"].is_number(), "err missing");
    assert!(v["p"].is_number(), "p missing");
    assert!(v["i"].is_number(), "i missing");
    assert!(v["d"].is_number(), "d missing");
}

// ---------------------------------------------------------------------------
// --output-log: writes to file instead of stdout
// ---------------------------------------------------------------------------

#[test]
fn replay_output_log_writes_to_file() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    let out = dir.path().join("out.ndjson");

    std::fs::write(
        &log,
        r#"{"schema_version":1,"ts":"2024-01-01T00:00:00Z","iter":1,"pv":1.0,"sp":2.0,"err":1.0,"p":1.0,"i":0.0,"d":0.0,"cv":1.0,"i_acc":0.0,"dt":1.0}"#,
    )
    .expect("write log");

    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
            "--output-log",
            out.to_str().expect("utf8"),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // stdout should be empty when --output-log is set.
    let stdout = String::from_utf8(output).expect("utf8");
    assert!(
        stdout.trim().is_empty(),
        "stdout should be empty when --output-log is used, got {stdout:?}"
    );

    // File should contain the replayed record.
    assert!(out.exists(), "output log not created");
    let contents = std::fs::read_to_string(&out).expect("read output log");
    let v: serde_json::Value =
        serde_json::from_str(contents.trim()).expect("output log is valid JSON");
    assert_eq!(v["iter"], 1);
}

// ---------------------------------------------------------------------------
// Empty log: graceful exit 0 with no output
// ---------------------------------------------------------------------------

#[test]
fn replay_empty_log_exits_ok() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log.ndjson");
    std::fs::write(&log, "").expect("write empty log");

    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "replay",
            "--log",
            log.to_str().expect("utf8"),
            "--kp",
            "1.0",
            "--ki",
            "0.0",
            "--kd",
            "0.0",
        ])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_cv_values(log: &std::path::Path) -> Vec<f64> {
    let contents = std::fs::read_to_string(log).expect("read log");
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("event").is_none())
        .filter_map(|v| v["cv"].as_f64())
        .collect()
}
