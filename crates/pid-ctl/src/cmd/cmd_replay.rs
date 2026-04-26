use crate::{CliError, ReplayArgs};
use pid_ctl::app::replay::{self, DiffSummary};
use std::fs::File;
use std::io::BufReader;

pub(crate) fn run_replay(args: &ReplayArgs) -> Result<(), CliError> {
    let file = File::open(&args.log_path).map_err(|e| {
        CliError::new(
            1,
            format!("failed to open log file {}: {e}", args.log_path.display()),
        )
    })?;
    let reader = BufReader::new(file);

    let mut out_file: Option<File> = if let Some(ref path) = args.output_log {
        let f = File::create(path).map_err(|e| {
            CliError::new(
                1,
                format!("failed to create output log {}: {e}", path.display()),
            )
        })?;
        Some(f)
    } else {
        None
    };

    let output: Option<&mut dyn std::io::Write> =
        out_file.as_mut().map(|f| f as &mut dyn std::io::Write);

    let summary = replay::replay(
        reader,
        args.pid_config.clone(),
        args.setpoint_from_cli,
        output,
    )
    .map_err(|e| CliError::new(1, e.to_string()))?;

    if args.diff {
        print_diff(&summary);
    }

    Ok(())
}

fn print_diff(s: &DiffSummary) {
    let line = serde_json::json!({
        "n": s.n,
        "max_diff": s.max_diff,
        "rms_diff": s.rms_diff,
    });
    println!("{line}");
}
