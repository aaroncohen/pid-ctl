//! JSON state snapshot and lockfile primitives for `pid-ctl`.
//!
//! This module intentionally stays small: it provides the versioned on-disk state
//! contract plus `<state>.lock` single-writer exclusivity. Higher-level runtime
//! orchestration can build on top of these pieces later.

use fs2::FileExt;
use pid_ctl_core::PidRuntimeState;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Supported on-disk schema version for persisted controller snapshots.
pub const STATE_SCHEMA_VERSION: u64 = 1;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persisted controller state shared between `once`, `loop`, and offline tools.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct StateSnapshot {
    #[serde(default = "default_schema_version")]
    pub schema_version: u64,
    pub name: Option<String>,
    pub kp: Option<f64>,
    pub ki: Option<f64>,
    pub kd: Option<f64>,
    pub setpoint: Option<f64>,
    pub out_min: Option<f64>,
    pub out_max: Option<f64>,
    pub effective_sp: Option<f64>,
    pub target_sp: Option<f64>,
    pub i_acc: f64,
    pub last_error: Option<f64>,
    pub last_cv: Option<f64>,
    pub last_pv: Option<f64>,
    pub iter: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            name: None,
            kp: None,
            ki: None,
            kd: None,
            setpoint: None,
            out_min: None,
            out_max: None,
            effective_sp: None,
            target_sp: None,
            i_acc: 0.0,
            last_error: None,
            last_cv: None,
            last_pv: None,
            iter: 0,
            created_at: None,
            updated_at: None,
        }
    }
}

impl StateSnapshot {
    /// Parses a state snapshot from JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError`] when the JSON is invalid or the snapshot
    /// advertises a newer schema version than this binary supports.
    pub fn from_json_str(json: &str) -> Result<Self, StateStoreError> {
        let snapshot: Self = serde_json::from_str(json).map_err(StateStoreError::Json)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Serializes the snapshot to pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError`] when the snapshot is invalid or cannot be
    /// serialized.
    pub fn to_json_string(&self) -> Result<String, StateStoreError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(StateStoreError::Json)
    }

    fn validate(&self) -> Result<(), StateStoreError> {
        if self.schema_version > STATE_SCHEMA_VERSION {
            return Err(StateStoreError::UnsupportedSchemaVersion {
                found: self.schema_version,
                supported: STATE_SCHEMA_VERSION,
            });
        }

        Ok(())
    }

    #[must_use]
    pub fn runtime_state(&self) -> PidRuntimeState {
        PidRuntimeState {
            i_acc: self.i_acc,
            last_pv: self.last_pv,
            last_error: self.last_error,
            last_cv: self.last_cv,
            effective_sp: self.effective_sp,
        }
    }
}

/// Filesystem-backed location for a controller state snapshot and its lockfile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// Creates a state store rooted at `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the configured snapshot path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the dedicated lockfile path (`<state>.lock`).
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        let mut lock_name: OsString = self.path.as_os_str().to_owned();
        lock_name.push(".lock");
        PathBuf::from(lock_name)
    }

    /// Loads a snapshot from disk when it exists.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError`] when reading or parsing fails.
    pub fn load(&self) -> Result<Option<StateSnapshot>, StateStoreError> {
        match fs::read_to_string(&self.path) {
            Ok(json) => StateSnapshot::from_json_str(&json).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StateStoreError::Io {
                path: self.path.clone(),
                source: error,
            }),
        }
    }

    /// Persists a snapshot with write-to-temp then rename semantics.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError`] when serialization or filesystem writes fail.
    pub fn save(&self, snapshot: &StateSnapshot) -> Result<(), StateStoreError> {
        let mut json = snapshot.to_json_string()?;
        json.push('\n');

        let temp_path = self.temp_path();
        let write_result = (|| -> Result<(), StateStoreError> {
            let mut temp_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .map_err(|source| StateStoreError::Io {
                    path: temp_path.clone(),
                    source,
                })?;

            temp_file
                .write_all(json.as_bytes())
                .map_err(|source| StateStoreError::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            temp_file.sync_all().map_err(|source| StateStoreError::Io {
                path: temp_path.clone(),
                source,
            })?;

            fs::rename(&temp_path, &self.path).map_err(|source| StateStoreError::Io {
                path: self.path.clone(),
                source,
            })?;

            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }

        write_result
    }

    /// Acquires the dedicated lockfile for single-writer state access.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError::LockHeld`] when another process or handle already
    /// owns the lock, or an I/O error if the lockfile cannot be opened.
    pub fn acquire_lock(&self) -> Result<StateLock, StateStoreError> {
        let lock_path = self.lock_path();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| StateStoreError::Io {
                path: lock_path.clone(),
                source,
            })?;

        file.try_lock_exclusive().map_err(|source| {
            if source.kind() == io::ErrorKind::WouldBlock {
                StateStoreError::LockHeld {
                    path: lock_path.clone(),
                }
            } else {
                StateStoreError::Io {
                    path: lock_path.clone(),
                    source,
                }
            }
        })?;

        Ok(StateLock {
            path: lock_path,
            file,
        })
    }

    fn temp_path(&self) -> PathBuf {
        let parent = self
            .path
            .parent()
            .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
        let file_name = self.path.file_name().map_or_else(
            || OsString::from("state.json"),
            std::ffi::OsStr::to_os_string,
        );

        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        parent.join(temp_name)
    }
}

/// Held exclusive lock on `<state>.lock`.
#[derive(Debug)]
pub struct StateLock {
    path: PathBuf,
    file: File,
}

impl StateLock {
    /// Returns the filesystem path of the held lockfile.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// State-store and lockfile failures.
#[derive(Debug)]
pub enum StateStoreError {
    Io { path: PathBuf, source: io::Error },
    Json(serde_json::Error),
    LockHeld { path: PathBuf },
    UnsupportedSchemaVersion { found: u64, supported: u64 },
}

impl fmt::Display for StateStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            Self::Json(source) => write!(f, "state JSON error: {source}"),
            Self::LockHeld { path } => write!(
                f,
                "{} is already locked by another pid-ctl writer",
                path.display()
            ),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "state schema_version {found} is newer than supported version {supported}"
            ),
        }
    }
}

impl Error for StateStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            Self::LockHeld { .. } | Self::UnsupportedSchemaVersion { .. } => None,
        }
    }
}

fn default_schema_version() -> u64 {
    STATE_SCHEMA_VERSION
}
