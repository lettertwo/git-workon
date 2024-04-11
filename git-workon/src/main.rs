mod cli;
mod cmd;

use clap::Parser;
use cli::Cmd;
use miette::Result;

use crate::cli::Cli;
use crate::cmd::Run;

fn main() -> Result<()> {
    let mut cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .init();

    if cli.command.is_none() {
        cli.command = Some(Cmd::Switch(cli.switch));
    }

    cli.command.unwrap().run()
}
