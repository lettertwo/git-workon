mod cli;
mod cmd;
mod copy;
mod hooks;

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
        match cli.find.name {
            Some(ref name) if workon::is_pr_reference(name) => {
                cli.command = route_pr_ref_to_command(name).or(Some(Cmd::Find(cli.find)));
            }
            _ => {
                cli.command = Some(Cmd::Find(cli.find));
            }
        }
    }

    let worktree = cli.command.unwrap().run()?;

    if let Some(worktree) = worktree {
        if let Some(path_str) = worktree.path().to_str() {
            println!("{}", path_str);
        }
    }

    Ok(())
}

/// Returns `Some(Cmd::New)` if PR worktree doesn't exist yet; `None` if it exists or parsing fails.
fn route_pr_ref_to_command(pr_ref: &str) -> Option<Cmd> {
    let repo = workon::get_repo(None).ok()?;
    let config = workon::WorkonConfig::new(&repo).ok()?;
    let pr_format = config.pr_format(None).ok()?;
    let pr_info = workon::parse_pr_reference(pr_ref).ok()??;
    let pr_name = workon::format_pr_name(&pr_format, pr_info.number);

    match repo.find_worktree(&pr_name) {
        Ok(_) => None, // worktree already exists
        _ => Some(Cmd::New(cli::New {
            name: Some(pr_name),
            base: None,
            orphan: false,
            detach: false,
            no_hooks: false,
        })),
    }
}
