use crate::CliError;
use std::io::Write;

pub(crate) fn print_iteration_json(record: &pid_ctl::app::IterationRecord) -> Result<(), CliError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, record)
        .map_err(|error| CliError::new(3, format!("failed to serialize JSON output: {error}")))?;
    writeln!(handle).map_err(|error| CliError::new(1, format!("stdout write failed: {error}")))
}
