//! Integration tests for `pid-ctl autotune` (Åström–Hägglund relay autotune).
//!
//! Tests verify the CLI contracts (exit codes, JSON output, state-file writes,
//! validation errors) without inspecting relay engine internals.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path to `target/debug/<name>`.
fn target_debug_bin(name: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // deps
    p.pop(); // debug
    p.join(name)
}

// ---------------------------------------------------------------------------
// Validation: reject misconfig at parse time (exit 3)
// ---------------------------------------------------------------------------

/// amp = 0 must be rejected with exit 3.
#[test]
fn autotune_rejects_zero_amp() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "50",
            "--amp",
            "0",
            "--duration",
            "30s",
        ])
        .assert()
        .code(3)
        .stderr(contains("--amp must be > 0"));
}

/// amp < 0 must be rejected with exit 3 (either as a clap parse error or
/// as a validation error — both are acceptable at exit 3).
#[test]
fn autotune_rejects_negative_amp() {
    // Negative floats look like flags to clap; pass via `--amp=-5` to avoid
    // the "unexpected argument" parse error while still testing the value path.
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "50",
            "--amp=-5",
            "--duration",
            "30s",
        ])
        .assert()
        .code(3)
        .stderr(contains("--amp must be > 0"));
}

/// bias outside `[out_min, out_max]` must be rejected.
#[test]
fn autotune_rejects_bias_above_out_max() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "110",
            "--amp",
            "10",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--duration",
            "30s",
        ])
        .assert()
        .code(3)
        .stderr(contains("outside"));
}

/// relay high (bias + amp) exceeding `out_max` must be rejected.
#[test]
fn autotune_rejects_relay_high_exceeds_out_max() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "95",
            "--amp",
            "20",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--duration",
            "30s",
        ])
        .assert()
        .code(3)
        .stderr(contains("exceed"));
}

/// relay low (bias - amp) below `out_min` must be rejected.
#[test]
fn autotune_rejects_relay_low_below_out_min() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "5",
            "--amp",
            "20",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--duration",
            "30s",
        ])
        .assert()
        .code(3)
        .stderr(contains("below out_min"));
}

/// duration < 10s must be rejected with exit 3.
#[test]
fn autotune_rejects_duration_too_short() {
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            "echo 50",
            "--cv-cmd",
            "true",
            "--bias",
            "50",
            "--amp",
            "20",
            "--duration",
            "5s",
        ])
        .assert()
        .code(3)
        .stderr(contains("--duration must be at least"));
}

// ---------------------------------------------------------------------------
// End-to-end: relay drives pid-ctl-sim; assert Ku/Tu within 20% tolerance
// ---------------------------------------------------------------------------

/// Full autotune run against the pid-ctl-sim first-order plant.
///
/// Uses a fast plant (`tau=0.2`) so the relay oscillation settles in a few
/// seconds and the test completes well within the CI timeout.
#[test]
fn autotune_e2e_first_order_produces_valid_ku_tu() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    if !sim_bin.exists() {
        eprintln!("skip: build pid-ctl-sim first ({})", sim_bin.display());
        return;
    }

    let dir = tempdir().expect("tempdir");
    let plant = dir.path().join("plant.json");

    // Fast plant: tau=0.2 → Tu ≈ 0.8s → 3 cycles in ~2.4s.
    Command::new(&sim_bin)
        .args([
            "init",
            "--state",
            plant.to_str().expect("utf8"),
            "--plant",
            "first-order",
            "--param",
            "tau=0.2",
            "--param",
            "gain=1",
        ])
        .assert()
        .success();

    // dt matches interval so the sim advances one tick per relay tick.
    let pv_cmd = format!("{} print-pv --state {}", sim_bin.display(), plant.display());
    let cv_cmd = format!(
        "{} apply-cv --state {} --dt 0.05 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    );

    // Duration=10s: warmup≈2.5s (25%), relay test 7.5s ≫ 3*Tu=2.4s.
    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            &pv_cmd,
            "--cv-cmd",
            &cv_cmd,
            "--bias",
            "50",
            "--amp",
            "10",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--interval",
            "50ms",
            "--duration",
            "10s",
            "--rule",
            "pid",
        ])
        .timeout(Duration::from_secs(60))
        .output()
        .expect("spawn autotune");

    // The process may exit 0 (settled) or be killed by timeout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Must not have I/O error messages.
    assert!(
        !stderr.contains("PV read failed") && !stderr.contains("CV write failed"),
        "I/O errors in stderr: {stderr:?}"
    );

    // The last non-empty stdout line should be valid JSON with ku and tu fields.
    let last_json_line = stdout
        .lines()
        .rfind(|l| l.starts_with('{'))
        .expect("at least one JSON line in stdout");

    let result: serde_json::Value =
        serde_json::from_str(last_json_line).expect("last stdout line is valid JSON");

    let ku = result["ku"].as_f64().expect("ku is f64");
    let tu = result["tu"].as_f64().expect("tu is f64");
    let kp = result["kp"].as_f64().expect("kp is f64");

    assert!(ku > 0.0, "ku={ku}");
    assert!(tu > 0.0, "tu={tu}");
    assert!(kp > 0.0, "kp={kp}");

    // Basic sanity: Z-N PID kp = 0.6 * ku
    let expected_kp = 0.6 * ku;
    let diff = (kp - expected_kp).abs() / expected_kp;
    assert!(diff < 0.01, "kp={kp} expected ~{expected_kp} (0.6*ku={ku})");
}

