mod cli;
mod cmd;
mod completers;
mod display;
mod hooks;
mod json;
mod output;
mod picker;

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

    // Resolve the stack model once up front so the routing functions can consult it
    // without re-opening the repository or re-reading config.
    let routing_repo = workon::get_repo(None).ok();
    let routing_model = match (&routing_repo, cli.no_stack) {
        (Some(repo), false) => workon::WorkonConfig::new(repo)
            .ok()
            .and_then(|c| c.stack_model(None).ok())
            .unwrap_or(workon::StackModel::None),
        _ => workon::StackModel::None,
    };

    if cli.command.is_none() {
        // Clone the name so cli.find can be moved freely into Cmd::Find below.
        let name_opt = cli.find.name.clone();
        match name_opt {
            Some(name) if workon::is_pr_reference(&name) => {
                cli.command = route_pr_ref_to_command(&name, routing_repo.as_ref())
                    .or(Some(Cmd::Find(cli.find)));
            }
            Some(name) => {
                if cli.find.new {
                    cli.command = Some(Cmd::New(cli::New::attach(name)));
                } else {
                    cli.command =
                        route_branch_to_command(routing_repo.as_ref(), &name, routing_model)?
                            .or(Some(Cmd::Find(cli.find)));
                }
            }
            None => {
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
            Cmd::Doctor(doctor) => doctor.json = true,
            Cmd::Find(find) => find.no_interactive = true,
            Cmd::New(new) => new.no_interactive = true,
            Cmd::Checkout(c) => c.no_interactive = true,
            _ => {}
        }
    }

    // Propagate --no-stack to commands that have stack-aware behavior
    if cli.no_stack {
        match &mut cmd {
            Cmd::List(list) => list.no_stack = true,
            Cmd::Find(find) => find.no_stack = true,
            Cmd::New(new) => new.no_stack = true,
            Cmd::Checkout(checkout) => checkout.no_stack = true,
            _ => {}
        }
    }

    let worktree = match cmd.run() {
        Ok(wt) => wt,
        Err(ref e) if json_mode => {
            let code = e.code().map(|c| c.to_string());
            let msg = e.to_string();
            let json = serde_json::json!({"error": {"code": code, "message": msg}});
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
            std::process::exit(1);
        }
        Err(e) => return Err(e),
    };

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

/// Route a bare `workon <name>` to the appropriate `Cmd`.
///
/// Maps [`workon::resolve_action`] output to a `Cmd`:
/// - [`Navigate`] / [`NotFound`] → `None` (falls through to `Cmd::Find`)
/// - [`Materialize`] → `Some(Cmd::New)`
/// - [`Checkout`] → `Some(Cmd::Checkout)` *(added in PR-2)*
/// - [`DeletedNode`] → structured error (`StackError::DeletedBranchNode`)
///
/// [`Navigate`]: workon::Resolution::Navigate
/// [`NotFound`]: workon::Resolution::NotFound
/// [`DeletedNode`]: workon::Resolution::DeletedNode
/// [`Materialize`]: workon::Resolution::Materialize
/// [`Checkout`]: workon::Resolution::Checkout
fn route_branch_to_command(
    repo: Option<&git2::Repository>,
    name: &str,
    model: workon::StackModel,
) -> miette::Result<Option<Cmd>> {
    let repo = match repo {
        Some(r) => r,
        None => return Ok(None),
    };
    match workon::resolve_action(repo, name, model) {
        workon::Resolution::Materialize => Ok(Some(Cmd::New(cli::New::attach(name)))),
        workon::Resolution::Checkout { host } => Ok(Some(Cmd::Checkout(cli::Checkout {
            branch: name.to_string(),
            host_worktree: host,
            no_stack: false,
            no_interactive: false,
        }))),
        workon::Resolution::DeletedNode { branch } => {
            miette::bail!(workon::StackError::DeletedBranchNode { branch })
        }
        // Navigate → Find handles the cd; NotFound → Find shows the error.
        _ => Ok(None),
    }
}

/// Returns `Some(Cmd::New)` if the PR worktree doesn't exist yet; `None` otherwise.
fn route_pr_ref_to_command(pr_ref: &str, repo: Option<&git2::Repository>) -> Option<Cmd> {
    let repo = repo?;
    let config = workon::WorkonConfig::new(repo).ok()?;
    let pr_format = config.pr_format(None).ok()?;
    let pr_info = workon::parse_pr_reference(pr_ref).ok()??;
    let pr_name = workon::format_pr_name(&pr_format, pr_info.number);

    match repo.find_worktree(&pr_name) {
        Ok(_) => None, // worktree already exists
        _ => Some(Cmd::New(cli::New::attach(pr_name))),
    }
}
