mod cli;
mod cmd;

use clap::Parser;
use miette::Result;

use crate::cli::Cli;
use crate::cmd::Run;

fn main() -> Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .init();

    cli.command.run()
}
