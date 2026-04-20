use super::error::CliError;
use pid_ctl::app::IterationRecord;
use std::io::{self, Write};

/// Serialize `record` as a single JSON line to stdout.
///
/// Returns a [`CliError`] if serialization or the stdout write fails.
pub(crate) fn print_iteration_json(record: &IterationRecord) -> Result<(), CliError> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, record)
        .map_err(|error| CliError::new(3, format!("failed to serialize JSON output: {error}")))?;
    writeln!(handle).map_err(|error| CliError::new(1, format!("stdout write failed: {error}")))
}
