use crate::{CliError, StatusFlags};
use pid_ctl::app::{self, StateSnapshot, StateStore};
use std::io::{self, Write};
use std::path::Path;

#[cfg(unix)]
use pid_ctl::socket::{Request, SocketError, client_request};

pub(crate) fn run_status_dispatch(flags: &StatusFlags) -> Result<(), CliError> {
    // Try socket first if provided (Unix only).
    #[cfg(unix)]
    if let Some(ref socket_path) = flags.socket_path {
        match client_request(socket_path, &Request::Status) {
            Ok(response) => {
                let json = serde_json::to_string(&response)
                    .map_err(|e| CliError::new(1, e.to_string()))?;
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                writeln!(handle, "{json}")
                    .map_err(|e| CliError::new(1, format!("stdout write failed: {e}")))?;
                return Ok(());
            }
            Err(SocketError::Io(ref e)) if e.kind() == io::ErrorKind::ConnectionRefused => {
                if flags.state_path.is_none() {
                    return Err(CliError::new(
                        1,
                        format!(
                            "socket connection refused at {} (no --state fallback)",
                            socket_path.display()
                        ),
                    ));
                }
                eprintln!("socket connection refused, falling back to state file");
            }
            Err(e) => return Err(CliError::new(1, format!("socket: {e}"))),
        }
    }

    // Fall back to state file.
    if let Some(ref state_path) = flags.state_path {
        run_status(state_path)
    } else {
        Err(CliError::config("status requires --state or --socket"))
    }
}

pub(crate) fn run_purge(state_path: &Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let _lock = store
        .acquire_lock()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    let snapshot = store
        .load()
        .map_err(|error| CliError::new(1, error.to_string()))?
        .ok_or_else(|| {
            CliError::new(1, format!("{}: state file not found", state_path.display()))
        })?;

    let purged = StateSnapshot {
        schema_version: snapshot.schema_version,
        name: snapshot.name,
        kp: snapshot.kp,
        ki: snapshot.ki,
        kd: snapshot.kd,
        setpoint: snapshot.setpoint,
        out_min: snapshot.out_min,
        out_max: snapshot.out_max,
        created_at: snapshot.created_at,
        updated_at: Some(app::now_iso8601()),
        // Runtime fields cleared:
        i_acc: 0.0,
        last_pv: None,
        last_error: None,
        last_cv: None,
        iter: 0,
        effective_sp: None,
        target_sp: None,
    };

    store
        .save(&purged)
        .map_err(|error| CliError::new(1, error.to_string()))?;

    eprintln!("purged runtime state from {}", state_path.display());

    Ok(())
}

pub(crate) fn run_init(state_path: &Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let _lock = store
        .acquire_lock()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    if state_path.exists() {
        std::fs::remove_file(state_path).map_err(|error| {
            CliError::new(
                1,
                format!("{}: failed to remove: {error}", state_path.display()),
            )
        })?;
    }

    let now = app::now_iso8601();
    let fresh = StateSnapshot {
        created_at: Some(now.clone()),
        updated_at: Some(now),
        ..StateSnapshot::default()
    };

    store
        .save(&fresh)
        .map_err(|error| CliError::new(1, error.to_string()))?;

    eprintln!("initialized state file {}", state_path.display());

    Ok(())
}

fn run_status(state_path: &Path) -> Result<(), CliError> {
    let store = StateStore::new(state_path);
    let snapshot = store
        .load()
        .map_err(|error| CliError::new(1, error.to_string()))?
        .ok_or_else(|| {
            CliError::new(1, format!("{}: state file not found", state_path.display()))
        })?;

    let json = snapshot
        .to_json_string()
        .map_err(|error| CliError::new(1, error.to_string()))?;

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{json}")
        .map_err(|error| CliError::new(1, format!("stdout write failed: {error}")))?;

    Ok(())
}
