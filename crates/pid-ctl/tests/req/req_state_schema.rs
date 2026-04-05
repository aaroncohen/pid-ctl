//! State file & NDJSON — `pid-ctl_plan.md` (State File Schema, JSON Log Line Schema).

use assert_cmd::Command;
use pid_ctl::app::{STATE_SCHEMA_VERSION, StateSnapshot, StateStore};
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn state_snapshot_includes_schema_version() {
    let tempdir = tempdir().expect("temporary directory");
    let path = tempdir.path().join("fan.json");
    let store = StateStore::new(&path);
    let snapshot = StateSnapshot {
        name: Some(String::from("fan-cpu")),
        setpoint: Some(55.0),
        i_acc: -8.23,
        iter: 47,
        ..StateSnapshot::default()
    };

    store.save(&snapshot).expect("state saved");

    let json = std::fs::read_to_string(&path).expect("saved state JSON");
    let value: Value = serde_json::from_str(&json).expect("valid state JSON");

    assert_eq!(value["schema_version"], Value::from(STATE_SCHEMA_VERSION));

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot exists");
    assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
}

#[test]
fn unknown_fields_ignored_on_load() {
    let tempdir = tempdir().expect("temporary directory");
    let path = tempdir.path().join("fan.json");
    let store = StateStore::new(&path);

    std::fs::write(
        &path,
        r#"{
  "schema_version": 1,
  "setpoint": 42.5,
  "i_acc": 1.25,
  "future_field": "ignore me",
  "future_object": { "nested": true }
}
"#,
    )
    .expect("state fixture written");

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot exists");

    assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
    assert!((loaded.setpoint.expect("setpoint present") - 42.5).abs() < 1e-9);
    assert!((loaded.i_acc - 1.25).abs() < 1e-9);
    assert_eq!(loaded.iter, 0);
}

#[test]
fn output_limits_round_trip_when_present() {
    let tempdir = tempdir().expect("temporary directory");
    let path = tempdir.path().join("fan.json");
    let store = StateStore::new(&path);
    let snapshot = StateSnapshot {
        out_min: Some(10.0),
        out_max: Some(90.0),
        ..StateSnapshot::default()
    };

    store.save(&snapshot).expect("state saved");

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot exists");

    assert_eq!(loaded.out_min, Some(10.0));
    assert_eq!(loaded.out_max, Some(90.0));
}

// --- Bugs: timestamps never populated (pid-ctl-2hd) ---

/// pid-ctl-2hd: first run sets `created_at` and `updated_at`.
#[test]
fn first_run_populates_created_at_and_updated_at() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ts.json");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--cv-stdout",
        "--state",
    ]);
    cmd.arg(&state_path);
    cmd.assert().success();

    let store = StateStore::new(&state_path);
    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot exists");

    assert!(
        loaded.created_at.is_some(),
        "created_at should be set on first run"
    );
    assert!(
        loaded.updated_at.is_some(),
        "updated_at should be set on first run"
    );
}

/// pid-ctl-2hd: subsequent runs preserve `created_at` and advance `updated_at`.
#[test]
fn subsequent_run_preserves_created_at_and_advances_updated_at() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ts.json");
    let store = StateStore::new(&state_path);

    store
        .save(&StateSnapshot {
            created_at: Some(String::from("2026-01-01T00:00:00Z")),
            updated_at: Some(String::from("2026-01-01T00:00:00Z")),
            ..StateSnapshot::default()
        })
        .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "50.0",
        "--setpoint",
        "55.0",
        "--cv-stdout",
        "--state",
    ]);
    cmd.arg(&state_path);
    cmd.assert().success();

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot exists");

    assert_eq!(
        loaded.created_at.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "created_at must not change on subsequent runs"
    );
    assert_ne!(
        loaded.updated_at.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "updated_at must advance on each run"
    );
}
