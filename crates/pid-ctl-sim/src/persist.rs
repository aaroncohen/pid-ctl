//! File load/save for [`crate::SimState`].

use crate::state::SimState;
use std::fs;
use std::io;
use std::path::Path;

/// Reads JSON state from `path`.
///
/// # Errors
///
/// Returns an error on I/O or JSON parse failure.
pub fn load(path: &Path) -> io::Result<SimState> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes pretty-printed JSON state to `path`.
///
/// # Errors
///
/// Returns an error on I/O or serialization failure.
pub fn save(path: &Path, state: &SimState) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    fs::write(path, json)
}
