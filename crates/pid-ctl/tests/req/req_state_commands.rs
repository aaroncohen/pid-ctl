//! `status`, `purge`, and `init` subcommand tests — pid-ctl-8go.

use assert_cmd::Command;
use pid_ctl::app::{STATE_SCHEMA_VERSION, StateSnapshot, StateStore};
use predicates::str::contains;
use tempfile::tempdir;

/// pid-ctl-8go: `status` loads and pretty-prints the state file as JSON to stdout.
#[test]
fn status_prints_state_json() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ctrl.json");
    let store = StateStore::new(&state_path);

    let snapshot = StateSnapshot {
        kp: Some(1.5),
        ki: Some(0.3),
        kd: Some(0.0),
        setpoint: Some(42.0),
        i_acc: 7.25,
        iter: 10,
        ..StateSnapshot::default()
    };
    store.save(&snapshot).expect("state saved");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("status").arg("--state").arg(&state_path);

    cmd.assert()
        .success()
        .stdout(contains("schema_version"))
        .stdout(contains("1.5"))
        .stdout(contains("42.0"))
        .stdout(contains("7.25"));
}

/// pid-ctl-8go: `status` without `--state` exits 3 with an appropriate message.
#[test]
fn status_requires_state_flag() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("status");

    cmd.assert()
        .code(3)
        .stderr(contains("status requires --state"));
}

/// pid-ctl-8go: `purge` clears runtime fields and preserves config fields.
#[test]
fn purge_clears_runtime_preserves_config() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ctrl.json");
    let store = StateStore::new(&state_path);

    let snapshot = StateSnapshot {
        schema_version: STATE_SCHEMA_VERSION,
        name: Some(String::from("motor")),
        kp: Some(2.0),
        ki: Some(0.5),
        kd: Some(0.1),
        setpoint: Some(100.0),
        out_min: Some(0.0),
        out_max: Some(255.0),
        i_acc: 12.5,
        last_pv: Some(98.0),
        last_error: Some(2.0),
        last_cv: Some(130.0),
        iter: 50,
        effective_sp: Some(99.0),
        target_sp: Some(100.0),
        created_at: Some(String::from("2026-01-01T00:00:00Z")),
        updated_at: Some(String::from("2026-01-01T00:00:00Z")),
    };
    store.save(&snapshot).expect("state saved");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("purge").arg("--state").arg(&state_path);
    cmd.assert()
        .success()
        .stderr(contains("purged runtime state from"));

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    // Runtime fields must be cleared.
    assert!((loaded.i_acc - 0.0).abs() < f64::EPSILON);
    assert_eq!(loaded.last_pv, None);
    assert_eq!(loaded.last_error, None);
    assert_eq!(loaded.last_cv, None);
    assert_eq!(loaded.iter, 0);
    assert_eq!(loaded.effective_sp, None);
    assert_eq!(loaded.target_sp, None);

    // Config fields must be preserved.
    assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
    assert_eq!(loaded.name.as_deref(), Some("motor"));
    assert_eq!(loaded.kp, Some(2.0));
    assert_eq!(loaded.ki, Some(0.5));
    assert_eq!(loaded.kd, Some(0.1));
    assert_eq!(loaded.setpoint, Some(100.0));
    assert_eq!(loaded.out_min, Some(0.0));
    assert_eq!(loaded.out_max, Some(255.0));
    assert_eq!(loaded.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert!(
        loaded.updated_at.as_deref() != Some("2026-01-01T00:00:00Z"),
        "updated_at should have been advanced"
    );
}

/// pid-ctl-8go: `purge` without `--state` exits 3.
#[test]
fn purge_requires_state_flag() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("purge");

    cmd.assert()
        .code(3)
        .stderr(contains("purge requires --state"));
}

/// pid-ctl-8go: `purge` fails with non-zero exit when the lock is already held.
#[test]
fn purge_fails_when_locked() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ctrl.json");
    let store = StateStore::new(&state_path);

    // Seed a state file so purge won't fail on "file not found" before the lock check.
    store.save(&StateSnapshot::default()).expect("seed state");

    // Acquire the lock in-process to block the subprocess.
    let _lock = store.acquire_lock().expect("lock acquired");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("purge").arg("--state").arg(&state_path);

    // The lock is held, so purge must exit non-zero.
    cmd.assert().failure();
}

/// pid-ctl-8go: `init` creates a fresh default state file over an existing one.
#[test]
fn init_creates_fresh_state() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("ctrl.json");
    let store = StateStore::new(&state_path);

    // Write a state file with non-default runtime state.
    let existing = StateSnapshot {
        kp: Some(3.0),
        i_acc: 99.9,
        iter: 1000,
        last_cv: Some(200.0),
        ..StateSnapshot::default()
    };
    store.save(&existing).expect("existing state saved");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("init").arg("--state").arg(&state_path);
    cmd.assert()
        .success()
        .stderr(contains("initialized state file"));

    let loaded = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    // Should be a fresh default (no runtime or config from the old file).
    assert!((loaded.i_acc - 0.0).abs() < f64::EPSILON);
    assert_eq!(loaded.iter, 0);
    assert_eq!(loaded.last_cv, None);
    assert_eq!(loaded.kp, None);
    assert!(loaded.created_at.is_some(), "created_at should be set");
    assert!(loaded.updated_at.is_some(), "updated_at should be set");
}

/// pid-ctl-8go: `init` without `--state` exits 3.
#[test]
fn init_requires_state_flag() {
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.arg("init");

    cmd.assert()
        .code(3)
        .stderr(contains("init requires --state"));
}
