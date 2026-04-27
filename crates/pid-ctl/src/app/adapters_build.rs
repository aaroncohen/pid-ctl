//! PV source and CV sink configuration enums plus their builder functions.
//!
//! `OncePvSource`, `LoopPvSource`, and `CvSinkConfig` live here so the library can own
//! `build_loop_pv_source` / `build_cv_sink` without depending on the binary's CLI types.
//! The binary's `cli/types.rs` re-exports them.
//!
//! PV sources are split per subcommand so the type system encodes which inputs are valid
//! for each mode: `once` accepts a literal, a file, or a command; `loop` accepts a file,
//! a command, or stdin. This removes the runtime `unreachable!()` panics that would fire
//! if CLI validation ever missed a case.

use crate::adapters::{
    CmdCvSink, CmdPvSource, CvSink, DryRunCvSink, FileCvSink, FilePvSource, PvSource,
    StdinPvSource, StdoutCvSink,
};
use std::path::PathBuf;
use std::time::Duration;

/// Feed-forward source for the `once` subcommand.
#[derive(Clone, Debug, PartialEq)]
pub enum OnceFfSource {
    Zero,
    Literal(f64),
    File(PathBuf),
    Cmd(String),
}

/// Feed-forward source for the `loop` subcommand.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopFfSource {
    Zero,
    File(PathBuf),
    Cmd(String),
}

/// Reads one FF value for `once`, returning `0.0` on any error.
#[must_use]
pub fn resolve_once_ff(source: &OnceFfSource, cmd_timeout: Duration) -> f64 {
    match source {
        OnceFfSource::Zero => 0.0,
        OnceFfSource::Literal(v) => *v,
        OnceFfSource::File(path) => FilePvSource::new(path.clone()).read_pv().unwrap_or(0.0),
        OnceFfSource::Cmd(cmd) => CmdPvSource::new(cmd.clone(), cmd_timeout)
            .read_pv()
            .unwrap_or(0.0),
    }
}

/// Builds a reusable `Box<dyn PvSource>` for reading FF each loop tick.
/// Returns `None` when no FF source is configured.
#[must_use]
pub fn build_loop_ff_source(
    source: &LoopFfSource,
    cmd_timeout: Duration,
) -> Option<Box<dyn PvSource>> {
    match source {
        LoopFfSource::Zero => None,
        LoopFfSource::File(path) => Some(Box::new(FilePvSource::new(path.clone()))),
        LoopFfSource::Cmd(cmd) => Some(Box::new(CmdPvSource::new(cmd.clone(), cmd_timeout))),
    }
}

/// PV source accepted by the `once` subcommand (no stdin — `once` reads PV once).
#[derive(Clone, Debug, PartialEq)]
pub enum OncePvSource {
    Literal(f64),
    File(PathBuf),
    Cmd(String),
}

/// PV source accepted by the `loop` subcommand (no literal — `loop` reads PV every tick).
#[derive(Clone, Debug, PartialEq)]
pub enum LoopPvSource {
    File(PathBuf),
    Cmd(String),
    /// `loop --pv-stdin`: one line per tick, with a per-tick timeout.
    Stdin,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CvSinkConfig {
    Stdout,
    File {
        path: PathBuf,
        verify: bool,
    },
    Cmd {
        command: String,
        timeout: Option<Duration>,
    },
}

/// What to do with computed CV values.
///
/// Either discard (dry-run) or forward to a configured sink. Parse-time
/// validation rejects the invalid fourth state (no sink, no dry-run), so
/// matching is total and the runtime cannot hit an `unwrap`/`expect`.
#[derive(Clone, Debug, PartialEq)]
pub enum CvMode {
    DryRun,
    Sink(CvSinkConfig),
}

#[must_use]
pub fn build_loop_pv_source(
    source: &LoopPvSource,
    cmd_timeout: Duration,
    pv_stdin_timeout: Duration,
) -> Box<dyn PvSource> {
    match source {
        LoopPvSource::File(path) => Box::new(FilePvSource::new(path.clone())),
        LoopPvSource::Cmd(cmd) => Box::new(CmdPvSource::new(cmd.clone(), cmd_timeout)),
        LoopPvSource::Stdin => Box::new(StdinPvSource::new(pv_stdin_timeout)),
    }
}

#[must_use]
pub fn build_cv_mode_sink(
    cv_mode: &CvMode,
    precision: usize,
    default_cmd_timeout: Duration,
) -> Box<dyn CvSink> {
    match cv_mode {
        CvMode::DryRun => Box::new(DryRunCvSink),
        CvMode::Sink(cfg) => build_cv_sink(cfg, precision, default_cmd_timeout),
    }
}

#[must_use]
pub fn build_cv_sink(
    cv_sink: &CvSinkConfig,
    precision: usize,
    default_cmd_timeout: Duration,
) -> Box<dyn CvSink> {
    match cv_sink {
        CvSinkConfig::Stdout => Box::new(StdoutCvSink { precision }),
        CvSinkConfig::File { path, verify } => {
            let mut sink = FileCvSink::new(path.clone());
            sink.precision = precision;
            sink.verify = *verify;
            Box::new(sink)
        }
        CvSinkConfig::Cmd { command, timeout } => {
            let effective_timeout = timeout.unwrap_or(default_cmd_timeout);
            Box::new(CmdCvSink::new(
                command.clone(),
                effective_timeout,
                precision,
            ))
        }
    }
}
