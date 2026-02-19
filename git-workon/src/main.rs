mod cli;
mod cmd;
mod completers;
mod display;
mod hooks;
mod json;
mod output;

use clap::{CommandFactory, Parser};
use clap_complete::env::CompleteEnv;
use cli::Cmd;
use miette::{IntoDiagnostic, Result};

use crate::cli::Cli;
use crate::cmd::Run;
use crate::json::worktree_to_json;

fn main() -> Result<()> {
    CompleteEnv::with_factory(|| completers::augment(Cli::command())).complete();

    let mut cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .format(|buf, record| {
            use std::io::Write;
            writeln!(buf, "{}", record.args())
        })
        .init();

    let json_mode = cli.json;

    if json_mode {
        output::set_json_mode(true);
    }

    if cli.no_color {
        output::set_no_color(true);
    }

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

    let mut cmd = cli.command.unwrap();

    // Propagate --json to commands that handle it internally
    if json_mode {
        match &mut cmd {
            Cmd::List(list) => list.json = true,
            Cmd::Prune(prune) => prune.json = true,
            Cmd::Find(find) => find.no_interactive = true,
            _ => {}
        }
    }

    let worktree = cmd.run()?;

    if json_mode {
        if let Some(wt) = worktree {
            let json = serde_json::to_string_pretty(&worktree_to_json(&wt)).into_diagnostic()?;
            println!("{}", json);
        }
        // list/prune already printed their JSON in run()
        // other None cases: output nothing (valid for commands that don't return a worktree)
    } else if let Some(worktree) = worktree {
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
            copy_untracked: false,
            no_copy_untracked: false,
            no_interactive: false,
        })),
    }
}
