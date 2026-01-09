//! Move command CLI wrapper.
//!
//! CLI wrapper for the move_worktree library function (see git-workon-lib/src/move.rs
//! for the atomic operation implementation).
//!
//! ## Two Invocation Modes
//!
//! 1. **Single-arg**: `git workon move <new-name>` - Renames current worktree
//!    - Detects current worktree from working directory
//!    - Extracts branch name as source
//!    - Fails if not in a worktree or in detached HEAD
//!
//! 2. **Two-arg**: `git workon move <from> <to>` - Explicit source and target
//!    - Can run from anywhere (doesn't need to be in a worktree)
//!    - Explicitly specifies which worktree to rename
//!
//! ## Dry Run Mode
//!
//! `--dry-run` validates the operation and shows what would happen:
//! - Checks all safety constraints
//! - Shows branch and directory paths that would change
//! - No modifications made
//!
//! See git-workon-lib/src/move.rs for implementation details, safety checks,
//! and atomic operation strategy.

use miette::{bail, Context, Result};
use workon::{
    current_worktree, find_worktree, get_repo, move_worktree, validate_move, MoveOptions,
    WorktreeDescriptor,
};

use crate::cli::Move;

use super::Run;

impl Run for Move {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;

        // Parse arguments: either [to] or [from, to]
        let (from, to) = match self.names.len() {
            1 => {
                // Single argument: rename current worktree
                let current = current_worktree(&repo)?;
                let from = current
                    .branch()?
                    .ok_or_else(|| miette::miette!("Current worktree is detached HEAD"))?;
                (from, self.names[0].clone())
            }
            2 => {
                // Two arguments: explicit from and to
                (self.names[0].clone(), self.names[1].clone())
            }
            _ => {
                bail!("Expected 1 or 2 arguments, got {}", self.names.len());
            }
        };

        // Early exit for identical names
        if from == to {
            bail!("Source and target names are identical");
        }

        // Set up options
        let options = MoveOptions { force: self.force };

        if self.dry_run {
            let root = workon::workon_root(&repo)?;
            let source = find_worktree(&repo, &from)?;

            validate_move(&repo, &source, &to, &options)?;

            println!("Would move worktree '{}' to '{}'", from, to);
            println!("  Branch: {} → {}", from, to);
            println!(
                "  Path: {} → {}",
                source.path().display(),
                root.join(&to).display()
            );

            return Ok(None);
        }

        // Execute the move
        let worktree = move_worktree(&repo, &from, &to, &options)
            .wrap_err(format!("Failed to move worktree '{}' to '{}'", from, to))?;

        Ok(Some(worktree))
    }
}
