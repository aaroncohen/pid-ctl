//! PV source and CV sink configuration enums plus their builder functions.
//!
//! `PvSourceConfig` and `CvSinkConfig` live here so the library can own
//! `build_pv_source` / `build_cv_sink` without depending on the binary's CLI types.
//! The binary's `cli/types.rs` re-exports them.

use crate::adapters::{
    CmdCvSink, CmdPvSource, CvSink, FileCvSink, FilePvSource, PvSource, StdinPvSource,
    StdoutCvSink,
};
use std::path::PathBuf;
use std::time::Duration;

/// Which PV source was specified on the CLI.
#[derive(Clone, Debug, PartialEq)]
pub enum PvSourceConfig {
    Literal(f64),
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

#[must_use]
pub fn build_pv_source(
    source: &PvSourceConfig,
    cmd_timeout: Duration,
    pv_stdin_timeout: Duration,
) -> Box<dyn PvSource> {
    match source {
        PvSourceConfig::Literal(_) => unreachable!("loop rejects literal PV"),
        PvSourceConfig::File(path) => Box::new(FilePvSource::new(path.clone())),
        PvSourceConfig::Cmd(cmd) => Box::new(CmdPvSource::new(cmd.clone(), cmd_timeout)),
        PvSourceConfig::Stdin => Box::new(StdinPvSource::new(pv_stdin_timeout)),
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
