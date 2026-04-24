mod cmd_loop;
mod cmd_once;
mod cmd_pipe;
#[cfg(unix)]
mod cmd_socket;
mod cmd_state;

pub(crate) use cmd_loop::run_loop;
pub(crate) use cmd_once::run_once;
pub(crate) use cmd_pipe::run_pipe;
#[cfg(unix)]
pub(crate) use cmd_socket::{
    run_socket_hold, run_socket_reset, run_socket_resume, run_socket_save, run_socket_set,
};
pub(crate) use cmd_state::{run_init, run_purge, run_status_dispatch};

use pid_ctl::app::logger::Logger;
use std::path::Path;

pub(super) fn open_log(path: Option<&Path>) -> Result<Logger, crate::CliError> {
    Logger::open(path).map_err(|e| {
        crate::CliError::new(
            1,
            format!("failed to open log file {}: {e}", path.unwrap().display()),
        )
    })
}