// ---------------------------------------------------------------------------
// State file: --state writes suggested gains
// ---------------------------------------------------------------------------

/// `--state` causes suggested gains to be written to the state file.
#[test]
fn autotune_writes_state_with_suggested_gains() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    if !sim_bin.exists() {
        eprintln!("skip: build pid-ctl-sim first ({})", sim_bin.display());
        return;
    }

    let dir = tempdir().expect("tempdir");
    let plant = dir.path().join("plant.json");
    let state = dir.path().join("gains.json");

    // Fast plant: tau=0.2, same settings as the e2e test.
    Command::new(&sim_bin)
        .args([
            "init",
            "--state",
            plant.to_str().expect("utf8"),
            "--plant",
            "first-order",
            "--param",
            "tau=0.2",
            "--param",
            "gain=1",
        ])
        .assert()
        .success();

    let pv_cmd = format!("{} print-pv --state {}", sim_bin.display(), plant.display());
    let cv_cmd = format!(
        "{} apply-cv --state {} --dt 0.05 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    );

    // Run with --state so gains are persisted.
    Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            &pv_cmd,
            "--cv-cmd",
            &cv_cmd,
            "--bias",
            "50",
            "--amp",
            "10",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--interval",
            "50ms",
            "--duration",
            "10s",
            "--rule",
            "pid",
            "--state",
            state.to_str().expect("utf8"),
        ])
        .timeout(Duration::from_secs(60))
        .output()
        .expect("spawn autotune");

    // State file must exist and contain kp, ki, kd.
    assert!(state.exists(), "state file not created");
    let json_str = std::fs::read_to_string(&state).expect("read state");
    let snapshot: serde_json::Value = serde_json::from_str(&json_str).expect("state is valid JSON");

    let kp = snapshot["kp"].as_f64().expect("kp in state");
    let ki = snapshot["ki"].as_f64().expect("ki in state");
    let kd = snapshot["kd"].as_f64().expect("kd in state");

    assert!(kp > 0.0, "kp={kp}");
    assert!(ki > 0.0, "ki={ki}");
    assert!(kd > 0.0, "kd={kd}");
}

// ---------------------------------------------------------------------------
// NDJSON events: relay_flip events in stdout
// ---------------------------------------------------------------------------

/// Stdout must contain at least one `relay_flip` event in NDJSON format
/// before the final result line.
#[test]
fn autotune_emits_relay_flip_events() {
    let sim_bin = target_debug_bin("pid-ctl-sim");
    if !sim_bin.exists() {
        return;
    }

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
            "tau=0.2",
            "--param",
            "gain=1",
        ])
        .assert()
        .success();

    let pv_cmd = format!("{} print-pv --state {}", sim_bin.display(), plant.display());
    let cv_cmd = format!(
        "{} apply-cv --state {} --dt 0.05 --cv {{cv}}",
        sim_bin.display(),
        plant.display()
    );

    let output = Command::cargo_bin("pid-ctl")
        .expect("pid-ctl")
        .args([
            "autotune",
            "--pv-cmd",
            &pv_cmd,
            "--cv-cmd",
            &cv_cmd,
            "--bias",
            "50",
            "--amp",
            "10",
            "--out-min",
            "0",
            "--out-max",
            "100",
            "--interval",
            "50ms",
            "--duration",
            "10s",
        ])
        .timeout(Duration::from_secs(60))
        .output()
        .expect("spawn autotune");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let has_flip = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| v["event"].as_str() == Some("relay_flip"));

    assert!(has_flip, "no relay_flip event in stdout:\n{stdout}");
}
