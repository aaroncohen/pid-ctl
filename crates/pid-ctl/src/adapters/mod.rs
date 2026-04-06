//! PV/CV I/O adapters for `once`, `pipe`, and `loop` execution paths.

use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

/// A no-op CV sink used when `--dry-run` is active.
///
/// PID computation and state persistence proceed normally; the computed CV is
/// simply discarded instead of being forwarded to the actuator.
pub struct DryRunCvSink;

impl CvSink for DryRunCvSink {
    fn write_cv(&mut self, _cv: f64) -> io::Result<()> {
        Ok(())
    }
}

/// Writes controller output to a file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCvSink {
    path: PathBuf,
    pub precision: usize,
    /// When `true`, re-reads the file after writing to confirm the value was accepted.
    pub verify: bool,
}

impl FileCvSink {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            precision: 2,
            verify: false,
        }
    }
}

impl CvSink for FileCvSink {
    fn write_cv(&mut self, cv: f64) -> io::Result<()> {
        let formatted = format!("{cv:.prec$}\n", prec = self.precision);
        fs::write(&self.path, &formatted)?;

        if self.verify {
            let readback_str = fs::read_to_string(&self.path)?;
            let readback: f64 = readback_str.trim().parse().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "--verify-cv: readback from {} could not be parsed: {e}",
                        self.path.display()
                    ),
                )
            })?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let exp = self.precision as i32;
            let scale = 10_f64.powi(exp);
            let written_rounded = (cv * scale).round();
            let readback_rounded = (readback * scale).round();
            if (written_rounded - readback_rounded).abs() >= 1.0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "--verify-cv: readback mismatch at {}: wrote {cv:.prec$}, read back {readback:.prec$}",
                        self.path.display(),
                        prec = self.precision,
                    ),
                ));
            }
        }

        Ok(())
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
    timeout: Duration,
}

impl CmdPvSource {
    /// Creates a new [`CmdPvSource`].
    #[must_use]
    pub fn new(command: String, timeout: Duration) -> Self {
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

// ---------------------------------------------------------------------------
// Stdin PV source (loop --pv-stdin)
// ---------------------------------------------------------------------------

/// Reads one line from stdin per tick, blocking up to a configurable timeout.
///
/// Each call to [`read_pv`] blocks for at most `timeout` waiting for a line.
/// On timeout, returns an [`io::Error`] with [`io::ErrorKind::TimedOut`].
/// The line is trimmed and parsed as [`f64`].
///
/// [`read_pv`]: PvSource::read_pv
#[derive(Debug)]
pub struct StdinPvSource {
    timeout: Duration,
}

impl StdinPvSource {
    /// Creates a new [`StdinPvSource`] that blocks for at most `timeout` per read.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl PvSource for StdinPvSource {
    fn read_pv(&mut self) -> io::Result<f64> {
        let timeout = self.timeout;

        let (tx, rx) = mpsc::channel::<io::Result<String>>();

        thread::spawn(move || {
            let stdin = io::stdin();
            let mut line = String::new();
            let result = stdin.lock().read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });

        match rx.recv_timeout(timeout) {
            Ok(Ok(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "--pv-stdin: stdin reached EOF",
                    ));
                }
                trimmed.parse::<f64>().map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("--pv-stdin: cannot parse PV from stdin line {trimmed:?}: {e}"),
                    )
                })
            }
            Ok(Err(e)) => Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("--pv-stdin: timed out after {timeout:?} waiting for stdin line"),
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "--pv-stdin: stdin reader thread disconnected unexpectedly",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// CmdCvSink
// ---------------------------------------------------------------------------

/// Executes a shell command to deliver the CV value.
///
/// The command template is a shell string with `{cv}` and/or `{cv:url}`
/// placeholders.  Before execution:
/// - `{cv}` is replaced with the CV formatted to `precision` decimal places.
/// - `{cv:url}` is replaced with the same string percent-encoded (RFC 3986
///   unreserved characters are not encoded; every other byte is `%XX`).
///
/// The command runs as `sh -c <command>` in its own process group (Unix) so
/// the entire tree can be killed on timeout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdCvSink {
    command_template: String,
    timeout: Duration,
    pub precision: usize,
}

impl CmdCvSink {
    /// Creates a new [`CmdCvSink`].
    #[must_use]
    pub fn new(command_template: String, timeout: Duration, precision: usize) -> Self {
        Self {
            command_template,
            timeout,
            precision,
        }
    }

    fn build_command(&self, cv: f64) -> String {
        let cv_str = format!("{cv:.prec$}", prec = self.precision);
        let cv_url = percent_encode(&cv_str);
        self.command_template
            .replace("{cv:url}", &cv_url)
            .replace("{cv}", &cv_str)
    }
}

/// Percent-encodes a string, leaving RFC 3986 unreserved characters as-is.
///
/// Unreserved characters: `A-Z a-z 0-9 - _ . ~`
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

impl CvSink for CmdCvSink {
    fn write_cv(&mut self, cv: f64) -> io::Result<()> {
        let command = self.build_command(cv);

        let mut cmd = Command::new("sh");
        cmd.args(["-c", &command]);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;

        let outcome = child.wait_timeout(self.timeout);
        match outcome {
            Ok(Some(status)) => {
                if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "CV command exited with {status}: {command}"
                    )))
                }
            }
            Ok(None) => {
                kill_child_process_tree(&mut child);
                let _ = child.wait();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("CV command timed out after {:?}: `{command}`", self.timeout),
                ))
            }
            Err(e) => {
                kill_child_process_tree(&mut child);
                let _ = child.wait();
                Err(e)
            }
        }
    }
}
