use miette::{Result, WrapErr};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Complete;

use super::Run;

impl Run for Complete {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let worktrees = get_worktrees(&repo).wrap_err("Failed to list worktrees")?;

        for wt in &worktrees {
            if let Some(name) = wt.name() {
                println!("{}", name);
            }
        }

        Ok(None)
    }
}
