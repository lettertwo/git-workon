use std::ffi::OsStr;
use std::path::Path;

use clap::builder::StyledStr;
use clap::Command;
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use workon::WorktreeDescriptor;

use crate::display::worktree_display_row;

fn worktree_help(wt: &WorktreeDescriptor, root: &Path, current_dir: &Path) -> Option<StyledStr> {
    let Ok(row) = worktree_display_row(wt, root, current_dir) else {
        return None;
    };

    let mut parts = Vec::new();

    if row.is_active {
        parts.push("→".to_string());
    }
    if !row.indicators.is_empty() {
        parts.push(row.indicators.join(" "));
    }
    if !row.last_activity.is_empty() {
        parts.push(row.last_activity);
    }
    if let Some(ann) = row.branch_annotation {
        parts.push(ann);
    }

    Some(StyledStr::from(parts.join("  ")))
}

pub fn complete_worktree_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(repo) = workon::get_repo(None) else {
        return vec![];
    };
    let Ok(worktrees) = workon::get_worktrees(&repo) else {
        return vec![];
    };
    let Ok(root) = workon::workon_root(&repo) else {
        return vec![];
    };
    let current_dir = std::env::current_dir().unwrap_or_default();
    let prefix = current.to_string_lossy();
    worktrees
        .iter()
        .filter(|wt| wt.name().is_some_and(|n| n.starts_with(prefix.as_ref())))
        .map(|wt| {
            let name = wt.name().unwrap();
            CompletionCandidate::new(name).help(worktree_help(wt, root, &current_dir))
        })
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
        .map(CompletionCandidate::new)
        .collect()
}

/// Surface `git-workon-*` executables on `$PATH` as top-level subcommands, so `git workon <TAB>`
/// offers them alongside the built-ins (mirroring `git`'s / `cargo`'s plugin completion). A name
/// already claimed by a built-in is skipped — the built-in owns it (dispatch enforces the same
/// precedence). Each is a bare stub `Command`; per-external argument completion (delegating into the
/// external's own `COMPLETE=` responder) is a separate concern, not wired here.
fn augment_external_subcommands(cmd: Command) -> Command {
    let known = crate::dispatch::known_subcommands(&cmd);
    crate::dispatch::external_subcommand_names()
        .into_iter()
        .filter(|name| !known.contains(name))
        .fold(cmd, |cmd, name| {
            // `Command::new` needs `&'static str` and clap's `Str` won't take an owned `String`.
            // Leaking is fine here: `augment` only runs inside the ephemeral `COMPLETE=` completer
            // process, which emits candidates and exits immediately — the handful of leaked names
            // live microseconds, never accumulating across invocations.
            let name: &'static str = String::leak(name);
            cmd.subcommand(Command::new(name).about("External git-workon subcommand"))
        })
}

pub fn augment(cmd: Command) -> Command {
    let cmd = augment_external_subcommands(cmd);
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
    .mut_subcommand("copy", |sub| {
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
