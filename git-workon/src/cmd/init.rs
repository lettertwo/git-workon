use std::path::PathBuf;

use miette::{Result, WrapErr};
use workon::{add_worktree, get_default_branch_name, init, BranchType, WorktreeDescriptor};

use crate::cli::Init;
use crate::hooks::execute_post_create_hooks;

use super::Run;

impl Run for Init {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));
        let repo = init(path.clone()).wrap_err(format!(
            "Failed to initialize repository at {}",
            path.display()
        ))?;
        let config = workon::WorkonConfig::new(&repo)?;
        let default_branch =
            get_default_branch_name(&repo, None).wrap_err("Failed to determine default branch")?;
        let worktree =
            add_worktree(&repo, &default_branch, BranchType::default(), None).wrap_err(format!(
                "Failed to create worktree for default branch '{}'",
                default_branch
            ))?;

        // Execute post-create hooks after successful worktree creation
        if !self.no_hooks {
            if let Err(e) = execute_post_create_hooks(&worktree, None, &config) {
                eprintln!("Warning: Post-create hook failed: {}", e);
                // Continue - worktree is still valid
            }
        }

        Ok(Some(worktree))
    }
}
