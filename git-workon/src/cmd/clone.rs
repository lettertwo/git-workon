//! Clone command — bare-clone a repository and create an initial worktree.
//!
//! Clones the repository as a bare repo (worktrees-first layout), then creates a
//! worktree for the default branch (detected from the remote's HEAD).
//!
//! ## Path derivation
//!
//! When `path` is omitted, the local directory name is derived from the URL by
//! taking the last path component and stripping a trailing `.git`.
//!
//! ## Post-create hooks
//!
//! After the initial worktree is created, `workon.postCreateHook` commands run
//! inside it. Pass `--no-hooks` to skip.
//!
//! ## Error handling
//!
//! Both clone and worktree-creation phases clear the progress spinner on failure
//! so the error is readable, but the partial clone directory is not removed.

use std::path::PathBuf;

use miette::{Result, WrapErr};
use workon::{
    add_worktree, clone, get_default_branch_name, BranchType, CloneOptions, WorktreeDescriptor,
};

use crate::cli::Clone;
use crate::hooks::execute_post_create_hooks;
use crate::output;

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

        // Phase 1: clone the repository with transfer progress
        let pb = output::create_spinner();
        pb.set_message("Cloning repository...");
        let pb_progress = pb.clone();
        let repo = clone(
            path,
            &self.url,
            CloneOptions {
                on_transfer_progress: Box::new(move |received, total, _bytes| {
                    if total > 0 {
                        pb_progress
                            .set_message(format!("Cloning... {}/{} objects", received, total));
                    }
                }),
            },
        )
        .wrap_err(format!("Failed to clone repository from {}", self.url))
        .inspect_err(|_| pb.finish_and_clear())?;
        pb.finish_and_clear();

        // Phase 2: create the initial worktree
        let pb = output::create_spinner();
        pb.set_message("Creating worktree...");
        let config = workon::WorkonConfig::new(&repo)?;
        let default_branch = get_default_branch_name(&repo, repo.find_remote("origin").ok())
            .wrap_err("Failed to determine default branch")
            .inspect_err(|_| pb.finish_and_clear())?;
        let worktree = add_worktree(
            &repo,
            &default_branch,
            None,
            BranchType::default(),
            None,
            false,
        )
        .wrap_err(format!(
            "Failed to create worktree for default branch '{}'",
            default_branch
        ))
        .inspect_err(|_| pb.finish_and_clear())?;
        pb.finish_and_clear();

        // Execute post-create hooks after successful worktree creation
        if !self.no_hooks {
            if let Err(e) = execute_post_create_hooks(&worktree, None, &config) {
                output::warn(&format!("Post-create hook failed: {}", e));
                // Continue - worktree is still valid
            }
        }

        Ok(Some(worktree))
    }
}
