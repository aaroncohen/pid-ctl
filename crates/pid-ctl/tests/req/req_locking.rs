//! Lockfile exclusivity when `--state` set — `pid-ctl_plan.md` (Reliability item 13).

use pid_ctl::app::{StateStore, StateStoreError};
use tempfile::tempdir;

#[test]
fn lock_acquired_when_state_configured() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("fan.json");
    let store = StateStore::new(&state_path);

    let first_lock = store.acquire_lock().expect("first lock acquired");
    assert_eq!(first_lock.path(), store.lock_path().as_path());

    let second_lock = store.acquire_lock();
    assert!(matches!(
        second_lock,
        Err(StateStoreError::LockHeld { ref path }) if path == &store.lock_path()
    ));

    drop(first_lock);

    let third_lock = store.acquire_lock();
    assert!(third_lock.is_ok());
}
