mod error;
mod output;
mod parse;
mod raw;
mod types;
pub(crate) mod user_set;

pub(crate) use error::CliError;
pub(crate) use output::print_iteration_json;
#[cfg(feature = "tui")]
pub(crate) use parse::parse_duration_flag;
#[cfg(unix)]
pub(crate) use parse::{get_socket_path, parse_set_args};
pub(crate) use parse::{
    get_state_path, parse_f64_value, parse_loop, parse_once, parse_pipe, parse_status_flags,
    resolve_pv,
};
pub(crate) use raw::{Cli, SubCommand};
#[cfg(unix)]
pub(crate) use raw::{SetRawArgs, SocketOnlyArgs};
pub(crate) use types::{
    LoopArgs, LoopRuntimeConfig, OnceArgs, OutputFormat, PipeArgs, StatusFlags,
};
