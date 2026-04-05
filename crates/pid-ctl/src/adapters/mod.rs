//! Minimal PV/CV I/O adapters for the current `once` / `pipe` execution paths.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use wait_timeout::ChildExt;

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
/// Each read waits on the shell child for at most [`CmdPvSource`]'s `timeout`.
/// On timeout the child process group is killed where supported (Unix), so
/// hung commands do not block the controller indefinitely.
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

fn join_stdout_reader(handle: thread::JoinHandle<io::Result<String>>) -> io::Result<String> {
    handle
        .join()
        .map_err(|_| io::Error::other("PV command stdout reader panicked"))?
}

/// Best-effort: terminate the whole process group (Unix) so `sh -c` children
/// cannot outlive the timeout. Falls back to [`Child::kill`] on other targets.
fn kill_child_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

impl PvSource for CmdPvSource {
    fn read_pv(&mut self) -> io::Result<f64> {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &self.command]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture command stdout"))?;

        let reader = thread::spawn(move || {
            let mut s = String::new();
            let mut stdout = stdout;
            stdout.read_to_string(&mut s)?;
            Ok(s)
        });

        let outcome = child.wait_timeout(self.timeout);
        let (status, output) = match outcome {
            Ok(Some(status)) => {
                let output = join_stdout_reader(reader)?;
                (status, output)
            }
            Ok(None) => {
                kill_child_process_tree(&mut child);
                let _ = child.wait();
                let _ = join_stdout_reader(reader);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "PV command timed out after {:?}: `{}`",
                        self.timeout, self.command
                    ),
                ));
            }
            Err(e) => {
                kill_child_process_tree(&mut child);
                let _ = child.wait();
                let _ = join_stdout_reader(reader);
                return Err(e);
            }
        };

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
