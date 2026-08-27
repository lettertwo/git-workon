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
//! If `workon.autoCopy=true`:
//! - Copies local files from base branch's worktree (or HEAD's worktree if no base)
//! - Includes git-ignored files by default (build artifacts, local config, secrets)
//! - Uses `workon.copyPattern` patterns (or defaults to `**/*`)
//! - Respects `workon.copyExclude` patterns
//! - Runs after worktree creation, before post-create hooks
//! - Can be overridden with `--(no-)copy` flags
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
use git2::BranchType as GitBranchType;
use log::debug;
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use crate::cli::New;
use crate::hooks::execute_post_create_hooks;
use crate::output;
use workon::{
    add_worktree, copy_untracked, current_stack, current_worktree, get_repo, get_worktrees,
    graphite_trunk, link_worktree, resolve_remote_tracking, workon_root, BranchType, CopyOptions,
    RemoteResolution, StackModel, WorkonConfig, WorktreeDescriptor,
};

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
        let config = WorkonConfig::new(&repo)?;

        // Effective stack model for this invocation
        let effective_model = if self.no_stack {
            StackModel::None
        } else {
            config.stack_model(None)?
        };

        // --branch is incompatible with --orphan/--detach (those imply branch creation)
        if self.branch.is_some() && (self.orphan || self.detach) {
            bail!("--branch cannot be combined with --orphan or --detach");
        }

        // Check if this is a PR reference
        // Only treat as PR if no conflicting flags are provided
        let pr_info =
            if !self.orphan && !self.detach && self.base.is_none() && self.branch.is_none() {
                let info = workon::parse_pr_reference(&name)?;
                if info.is_some() {
                    debug!("Detected PR reference in '{}'", name);
                }
                info
            } else {
                debug!("Skipping PR detection (conflicting flags)");
                None
            };

        let (worktree_name, base_branch, branch_type) = if let Some(pr) = pr_info {
            // This is a PR reference - use gh CLI workflow
            let pr_format = config.pr_format(None)?;

            // Phase 1: fetch PR metadata
            let pb = output::create_spinner();
            pb.set_message(format!("Fetching PR #{} metadata...", pr.number));
            let (worktree_name, remote_ref, base_ref) =
                workon::prepare_pr_worktree(&repo, pr.number, &pr_format)
                    .wrap_err(format!("Failed to prepare PR #{} worktree", pr.number))
                    .inspect_err(|_| pb.finish_and_clear())?;
            pb.finish_and_clear();

            // Phase 2: create worktree
            let pb = output::create_spinner();
            pb.set_message("Creating worktree...");
            let worktree = add_worktree(
                &repo,
                &worktree_name,
                None,
                BranchType::Normal,
                Some(&remote_ref),
                self.lock,
            )
            .inspect_err(|_| pb.finish_and_clear())?;
            pb.finish_and_clear();

            // Fix upstream tracking
            // remote_ref is in format "remote/branch" - extract both parts
            let parts: Vec<&str> = remote_ref.split('/').collect();
            let remote_name = parts.first().copied().unwrap_or("origin");
            let branch_name = parts[1..].join("/"); // Handle branches with slashes
            let branch_ref = format!("refs/heads/{}", branch_name);
            workon::set_upstream_tracking(&worktree, remote_name, &branch_ref)
                .wrap_err("Failed to set upstream tracking for PR branch")?;

            // Copy files if configured
            let copy_override = if self.copy {
                Some(true)
            } else if self.no_copy {
                Some(false)
            } else {
                None
            };

            if config.auto_copy(copy_override)? {
                if let Err(e) = copy_untracked_files(
                    &repo,
                    &worktree,
                    Some(&base_ref),
                    &config,
                    self.no_copy_ignored,
                ) {
                    output::warn(&format!("Failed to copy local files: {}", e));
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
                debug!("Using explicit base branch: {}", base);
                config.default_branch(Some(base))?
            } else if !self.no_interactive && self.name.is_none() {
                // Interactive mode: prompt for base branch
                debug!("Prompting for base branch (interactive mode)");
                prompt_for_base_branch(&repo, &config)?
            } else if effective_model != StackModel::None {
                // Stack-aware: if in a stack-worktree, default base to current HEAD branch
                match current_stack_branch(&repo, effective_model)? {
                    Some(branch) => {
                        debug!("Stack-aware: defaulting base to current branch: {}", branch);
                        Some(branch)
                    }
                    None => {
                        debug!("Not in a stack-worktree, using config default branch");
                        config.default_branch(None)?
                    }
                }
            } else {
                debug!("Using default base branch from config");
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

        // When --branch is set, positional name is the worktree directory and
        // the flag value is the branch to look up or create.
        let (effective_branch, worktree_alias) = match &self.branch {
            Some(branch) => {
                if self.base.is_some() && repo.find_branch(branch, GitBranchType::Local).is_ok() {
                    output::warn(&format!(
                        "Branch '{}' already exists; --base ignored",
                        branch
                    ));
                }
                (branch.clone(), Some(worktree_name.clone()))
            }
            None => {
                if self.base.is_some()
                    && repo
                        .find_branch(&worktree_name, GitBranchType::Local)
                        .is_ok()
                {
                    output::warn(&format!(
                        "Branch '{}' already exists; --base ignored",
                        worktree_name
                    ));
                }
                (worktree_name.clone(), None)
            }
        };

        // Check before add_worktree: if the local branch already exists this is an
        // attach (not a create), so gt track must be skipped — re-tracking an existing
        // branch re-parents it and triggers an unwanted restack/rebase.
        let branch_pre_existed = repo
            .find_branch(&effective_branch, GitBranchType::Local)
            .is_ok();

        // For Normal branches that don't exist locally yet, an ambiguous remote
        // (two equally-ranked remotes carrying the name) needs a user decision —
        // add_worktree resolves the unambiguous cases itself. The chosen remote's
        // branch is materialized via the same lib path add_worktree uses, so
        // upstream tracking is always set.
        if branch_type == BranchType::Normal && !branch_pre_existed {
            if let RemoteResolution::Ambiguous(remotes) =
                resolve_remote_tracking(&repo, &effective_branch)
            {
                let remote = if self.no_interactive {
                    remotes.into_iter().next().unwrap()
                } else {
                    let idx = dialoguer::Select::new()
                        .with_prompt(format!(
                            "Branch '{}' exists on multiple remotes; choose one",
                            effective_branch
                        ))
                        .items(&remotes)
                        .default(0)
                        .interact()
                        .into_diagnostic()?;
                    remotes.into_iter().nth(idx).unwrap()
                };
                workon::create_branch_from_remote(&repo, &effective_branch, &remote).wrap_err(
                    format!(
                        "Failed to create branch '{}' from remote '{}'",
                        effective_branch, remote
                    ),
                )?;
            }
        }

        let worktree = add_worktree(
            &repo,
            &effective_branch,
            worktree_alias.as_deref(),
            branch_type,
            base_branch.as_deref(),
            self.lock,
        )
        .wrap_err(format!("Failed to create worktree '{}'", effective_branch))?;

        // Register the new branch with gt when stack-active (non-fatal on failure).
        //
        // Deliberately NOT guarded on `detect_gt()`: `StackModel::detect` resolves `Graphite`
        // from repo metadata alone, so this can run on a machine without `gt`. The resulting
        // "gt track unavailable" warning is the point — the new branch really is untracked, and
        // silence would hide that until the stack looked wrong later. See
        // `new_gt_track_failure_is_non_fatal`.
        if effective_model == StackModel::Graphite
            && !self.no_stack
            && !branch_pre_existed
            && config.gt_auto_track(None)?
        {
            // Prefer the explicit base branch, then the repo's graphite trunk.
            // If neither is known, omit --parent so gt infers from its own config.
            let parent = base_branch
                .as_deref()
                .map(String::from)
                .or_else(|| graphite_trunk(&repo));
            debug!(
                "Running: gt track{} in {}",
                parent
                    .as_deref()
                    .map(|p| format!(" --parent {p}"))
                    .unwrap_or_default(),
                worktree.path().display()
            );
            let mut cmd = std::process::Command::new("gt");
            cmd.arg("track");
            if let Some(p) = &parent {
                cmd.arg("--parent").arg(p);
            }
            match cmd.current_dir(worktree.path()).output() {
                Ok(out) if out.status.success() => {
                    debug!("gt track succeeded");
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    output::warn(&format!("gt track failed: {}", stderr.trim()));
                }
                Err(e) => {
                    output::warn(&format!("gt track unavailable: {}", e));
                }
            }
        }

        // Plant the gh-stack canonical-file symlinks for the new worktree (non-fatal on
        // failure, matching the `gt track` precedent above — `new` has already created the
        // worktree). Unlike `gt track`, there's nothing to run inside another process: this is
        // pure filesystem plumbing that makes gh-stack's own writes visible from every
        // worktree, so it isn't gated on `branch_pre_existed`.
        if effective_model == StackModel::GhStack && !self.no_stack {
            if let Some(name) = worktree.name() {
                if let Err(e) = link_worktree(&repo, name) {
                    output::warn(&format!("gh-stack link failed: {}", e));
                }
            }
        }

        // Copy local files if enabled
        let copy_override = if self.copy {
            Some(true)
        } else if self.no_copy {
            Some(false)
        } else {
            None
        };

        if config.auto_copy(copy_override)? {
            debug!("Auto-copy enabled, copying from base worktree");
            if let Err(e) = copy_untracked_files(
                &repo,
                &worktree,
                base_branch.as_deref(),
                &config,
                self.no_copy_ignored,
            ) {
                output::warn(&format!("Failed to copy local files: {}", e));
                // Continue - worktree is still valid
            }
        } else {
            debug!("Auto-copy disabled");
        }

        // Execute post-create hooks after successful worktree creation
        if !self.no_hooks {
            debug!("Executing post-create hooks");
            if let Err(e) = execute_post_create_hooks(&worktree, base_branch.as_deref(), &config) {
                output::warn(&format!("Post-create hook failed: {}", e));
                // Continue - worktree is still valid
            }
        } else {
            debug!("Hooks skipped (--no-hooks)");
        }

        Ok(Some(worktree))
    }
}

/// Return the HEAD branch of the current stack-worktree under `model`, or None if cwd
/// is not inside any worktree or the worktree's branch is not part of a tracked stack.
fn current_stack_branch(repo: &git2::Repository, model: StackModel) -> Result<Option<String>> {
    let wt = match current_worktree(repo) {
        Ok(wt) => wt,
        Err(_) => return Ok(None),
    };
    let branch = match wt.branch()? {
        Some(b) => b,
        None => return Ok(None),
    };
    match current_stack(repo, &branch, model)? {
        Some(_) => Ok(Some(branch)),
        None => Ok(None),
    }
}

/// Prompt user to select a base branch from available branches
fn prompt_for_base_branch(
    repo: &git2::Repository,
    config: &workon::WorkonConfig,
) -> Result<Option<String>> {
    let branches = repo
        .branches(Some(GitBranchType::Local))
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

/// Copy local files from the base worktree to the new worktree
fn copy_untracked_files(
    repo: &git2::Repository,
    worktree: &WorktreeDescriptor,
    base_branch: Option<&str>,
    config: &workon::WorkonConfig,
    no_copy_ignored: bool,
) -> Result<()> {
    let patterns = config.copy_patterns()?;
    let excludes = config.copy_excludes()?;
    let include_ignored = config.copy_include_ignored(no_copy_ignored.then_some(false))?;

    // Determine source branch name: explicit base, or HEAD's branch
    let source_branch_name = if let Some(base) = base_branch {
        base.to_string()
    } else {
        match repo.head() {
            Ok(head) => match head.shorthand() {
                Ok(s) => s.to_string(),
                Err(_) => return Ok(()), // detached HEAD, skip
            },
            Err(_) => return Ok(()), // can't determine HEAD, skip
        }
    };

    // Find source worktree path via libgit2 worktree list, then fall back to root join.
    // Using get_worktrees avoids breaking on slashed branch names that differ from the
    // worktree's filesystem path, or on PR base refs like "origin/main".
    let source_path = find_worktree_path(repo, &source_branch_name)?;

    let Some(source_path) = source_path else {
        // Source worktree doesn't exist, skip copying
        return Ok(());
    };

    let dest_path = worktree.path().to_path_buf();

    let json_mode = output::is_json_mode();
    let pb = output::create_spinner();
    pb.set_message("Copying files...");

    let mut count = 0usize;
    let pb_copied = pb.clone();
    let copied = copy_untracked(
        &source_path,
        &dest_path,
        CopyOptions {
            patterns: &patterns,
            excludes: &excludes,
            include_ignored,
            on_copied: Box::new(move |rel_path| {
                if !json_mode {
                    count += 1;
                    pb_copied.println(format!(
                        "      {} {}",
                        output::style::green_bold("Copied"),
                        rel_path.display()
                    ));
                    pb_copied.set_message(format!("Copying files... ({} copied)", count));
                }
            }),
            ..Default::default()
        },
    )?;

    pb.finish_and_clear();
    if !copied.is_empty() {
        output::success(&format!(
            "Copied {} file(s) from base worktree",
            copied.len()
        ));
    }

    Ok(())
}

/// Find the filesystem path of a worktree by branch name or worktree name.
///
/// Checks registered worktrees first (handles any naming scheme), then falls back
/// to `root/<name>` which covers the common case of branch-named worktrees.
fn find_worktree_path(
    repo: &git2::Repository,
    branch_name: &str,
) -> Result<Option<std::path::PathBuf>> {
    // Strip remote prefix (e.g., "origin/main" → "main") for matching against branch names
    let local_name = branch_name
        .split_once('/')
        .map(|(_, b)| b)
        .unwrap_or(branch_name);

    if let Ok(worktrees) = get_worktrees(repo) {
        for wt in worktrees {
            // Match by worktree name
            if wt.name() == Some(branch_name) || wt.name() == Some(local_name) {
                return Ok(Some(wt.path().to_path_buf()));
            }
            // Match by branch name
            if let Ok(Some(branch)) = wt.branch() {
                if branch == branch_name || branch == local_name {
                    return Ok(Some(wt.path().to_path_buf()));
                }
            }
        }
    }

    // Fall back to root/<local_name> (the standard git-workon layout)
    let root = workon_root(repo)?;
    let path = root.join(local_name);
    Ok(path.exists().then_some(path))
}
