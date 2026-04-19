//! Owned log sink: writes structured NDJSON to `--log` file and/or stderr.
//!
//! Replaces the `Option<&mut std::fs::File>` that was previously threaded through every
//! emit site.  When `suppress_stderr` is set, events go to `--log` only — used by
//! `loop --tune` so JSON lines do not interleave with the ratatui TUI.

use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;

pub struct Logger {
    file: Option<std::fs::File>,
    suppress_stderr: bool,
}

impl Logger {
    /// Opens (or creates, append-mode) a log file at `path`, or returns a no-op
    /// logger when `path` is `None`.
    ///
    /// # Errors
    /// Returns `io::Error` when the file cannot be opened or created.
    pub fn open(path: Option<&Path>) -> io::Result<Self> {
        match path {
            Some(p) => {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?;
                Ok(Self {
                    file: Some(file),
                    suppress_stderr: false,
                })
            }
            None => Ok(Self::none()),
        }
    }

    /// No-op logger: no file, no stderr output.
    #[must_use]
    pub fn none() -> Self {
        Self {
            file: None,
            suppress_stderr: false,
        }
    }

    /// Suppresses stderr; events are written to `--log` only.
    #[must_use]
    pub fn suppressed(self) -> Self {
        Self {
            suppress_stderr: true,
            ..self
        }
    }

    /// Writes a structured JSON event to stderr (unless suppressed) and to `--log` if set.
    pub fn write_event<T: Serialize>(&mut self, record: &T) {
        let Ok(json) = serde_json::to_string(record) else {
            return;
        };
        if !self.suppress_stderr {
            eprintln!("{json}");
        }
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{json}");
        }
    }

    /// Writes an iteration record to `--log` only (never to stderr).
    pub fn write_iteration_line<T: Serialize>(&mut self, record: &T) {
        if let Some(f) = &mut self.file
            && let Ok(json) = serde_json::to_string(record)
        {
            let _ = writeln!(f, "{json}");
        }
    }
}

#[cfg(test)]
impl Logger {
    #[must_use]
    pub fn from_file(file: std::fs::File) -> Self {
        Self {
            file: Some(file),
            suppress_stderr: false,
        }
    }

    #[must_use]
    pub fn into_file(self) -> Option<std::fs::File> {
        self.file
    }
}
