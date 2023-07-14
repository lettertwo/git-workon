mod cli;
mod cmd;

use clap::Parser;
use log::error;
use std::process::ExitCode;

use crate::cli::Cli;
use crate::cmd::Run;

fn main() -> ExitCode {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .init();

    match cli.command {
        Some(cmd) => match cmd.run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!("{}", e);
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        None => {
            eprintln!(
                "Invalid subcommand {:?}. Try --help for more information.",
                cli.command
            );
            ExitCode::FAILURE
        }
    }
}
