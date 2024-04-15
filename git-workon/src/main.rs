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
        cli.command = Some(Cmd::Find(cli.find));
    }

    let worktree = cli.command.unwrap().run()?;

    if let Some(worktree) = worktree {
        if let Some(path_str) = worktree.path().to_str() {
            println!("{}", path_str);
        }
    }

    Ok(())
}
