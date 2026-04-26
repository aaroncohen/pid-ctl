//! Social (integration-point) tests for `pid-ctl replay`.
//! These tests drive the CLI binary and verify observable contracts:
//! exit codes, stdout content, output-log NDJSON structure, and diff summaries.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a minimal valid NDJSON log with the given PV/cv/sp values and dt=1.0.
/// Each line is an iteration record with all fields `replay` needs.
fn make_ndjson_log(records: &[(f64, f64, f64)]) -> String {
    // (pv, cv, sp)
    records
        .iter()
        .enumerate()
        .map(|(i, (pv, cv, sp))| {
            format!(
                r#"{{"schema_version":1,"ts":"2026-01-01T00:00:00Z","iter":{iter},"pv":{pv},"sp":{sp},"err":{err},"p":{cv},"i":0.0,"d":0.0,"cv":{cv},"i_acc":0.0,"dt":1.0}}"#,
                iter = i + 1,
                pv = pv,
                sp = sp,
                err = sp - pv,
                cv = cv,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

// ---------------------------------------------------------------------------
// Round-trip: same gains → numerically identical CV
// ---------------------------------------------------------------------------

/// Replaying a log with the exact same gains must produce the same CV values,
/// within floating-point precision (issue §Verification).
#[test]
fn replay_round_trip_same_gains_produces_identical_cv() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("replayed.ndjson");

    // Generate log via `pipe --dt 1.0` so dt is fixed.
    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args([
            "pipe",
            "--setpoint",
            "55.0",
            "--kp",
            "1.0",
            "--ki",
            "0.1",
            "--kd",
            "0.0",
            "--dt",
            "1.0",
            "--log",
        ])
        .arg(&log_path)
        .write_stdin("50.0\n51.0\n52.0\n53.0\n")
        .assert()
        .success();

    // Replay with the same gains.
    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--ki", "0.1", "--kd", "0.0", "--output-log"])
        .arg(&out_path)
        .assert()
        .success();

    // Parse both logs and compare CV values.
    let original = std::fs::read_to_string(&log_path).expect("read source log");
    let replayed = std::fs::read_to_string(&out_path).expect("read replayed log");

    let orig_cvs: Vec<f64> = original
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON"))
        .filter(|v| v.get("event").is_none())
        .map(|v| v["cv"].as_f64().expect("cv field"))
        .collect();

    let replay_cvs: Vec<f64> = replayed
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON"))
        .filter(|v| v.get("event").is_none())
        .map(|v| v["cv"].as_f64().expect("cv field"))
        .collect();

    assert_eq!(orig_cvs.len(), replay_cvs.len(), "record counts must match");
    assert!(!orig_cvs.is_empty(), "must have at least one record");

    for (i, (oc, rc)) in orig_cvs.iter().zip(replay_cvs.iter()).enumerate() {
        let diff = (oc - rc).abs();
        assert!(
            diff <= f64::EPSILON * oc.abs().max(1.0),
            "tick {}: orig_cv={oc} replayed_cv={rc} diff={diff} exceeds 1 ULP",
            i + 1
        );
    }
}

// ---------------------------------------------------------------------------
// Different gains → deterministic, reproducible output
// ---------------------------------------------------------------------------

/// Replaying with different gains must be deterministic: two runs with the same
/// input produce bit-identical output files.
#[test]
fn replay_different_gains_is_deterministic() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out1 = dir.path().join("replay1.ndjson");
    let out2 = dir.path().join("replay2.ndjson");

    // Build a fixed log directly (no wall-clock dt involved).
    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0), (51.0, 4.0, 55.0), (52.0, 3.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    for out in [&out1, &out2] {
        Command::cargo_bin("pid-ctl")
            .unwrap()
            .args(["replay", "--log"])
            .arg(&log_path)
            .args(["--kp", "1.5", "--ki", "0.05", "--kd", "0.0", "--output-log"])
            .arg(out)
            .assert()
            .success();
    }

    let content1 = std::fs::read_to_string(&out1).unwrap();
    let content2 = std::fs::read_to_string(&out2).unwrap();

    // cv and i_acc must be identical across both runs.
    let cvs1 = extract_field_f64s(&content1, "cv");
    let cvs2 = extract_field_f64s(&content2, "cv");
    assert_eq!(cvs1, cvs2, "cv values must be identical across replay runs");

    let i_accs1 = extract_field_f64s(&content1, "i_acc");
    let i_accs2 = extract_field_f64s(&content2, "i_acc");
    assert_eq!(
        i_accs1, i_accs2,
        "i_acc must be identical across replay runs"
    );
}

fn extract_field_f64s(ndjson: &str, field: &str) -> Vec<f64> {
    ndjson
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON"))
        .filter(|v| v.get("event").is_none())
        .map(|v| v[field].as_f64().expect("numeric field"))
        .collect()
}

// ---------------------------------------------------------------------------
// Output log NDJSON structure
// ---------------------------------------------------------------------------

/// `--output-log` writes valid NDJSON with all required fields per record.
#[test]
fn replay_output_log_has_required_fields() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("out.ndjson");

    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0), (51.0, 4.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--output-log"])
        .arg(&out_path)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out_path).expect("read output log");
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(lines.len(), 2, "expected 2 output records");

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        for field in [
            "schema_version",
            "ts",
            "iter",
            "pv",
            "sp",
            "err",
            "p",
            "i",
            "d",
            "cv",
            "i_acc",
            "dt",
        ] {
            assert!(v.get(field).is_some(), "missing field `{field}` in: {line}");
        }
    }
}

