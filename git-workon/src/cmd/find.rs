use miette::{bail, Result, WrapErr};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Find;

use super::Run;

// Given no arguments, workon will show an interactive list of worktrees to choose from.
//
// If [name] is provided, workon will attempt to match that worktree.
// If a matching worktree is found, it will be printed to stdout.
// If there are multiple matches, an interactive list of the the matching worktrees will be shown.
//
// If no worktree matches are found, prompt to switch to the new worktree flow.

impl Run for Find {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let name = match &self.name {
            Some(name) => name,
            None => {
                unimplemented!("Interactive find not implemented!");
            }
        };
        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let worktrees = get_worktrees(&repo).wrap_err("Failed to list worktrees in repository")?;
        for worktree in worktrees {
            match worktree.name() {
                // TODO: Fuzzy match on name,
                // maybe with inteactive selection if there are multiple hits
                Some(worktree_name) if worktree_name == name => {
                    return Ok(Some(worktree));
                }
                _ => {}
            }
        }
        bail!("No matching worktree found!")
    }
}
