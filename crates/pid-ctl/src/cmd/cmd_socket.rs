use crate::{CliError, SetRawArgs, SocketOnlyArgs, get_socket_path, parse_set_args};
use std::io::{self, Write};
use std::path::Path;

pub(crate) fn run_socket_hold(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Hold, "hold")
}

pub(crate) fn run_socket_resume(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Resume, "resume")
}

pub(crate) fn run_socket_reset(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Reset, "reset")
}

pub(crate) fn run_socket_save(raw: &SocketOnlyArgs) -> Result<(), CliError> {
    let path = get_socket_path(raw);
    socket_send_and_print(&path, &pid_ctl::socket::Request::Save, "save")
}

pub(crate) fn run_socket_set(raw: &SetRawArgs) -> Result<(), CliError> {
    let parsed = parse_set_args(raw);
    let req = pid_ctl::socket::Request::Set {
        param: parsed.param,
        value: parsed.value,
    };
    socket_send_and_print(&parsed.socket_path, &req, "set")
}

fn socket_send_and_print(
    socket_path: &Path,
    req: &pid_ctl::socket::Request,
    cmd: &str,
) -> Result<(), CliError> {
    match pid_ctl::socket::client_request(socket_path, req) {
        Ok(response) => {
            let json =
                serde_json::to_string(&response).map_err(|e| CliError::new(1, e.to_string()))?;
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            writeln!(handle, "{json}")
                .map_err(|e| CliError::new(1, format!("stdout write failed: {e}")))?;
            // Mirror ok:false as a non-zero exit so callers can detect failure without parsing JSON.
            let ok = serde_json::to_value(&response)
                .ok()
                .and_then(|v| v["ok"].as_bool())
                .unwrap_or(true);
            if ok {
                Ok(())
            } else {
                Err(CliError::new(1, String::new()))
            }
        }
        Err(e) => Err(CliError::new(1, format!("{cmd}: socket error: {e}"))),
    }
}
