use std::path::PathBuf;

use miette::{Result, WrapErr};
use workon::{add_worktree, clone, get_default_branch_name, BranchType, WorktreeDescriptor};

use crate::cli::Clone;
use crate::hooks::execute_post_create_hooks;

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let path = self.path.clone().unwrap_or_else(|| {
            PathBuf::from(
                self.url
                    .trim_end_matches('/')
                    .split('/')
                    .next_back()
                    .unwrap_or(".")
                    .trim_end_matches(".git"),
            )
        });

        let repo = clone(path, &self.url)
            .wrap_err(format!("Failed to clone repository from {}", self.url))?;
        let config = workon::WorkonConfig::new(&repo)?;
        let default_branch = get_default_branch_name(&repo, repo.find_remote("origin").ok())
            .wrap_err("Failed to determine default branch")?;
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
