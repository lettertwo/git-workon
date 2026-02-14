//! New command with interactive mode, PR support, and auto-copy.
//!
//! Creates new worktrees with interactive prompts, pull request support, and
//! automatic file copying integration.
//!
//! ## Interactive Prompts
//!
//! When no name is provided:
//! 1. Prompts for branch name using Input widget
//! 2. Prompts for base branch using FuzzySelect (shows local branches + configured default)
//! 3. PR detection still works - user can type `pr#123` at the name prompt
//!
//! Use `--no-interactive` to bypass prompts (required for testing/scripting).
//!
//! ## PR Support Integration
//!
//! Detects PR references in the name and handles them specially:
//! - Parses PR reference (see pr.rs for supported formats)
//! - Auto-detects remote (upstream → origin → first)
//! - Auto-fetches if PR branch not present
//! - Names worktree using `workon.prFormat` config
//! - Sets up tracking to PR head branch
//!
//! Combined with smart routing in main.rs, enables: `git workon #123`
//!
//! ## Automatic File Copying
//!
//! If `workon.autoCopyUntracked=true`:
//! - Copies files from base branch's worktree (or HEAD's worktree if no base)
//! - Uses `workon.copyPattern` patterns (or defaults to `**/*`)
//! - Respects `workon.copyExclude` patterns
//! - Runs after worktree creation, before post-create hooks
//! - Can be overridden with `--(no-)copy-untracked` flags
//!
//! ## Execution Order
//!
//! 1. Create worktree
//! 2. Copy files (if auto-copy enabled)
//! 3. Execute post-create hooks (from hooks.rs)
//!
//! ## gh CLI Integration
//!
//! PR support uses gh CLI for robust metadata handling:
//! - Fetches PR title, author, branch name, and base branch
//! - Supports fork-based PRs by auto-adding fork remotes
//! - Properly sets upstream tracking for PR branches
//! - Enables format placeholders: {number}, {title}, {author}, {branch}

use dialoguer::{FuzzySelect, Input};
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use crate::cli::New;
use crate::hooks::execute_post_create_hooks;
use crate::output;
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
            Some(name) => name.clone(),
            None => {
                if self.no_interactive {
                    bail!("No worktree name provided. Specify a name or remove --no-interactive.");
                }

                // Prompt for branch name
                let name: String = Input::new()
                    .with_prompt("Branch name")
                    .interact_text()
                    .into_diagnostic()
                    .wrap_err("Failed to read branch name")?;

                if name.trim().is_empty() {
                    bail!("Branch name cannot be empty");
                }

                name.trim().to_string()
            }
        };

        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let config = workon::WorkonConfig::new(&repo)?;

        // Check if this is a PR reference
        // Only treat as PR if no conflicting flags are provided
        let pr_info = if !self.orphan && !self.detach && self.base.is_none() {
            workon::parse_pr_reference(&name)?
        } else {
            None
        };

        let (worktree_name, base_branch, branch_type) = if let Some(pr) = pr_info {
            // This is a PR reference - use gh CLI workflow
            let pr_format = config.pr_format(None)?;
            let (worktree_name, remote_ref, base_ref) =
                workon::prepare_pr_worktree(&repo, pr.number, &pr_format)
                    .wrap_err(format!("Failed to prepare PR #{} worktree", pr.number))?;

            // Create worktree
            let worktree =
                add_worktree(&repo, &worktree_name, BranchType::Normal, Some(&remote_ref))?;

            // Fix upstream tracking
            // remote_ref is in format "remote/branch" - extract both parts
            let parts: Vec<&str> = remote_ref.split('/').collect();
            let remote_name = parts.first().copied().unwrap_or("origin");
            let branch_name = parts[1..].join("/"); // Handle branches with slashes
            let branch_ref = format!("refs/heads/{}", branch_name);
            workon::set_upstream_tracking(&worktree, remote_name, &branch_ref)
                .wrap_err("Failed to set upstream tracking for PR branch")?;

            // Copy files if configured
            let copy_override = if self.copy_untracked {
                Some(true)
            } else if self.no_copy_untracked {
                Some(false)
            } else {
                None
            };

            if config.auto_copy_untracked(copy_override)? {
                if let Err(e) = copy_untracked_files(&repo, &worktree, Some(&base_ref), &config) {
                    output::warn(&format!("Failed to copy untracked files: {}", e));
                }
            }

            // Execute post-create hooks
            if !self.no_hooks {
                if let Err(e) = execute_post_create_hooks(&worktree, Some(&base_ref), &config) {
                    output::warn(&format!("Post-create hook failed: {}", e));
                }
            }

            return Ok(Some(worktree));
        } else {
            // Regular worktree creation

            // Determine base branch
            let base_branch = if let Some(base) = &self.base {
                config.default_branch(Some(base))?
            } else if !self.no_interactive && self.name.is_none() {
                // Interactive mode: prompt for base branch
                prompt_for_base_branch(&repo, &config)?
            } else {
                config.default_branch(None)?
            };

            let branch_type = if self.orphan {
                BranchType::Orphan
            } else if self.detach {
                BranchType::Detached
            } else {
                BranchType::Normal
            };

            (name, base_branch, branch_type)
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
                output::warn(&format!("Failed to copy untracked files: {}", e));
                // Continue - worktree is still valid
            }
        }

        // Execute post-create hooks after successful worktree creation
        if !self.no_hooks {
            if let Err(e) = execute_post_create_hooks(&worktree, base_branch.as_deref(), &config) {
                output::warn(&format!("Post-create hook failed: {}", e));
                // Continue - worktree is still valid
            }
        }

        Ok(Some(worktree))
    }
}

/// Prompt user to select a base branch from available branches
fn prompt_for_base_branch(
    repo: &git2::Repository,
    config: &workon::WorkonConfig,
) -> Result<Option<String>> {
    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .into_diagnostic()?;

    let branch_names: Vec<String> = branches
        .filter_map(|b| {
            b.ok()
                .and_then(|(branch, _)| branch.name().ok().flatten().map(|s| s.to_string()))
        })
        .collect();

    if branch_names.is_empty() {
        return config.default_branch(None).map_err(Into::into);
    }

    let default_branch = config
        .default_branch(None)?
        .unwrap_or_else(|| "main".to_string());
    let mut items = vec![format!("<default: {}>", default_branch)];
    items.extend(branch_names.iter().cloned());

    let selection = FuzzySelect::new()
        .with_prompt("Base branch")
        .items(&items)
        .default(0)
        .interact()
        .into_diagnostic()
        .wrap_err("Failed to select base branch")?;

    if selection == 0 {
        Ok(Some(default_branch))
    } else {
        Ok(Some(branch_names[selection - 1].clone()))
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
        output::success(&format!(
            "Copied {} file(s) from base worktree",
            copied.len()
        ));
    }

    Ok(())
}