/// iter in the output log increments starting from 1.
#[test]
fn replay_output_log_iter_increments() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("out.ndjson");

    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0), (51.0, 4.0, 55.0), (52.0, 3.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--output-log"])
        .arg(&out_path)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    let iters: Vec<u64> = content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["iter"].as_u64().unwrap()
        })
        .collect();

    assert_eq!(iters, vec![1, 2, 3]);
}

// ---------------------------------------------------------------------------
// Event records are skipped (not treated as iteration records)
// ---------------------------------------------------------------------------

/// NDJSON logs contain event records (e.g. `gains_changed`). Replay must skip
/// them and process only the iteration records.
#[test]
fn replay_skips_event_records() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("out.ndjson");

    // Mix event records and iteration records.
    let log_content = [
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:00Z","event":"gains_changed","kp":1.0,"ki":0.0,"kd":0.0,"sp":55.0,"iter":0,"source":"cli"}"#,
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:01Z","iter":1,"pv":50.0,"sp":55.0,"err":5.0,"p":5.0,"i":0.0,"d":0.0,"cv":5.0,"i_acc":0.0,"dt":1.0}"#,
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:01Z","event":"dt_skipped","raw_dt":0.001,"min_dt":0.01,"max_dt":10.0}"#,
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:02Z","iter":2,"pv":51.0,"sp":55.0,"err":4.0,"p":4.0,"i":0.0,"d":0.0,"cv":4.0,"i_acc":0.0,"dt":1.0}"#,
    ]
    .join("\n") + "\n";

    std::fs::write(&log_path, &log_content).unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--output-log"])
        .arg(&out_path)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "only iteration records should appear in output"
    );
}

// ---------------------------------------------------------------------------
// --diff flag
// ---------------------------------------------------------------------------

/// `--diff` prints a JSON summary to stdout with `n`, `max_diff`, `rms_diff`.
#[test]
fn replay_diff_flag_prints_summary_fields() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");

    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0), (51.0, 4.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    let output = Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--diff"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("diff output is JSON");

    assert!(v.get("n").is_some(), "missing `n` in diff output");
    assert!(
        v.get("max_diff").is_some(),
        "missing `max_diff` in diff output"
    );
    assert!(
        v.get("rms_diff").is_some(),
        "missing `rms_diff` in diff output"
    );
    assert_eq!(v["n"].as_u64().unwrap(), 2);
}

