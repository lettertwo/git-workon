use miette::{bail, Result};

use crate::cli::Switch;

use super::Run;

// Given no arguments, workon will show an interactive list of worktrees to choose from.
//
// If [name] is provided, workon will attempt to switch to that worktree.
// If a matching worktree is found, it will be switched to.
// If there are multiple matches, an interactive list of the the matching worktrees will be shown.
//
// If no worktree matches are found, prompt to switch to the new worktree flow.

impl Run for Switch {
    fn run(&self) -> Result<()> {
        unimplemented!("switch {:?} not implemented!", self.name);
    }
}
