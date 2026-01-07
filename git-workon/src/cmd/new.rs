use miette::{Result, WrapErr};

use crate::cli::New;
use crate::hooks::execute_post_create_hooks;
use workon::{add_worktree, copy_files, get_repo, workon_root, BranchType, WorktreeDescriptor};

use super::Run;

// Ability to easily create a worktree with namespcaing.
// Also see: https://lists.mcs.anl.gov/pipermail/petsc-dev/2021-May/027436.html
//
// The anatomy of the command is:
//
//   `git worktree add --track -b <branch> ../<path> <remote>/<remote-branch>`
//
// we want `<branch>` to exactly match `<remote-branch>`
// We want `<path>` to exactly match `<branch>`
//
// Use case: checking out an existing branch
//
//   `git worktree add --track -b bdo/browser-reporter ../bdo/browser-reporter origin/bdo/browser-reporter`
//
// Use case: creating a new branch
// In this case, we aren't tracking a remote (yet?)
//
//   `git worktree add -b lettertwo/some-thing ../lettertwo/some-thing`
//
// Hooks: on creation, we will often want to copy artifacts from the base worktree (e.g., node_modules, build dirs)
// One approach to this is the `copyuntracked` util that can (perhaps interactively?) copy over
// any untracked or git ignored files. It would be nice if this script was also SCM-aware, in that it could
// suggest rebuilds, or re-running install, etc, if the base artifacts are much older than the new worktree HEAD.

impl Run for New {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let name = match &self.name {
            Some(name) => name,
            None => {
                unimplemented!("Interactive new not implemented!");
            }
        };
        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let config = workon::WorkonConfig::new(&repo)?;

        // Check if this is a PR reference
        // Only treat as PR if no conflicting flags are provided
        let pr_info = if !self.orphan && !self.detach && self.base.is_none() {
            workon::parse_pr_reference(name)?
        } else {
            None
        };

        let (worktree_name, base_branch, branch_type) = if let Some(pr) = pr_info {
            // This is a PR reference - handle PR workflow
            let pr_format = config.pr_format(None)?;
            let (worktree_name, remote_ref) =
                workon::prepare_pr_worktree(&repo, pr.number, &pr_format)
                    .wrap_err(format!("Failed to prepare PR #{} worktree", pr.number))?;

            (worktree_name, Some(remote_ref), BranchType::Normal)
        } else {
            // Regular worktree creation
            let base_branch = config.default_branch(self.base.as_deref())?;

            let branch_type = if self.orphan {
                BranchType::Orphan
            } else if self.detach {
                BranchType::Detached
            } else {
                BranchType::Normal
            };

            (name.clone(), base_branch, branch_type)
        };

        let worktree = add_worktree(&repo, &worktree_name, branch_type, base_branch.as_deref())
            .wrap_err(format!("Failed to create worktree '{}'", worktree_name))?;

        // Copy untracked files if enabled
        let copy_override = if self.copy_untracked {
            Some(true)
        } else if self.no_copy_untracked {
            Some(false)
        } else {
            None
        };

        if config.auto_copy_untracked(copy_override)? {
            if let Err(e) = copy_untracked_files(&repo, &worktree, base_branch.as_deref(), &config)
            {
                eprintln!("Warning: Failed to copy untracked files: {}", e);
                // Continue - worktree is still valid
            }
        }

        // Execute post-create hooks after successful worktree creation
        if !self.no_hooks {
            if let Err(e) = execute_post_create_hooks(&worktree, base_branch.as_deref(), &config) {
                eprintln!("Warning: Post-create hook failed: {}", e);
                // Continue - worktree is still valid
            }
        }

        Ok(Some(worktree))
    }
}

/// Copy untracked files from the base worktree to the new worktree
fn copy_untracked_files(
    repo: &git2::Repository,
    worktree: &WorktreeDescriptor,
    base_branch: Option<&str>,
    config: &workon::WorkonConfig,
) -> Result<()> {
    // Get copy patterns from config, or default to copying everything
    let patterns = config.copy_patterns()?;
    let patterns = if patterns.is_empty() {
        vec!["**/*".to_string()]
    } else {
        patterns
    };

    let excludes = config.copy_excludes()?;

    // Determine which branch to copy from
    let source_branch_name = if let Some(base) = base_branch {
        base.to_string()
    } else {
        // No base branch specified, use HEAD's branch
        match repo.head() {
            Ok(head) => {
                if let Some(shorthand) = head.shorthand() {
                    shorthand.to_string()
                } else {
                    // HEAD is detached or not a branch, skip copying
                    return Ok(());
                }
            }
            Err(_) => {
                // Can't determine HEAD, skip copying
                return Ok(());
            }
        }
    };

    // Find the source worktree path
    let root = workon_root(repo)?;
    let source_path = root.join(&source_branch_name);
    if !source_path.exists() {
        // Source worktree doesn't exist, skip copying
        return Ok(());
    }

    // Get destination path
    let dest_path = worktree.path().to_path_buf();

    // Copy files
    let copied = copy_files(&source_path, &dest_path, &patterns, &excludes, false)?;

    // Report what was copied
    if !copied.is_empty() {
        println!("Copied {} file(s) from base worktree", copied.len());
    }

    Ok(())
}
