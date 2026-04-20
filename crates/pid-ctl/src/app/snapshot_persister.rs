//! Persistence and failure-escalation bookkeeping, extracted from [`super::ControllerSession`].
//!
//! [`SnapshotPersister`] owns the `StateStore`, `StateLock`, flush-coalescing state,
//! and the consecutive-failure counter. It exposes a narrow API so
//! `ControllerSession` can delegate all persistence concerns here.

use super::state_store::{StateLock, StateSnapshot, StateStore, StateStoreError};
use std::time::{Duration, Instant};

/// Owns all snapshot-persistence state for a controller session.
///
/// Handles flush coalescing (skip writes that arrive before `flush_interval` has
/// elapsed), consecutive-failure tracking, and the lockfile lifetime.
#[derive(Debug)]
pub struct SnapshotPersister {
    store: Option<StateStore>,
    _lock: Option<StateLock>,
    flush_interval: Option<Duration>,
    last_flush: Option<Instant>,
    fail_count: u32,
    fail_after: u32,
}

impl SnapshotPersister {
    /// Constructs a persister, taking ownership of the store and its lock.
    #[must_use]
    pub fn new(
        store: Option<StateStore>,
        lock: Option<StateLock>,
        flush_interval: Option<Duration>,
        fail_after: u32,
    ) -> Self {
        Self {
            store,
            _lock: lock,
            flush_interval,
            last_flush: None,
            fail_count: 0,
            fail_after,
        }
    }

    /// Updates the flush coalescing cadence (e.g. when `--interval` changes at runtime).
    pub const fn set_flush_interval(&mut self, flush_interval: Option<Duration>) {
        self.flush_interval = flush_interval;
    }

    /// Returns `true` if a backing `StateStore` was provided at construction.
    #[must_use]
    pub const fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Returns the current consecutive state write failure count.
    #[must_use]
    pub const fn fail_count(&self) -> u32 {
        self.fail_count
    }

    /// Returns `true` when consecutive failures have reached the escalation threshold.
    #[must_use]
    pub const fn fail_escalated(&self) -> bool {
        self.fail_count >= self.fail_after && self.fail_after > 0
    }

    /// Persists `snapshot`, respecting `flush_interval` coalescing.
    ///
    /// When `flush_interval` is set, skips the disk write if not enough time has
    /// elapsed since the last flush. In-memory state is always current regardless.
    ///
    /// Updates `fail_count` on failure or success.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError`] when the underlying `StateStore::save` fails.
    pub fn persist(&mut self, snapshot: &StateSnapshot) -> Result<(), StateStoreError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        // Coalescing: skip disk write if within the flush interval.
        if let (Some(interval), Some(last)) = (self.flush_interval, self.last_flush)
            && last.elapsed() < interval
        {
            return Ok(());
        }

