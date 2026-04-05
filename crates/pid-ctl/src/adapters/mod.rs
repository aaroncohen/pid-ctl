//! Minimal PV/CV I/O adapters for the current `once` / `pipe` execution paths.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

// ---------------------------------------------------------------------------
// PV sources
// ---------------------------------------------------------------------------

/// Source for process variable readings.
pub trait PvSource {
    /// Reads a single PV value.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the value cannot be read or parsed.
    fn read_pv(&mut self) -> io::Result<f64>;
}

/// Reads a PV value from a file by parsing its entire contents as [`f64`].
///
/// Designed for sysfs-style files that contain a single number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePvSource {
    path: PathBuf,
}

impl FilePvSource {
    /// Creates a new [`FilePvSource`] that reads from `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl PvSource for FilePvSource {
    fn read_pv(&mut self) -> io::Result<f64> {
        let content = std::fs::read_to_string(&self.path)?;
        content.trim().parse::<f64>().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot parse PV from {}: {e}", self.path.display()),
            )
        })
    }
}

/// Executes a shell command and parses its stdout as [`f64`].
///
/// The `timeout` field is stored for future loop-mode enforcement; command
/// execution is currently synchronous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdPvSource {
    command: String,
    timeout: std::time::Duration,
}

impl CmdPvSource {
    /// Creates a new [`CmdPvSource`].
    #[must_use]
    pub fn new(command: String, timeout: std::time::Duration) -> Self {
        Self { command, timeout }
    }
}

impl PvSource for CmdPvSource {
    fn read_pv(&mut self) -> io::Result<f64> {
        let _ = self.timeout; // reserved for future kill-on-timeout logic

        let mut child = Command::new("sh")
            .args(["-c", &self.command])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture command stdout"))?;

        let mut output = String::new();
        stdout.read_to_string(&mut output)?;

        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "PV command exited with {status}: {}",
                self.command
            )));
        }

        output.trim().parse::<f64>().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot parse PV from command `{}`: {e}", self.command),
            )
        })
    }
}
