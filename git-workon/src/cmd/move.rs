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
