mod cmd;

use std::process::ExitCode;

use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use log::error;

use crate::cmd::{Cmd, Worktree};

#[derive(Debug, Parser)]
#[clap(
    about,
    author,
    bin_name = env!("CARGO_PKG_NAME"),
    disable_help_subcommand = true,
    propagate_version = true,
    version,
)]

struct Cli {
    #[clap(flatten)]
    pub verbose: Verbosity<InfoLevel>,
    #[command(subcommand)]
    pub command: Option<Cmd>,
    #[clap(flatten)]
    pub worktree: Worktree,
}

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
