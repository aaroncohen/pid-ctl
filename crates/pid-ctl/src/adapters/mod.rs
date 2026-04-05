//! Minimal PV/CV I/O adapters for the current `once` / `pipe` execution paths.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// Destination for computed controller output values.
pub trait CvSink {
    /// Writes a single controller output value.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the sink cannot confirm that the value was
    /// written.
    fn write_cv(&mut self, cv: f64) -> io::Result<()>;
}

/// Writes controller output to stdout, one value per line.
#[derive(Debug)]
pub struct StdoutCvSink {
    pub precision: usize,
}

impl Default for StdoutCvSink {
    fn default() -> Self {
        Self { precision: 2 }
    }
}

impl CvSink for StdoutCvSink {
    fn write_cv(&mut self, cv: f64) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        writeln!(handle, "{cv:.prec$}", prec = self.precision)
    }
}

/// Writes controller output to a file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCvSink {
    path: PathBuf,
    pub precision: usize,
}

impl FileCvSink {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            precision: 2,
        }
    }
}

impl CvSink for FileCvSink {
    fn write_cv(&mut self, cv: f64) -> io::Result<()> {
        fs::write(&self.path, format!("{cv:.prec$}\n", prec = self.precision))
    }
}
