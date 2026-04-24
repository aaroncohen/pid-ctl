mod cli;
mod cmd;
#[cfg(feature = "tui")]
mod tune;
#[allow(clippy::wildcard_imports)]
pub(crate) use cli::*;
pub(crate) use cmd::{run_init, run_loop, run_once, run_pipe, run_purge, run_status_dispatch};
#[cfg(unix)]
pub(crate) use cmd::{
    run_socket_hold, run_socket_reset, run_socket_resume, run_socket_save, run_socket_set,
};

use clap::Parser;
use std::process;

fn main() {
    let full_argv: Vec<String> = std::env::args().collect();

    let cli = Cli::try_parse().unwrap_or_else(|e| {
        // --help and --version print to stdout and exit 0 (normal clap behaviour).
        // All other parse errors are config errors → exit 3 (same as hand-rolled parser).
        match e.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                print!("{e}");
                process::exit(0);
            }
            _ => {
                eprintln!("{e}");
                process::exit(3);
            }
        }
    });

    let exit_code = match run(cli, &full_argv) {
        Ok(()) => 0,
        Err(error) => {
            if !error.message.is_empty() {
                eprintln!("{error}");
            }
            error.exit_code
        }
    };

    process::exit(exit_code);
}

fn run(
    cli: Cli,
    #[cfg_attr(not(feature = "tui"), allow(unused_variables))] full_argv: &[String],
) -> Result<(), CliError> {
    match cli.command {
        SubCommand::Once(raw) => {
            let parsed = parse_once(&raw)?;
            run_once(&parsed)
        }
        SubCommand::Pipe(raw) => {
            let parsed = parse_pipe(&raw)?;
            run_pipe(&parsed)
        }
        SubCommand::Loop(raw) => {
            let mut parsed = parse_loop(&raw)?;
            #[cfg(feature = "tui")]
            if parsed.tune {
                return tune::run(parsed, full_argv);
            }
            #[cfg(not(feature = "tui"))]
            if parsed.tune {
                return Err(CliError::config(
                    "--tune requires the 'tui' feature (not compiled in)",
                ));
            }
            run_loop(&mut parsed)
        }
        SubCommand::Status(raw) => {
            let flags = parse_status_flags(&raw)?;
            run_status_dispatch(&flags)
        }
        #[cfg(unix)]
        SubCommand::Set(raw) => run_socket_set(&raw),
        #[cfg(unix)]
        SubCommand::Hold(raw) => run_socket_hold(&raw),
        #[cfg(unix)]
        SubCommand::Resume(raw) => run_socket_resume(&raw),
        #[cfg(unix)]
        SubCommand::Reset(raw) => run_socket_reset(&raw),
        #[cfg(unix)]
        SubCommand::Save(raw) => run_socket_save(&raw),
        SubCommand::Purge(raw) => {
            let state_path =
                get_state_path(&raw).map_err(|_| CliError::config("purge requires --state"))?;
            run_purge(&state_path)
        }
        SubCommand::Init(raw) => {
            let state_path =
                get_state_path(&raw).map_err(|_| CliError::config("init requires --state"))?;
            run_init(&state_path)
        }
    }
}