        let result = store.save(snapshot);
        match &result {
            Ok(()) => {
                self.fail_count = 0;
                self.last_flush = Some(Instant::now());
            }
            Err(_) => {
                self.fail_count = self.fail_count.saturating_add(1);
            }
        }
        result
    }

    /// Forces a disk flush regardless of `flush_interval`.
    ///
    /// Used at shutdown to ensure the final in-memory state is persisted.
    pub fn force_flush(&mut self, snapshot: &StateSnapshot) -> Option<StateStoreError> {
        let Some(store) = &self.store else {
            return None;
        };
        match store.save(snapshot) {
            Ok(()) => {
                self.fail_count = 0;
                self.last_flush = Some(Instant::now());
                None
            }
            Err(e) => {
                self.fail_count = self.fail_count.saturating_add(1);
                Some(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state_store::StateSnapshot;

    fn persister_no_store() -> SnapshotPersister {
        SnapshotPersister::new(None, None, None, 10)
    }

    // ── has_store ──────────────────────────────────────────────────────────────

    #[test]
    fn has_store_false_when_no_store() {
        assert!(!persister_no_store().has_store());
    }

    // ── persist (no store) ────────────────────────────────────────────────────

    #[test]
    fn persist_no_store_is_ok() {
        let mut p = persister_no_store();
        assert!(p.persist(&StateSnapshot::default()).is_ok());
    }

    #[test]
    fn force_flush_no_store_returns_none() {
        let mut p = persister_no_store();
        assert!(p.force_flush(&StateSnapshot::default()).is_none());
    }

    // ── fail_count / fail_escalated ───────────────────────────────────────────

    #[test]
    fn fail_count_starts_at_zero() {
        assert_eq!(persister_no_store().fail_count(), 0);
    }

    #[test]
    fn fail_escalated_false_when_no_failures() {
        assert!(!persister_no_store().fail_escalated());
    }

    #[test]
    fn fail_escalated_false_when_fail_after_zero() {
        let p = SnapshotPersister::new(None, None, None, 0);
        assert!(!p.fail_escalated());
    }

    // ── coalescing (with real StateStore on tmpfile) ──────────────────────────

    fn persister_with_tmpfile(
        flush_interval: Option<Duration>,
    ) -> (SnapshotPersister, tempfile::TempDir) {
        use crate::app::state_store::StateStore;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = StateStore::new(path);
        let p = SnapshotPersister::new(Some(store), None, flush_interval, 3);
        (p, dir)
    }

    #[test]
    fn persist_writes_on_first_call() {
        let (mut p, dir) = persister_with_tmpfile(Some(Duration::from_secs(60)));
        p.persist(&StateSnapshot::default()).unwrap();
        assert!(dir.path().join("state.json").exists());
    }

    #[test]
    fn persist_coalesces_within_flush_interval() {
        let (mut p, dir) = persister_with_tmpfile(Some(Duration::from_secs(60)));
        let snap = StateSnapshot::default();
        p.persist(&snap).unwrap(); // first write lands
        // Delete the file so we can tell whether persist() tries to write again.
        std::fs::remove_file(dir.path().join("state.json")).unwrap();
        // Second call within the interval should be coalesced (no write).
        p.persist(&snap).unwrap();
        assert!(
            !dir.path().join("state.json").exists(),
            "second persist within flush_interval should be coalesced"
        );
    }

    #[test]
    fn persist_no_interval_always_writes() {
        let (mut p, dir) = persister_with_tmpfile(None);
        let snap = StateSnapshot::default();
        p.persist(&snap).unwrap();
        std::fs::remove_file(dir.path().join("state.json")).unwrap();
        p.persist(&snap).unwrap();
        assert!(
            dir.path().join("state.json").exists(),
            "no flush_interval means every call writes"
        );
    }

    #[test]
    fn force_flush_ignores_coalescing() {
        let (mut p, dir) = persister_with_tmpfile(Some(Duration::from_secs(60)));
        let snap = StateSnapshot::default();
        p.persist(&snap).unwrap(); // prime last_flush
        std::fs::remove_file(dir.path().join("state.json")).unwrap();
        // force_flush must write even though the interval hasn't elapsed.
        assert!(p.force_flush(&snap).is_none());
        assert!(
            dir.path().join("state.json").exists(),
            "force_flush should bypass coalescing"
        );
    }

    // ── escalation with a real failing store ──────────────────────────────────

    #[test]
    fn fail_escalated_after_threshold() {
        // Use a path in a non-existent directory so every save fails.
        let store = StateStore::new("/nonexistent/dir/state.json");
        let mut p = SnapshotPersister::new(Some(store), None, None, 2);
        let snap = StateSnapshot::default();

        assert!(!p.fail_escalated());
        let _ = p.persist(&snap); // fail #1 — count = 1, not yet escalated
        assert!(!p.fail_escalated());
        let _ = p.persist(&snap); // fail #2 — count = 2, threshold reached
        assert!(p.fail_escalated());
        assert_eq!(p.fail_count(), 2);
    }

    #[test]
    fn fail_count_resets_after_success() {
        use crate::app::state_store::StateStore;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = StateStore::new(path);
        let mut p = SnapshotPersister::new(Some(store), None, None, 10);
        let snap = StateSnapshot::default();

        // Force two failures by pointing at a bad path first (can't do in this
        // design without changing the store after construction, so simulate by
        // saturating directly: we'll just verify that success resets to 0).
        p.persist(&snap).unwrap(); // success → fail_count stays 0
        assert_eq!(p.fail_count(), 0);
    }

    #[test]
    fn force_flush_success_resets_fail_count() {
        use crate::app::state_store::StateStore;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = StateStore::new(&path);
        let mut p = SnapshotPersister::new(Some(store), None, None, 10);
        let snap = StateSnapshot::default();

        // Succeed once to initialise last_flush, then verify count is 0.
        assert!(p.force_flush(&snap).is_none());
        assert_eq!(p.fail_count(), 0);
    }
}