/// Round-trip: same gains → `max_diff` and `rms_diff` are both effectively zero.
#[test]
fn replay_diff_round_trip_is_zero() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");

    // Log was produced with kp=1.0, ki=0, kd=0, sp=55 — so cv = sp - pv.
    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0), (51.0, 4.0, 55.0), (52.0, 3.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    let output = Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--diff"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();

    let max_diff = v["max_diff"].as_f64().unwrap();
    let rms_diff = v["rms_diff"].as_f64().unwrap();
    assert!(
        max_diff < 1e-9,
        "max_diff should be ~0 for round-trip, got {max_diff}"
    );
    assert!(
        rms_diff < 1e-9,
        "rms_diff should be ~0 for round-trip, got {rms_diff}"
    );
}

/// `--diff` and `--output-log` can be combined: both the diff and the log are produced.
#[test]
fn replay_diff_and_output_log_combined() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("out.ndjson");

    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    let output = Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--diff", "--output-log"])
        .arg(&out_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Diff summary on stdout.
    let stdout = String::from_utf8(output).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v.get("n").is_some());

    // Output log written.
    let out_content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        !out_content.trim().is_empty(),
        "output log must not be empty"
    );
}

// ---------------------------------------------------------------------------
// Incomplete log record validation
// ---------------------------------------------------------------------------

/// A log record with `dt` but no `pv` must produce a clear error and non-zero exit.
#[test]
fn replay_rejects_missing_pv() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("bad.ndjson");

    // Has dt but no pv.
    std::fs::write(
        &log_path,
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:00Z","iter":1,"sp":55.0,"err":5.0,"cv":5.0,"i_acc":0.0,"dt":1.0}"#,
    )
    .unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--diff"])
        .assert()
        .failure()
        .stderr(contains("pv"));
}

/// A log record with `pv` but no `dt` must produce a clear error and non-zero exit.
#[test]
fn replay_rejects_missing_dt() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("bad.ndjson");

    // Has pv but no dt.
    std::fs::write(
        &log_path,
        r#"{"schema_version":1,"ts":"2026-01-01T00:00:00Z","iter":1,"pv":50.0,"sp":55.0,"err":5.0,"cv":5.0,"i_acc":0.0}"#,
    )
    .unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "1.0", "--diff"])
        .assert()
        .failure()
        .stderr(contains("dt"));
}

// ---------------------------------------------------------------------------
// Error: --log file does not exist
// ---------------------------------------------------------------------------

#[test]
fn replay_missing_log_file_exits_nonzero() {
    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args([
            "replay",
            "--log",
            "/nonexistent/path/missing.ndjson",
            "--kp",
            "1.0",
            "--diff",
        ])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// Setpoint auto-detected from log when --setpoint is absent
// ---------------------------------------------------------------------------

/// Without `--setpoint`, replay reads sp from the first log record and uses it.
/// CV should equal kp*(sp-pv) for a pure-P controller.
#[test]
fn replay_setpoint_from_log_when_not_given() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("source.ndjson");
    let out_path = dir.path().join("out.ndjson");

    // sp=55.0, pv=50.0 → with kp=2.0, expected CV=10.0
    let log_content = make_ndjson_log(&[(50.0, 5.0, 55.0)]);
    std::fs::write(&log_path, &log_content).unwrap();

    Command::cargo_bin("pid-ctl")
        .unwrap()
        .args(["replay", "--log"])
        .arg(&log_path)
        .args(["--kp", "2.0", "--output-log"])
        .arg(&out_path)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    let line = content.lines().find(|l| !l.is_empty()).unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let cv = v["cv"].as_f64().unwrap();
    assert!(
        (cv - 10.0).abs() < 1e-9,
        "expected CV=10.0 (kp=2 * error=5), got {cv}"
    );
}
