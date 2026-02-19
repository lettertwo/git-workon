use std::ffi::OsStr;

use clap::Command;
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

pub fn complete_worktree_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(repo) = workon::get_repo(None) else {
        return vec![];
    };
    let Ok(worktrees) = workon::get_worktrees(&repo) else {
        return vec![];
    };
    let prefix = current.to_string_lossy();
    worktrees
        .iter()
        .filter_map(|wt| wt.name())
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(|name| CompletionCandidate::new(name))
        .collect()
}

pub fn complete_branch_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(repo) = workon::get_repo(None) else {
        return vec![];
    };
    let Ok(branches) = repo.branches(None) else {
        return vec![];
    };
    let prefix = current.to_string_lossy();
    branches
        .filter_map(|b| b.ok())
        .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(|name| CompletionCandidate::new(name))
        .collect()
}

pub fn augment(cmd: Command) -> Command {
    cmd.mut_arg("name", |a| {
        a.add(ArgValueCompleter::new(complete_worktree_names))
    })
    .mut_subcommand("find", |sub| {
        sub.mut_arg("name", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("prune", |sub| {
        sub.mut_arg("names", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("move", |sub| {
        sub.mut_arg("names", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("copy-untracked", |sub| {
        sub.mut_arg("from", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
        .mut_arg("to", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("new", |sub| {
        sub.mut_arg("base", |a| {
            a.add(ArgValueCompleter::new(complete_branch_names))
        })
    })
}
