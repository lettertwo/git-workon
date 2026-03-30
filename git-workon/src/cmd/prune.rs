//! Prune command - remove merged/gone worktrees.
//!
//! Removes worktrees whose branches have been merged or deleted, with safety checks
//! to prevent accidental deletion of active work.
//!
//! ## Features
//!
//! - **Targeted pruning**: `git workon prune <name>...` - prune specific worktrees
//! - **Bulk pruning**: `--gone` and `--merged` flags for automatic discovery
//! - **Protected branches**: Respects `workon.pruneProtectedBranches` glob patterns
//! - **Safety checks**: `--allow-dirty` and `--allow-unmerged` to override warnings
//! - **Dry run**: `--dry-run` to preview without deleting
//! - **Branch cleanup**: Local branch refs are deleted after pruning by default; use `--keep-branch` to preserve them
//! - **Force mode**: `--force` overrides all safety checks (protected, default-branch, dirty, unmerged)
//!
//! ## Protected Branch Matching
//!
//! Simple glob patterns:
//! - Exact match: `main` protects only "main"
//! - Wildcard: `*` protects all branches
//! - Prefix: `release/*` protects "release/v1", "release/v2", etc.
//!
//! ## Status Filtering
//!
//! When using `--gone` or `--merged`, the command uses WorktreeDescriptor's status
//! methods to detect which worktrees can be safely pruned. For `--gone` candidates
//! (`RemoteGone`), the dirty check uses `has_tracked_changes()` instead of `is_dirty()`,
//! so untracked files (build artifacts, IDE directories) do not block pruning. The
//! unmerged check is also skipped for `RemoteGone` candidates.

use dialoguer::Confirm;
use git2::BranchType;
use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use workon::{get_default_branch, get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Prune;
use crate::output;

use super::Run;

impl Run for Prune {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let config = workon::WorkonConfig::new(&repo)?;
        let protected_patterns = config.prune_protected_branches()?;
        let worktrees = get_worktrees(&repo)?;

        let mut candidates: Vec<(&WorktreeDescriptor, PruneCandidate)> = Vec::new();

        // First, add explicitly named worktrees
        for name in &self.names {
            // Find worktree by name (exact match or branch name)
            let matching_wt = worktrees.iter().find(|wt| {
                // Match by worktree name or branch name
                if let Some(wt_name) = wt.name() {
                    if wt_name == name {
                        return true;
                    }
                }
                if let Ok(Some(branch)) = wt.branch() {
                    if branch == *name {
                        return true;
                    }
                }
                false
            });

            if let Some(wt) = matching_wt {
                // Get the branch name (if any)
                let branch_name = match wt.branch() {
                    Ok(Some(name)) => name,
                    Ok(None) => "(detached HEAD)".to_string(),
                    Err(_) => "(error reading branch)".to_string(),
                };

                candidates.push((
                    wt,
                    PruneCandidate {
                        worktree_name: wt.name().unwrap_or("").to_string(),
                        worktree_path: wt.path().to_path_buf(),
                        branch_name,
                        reason: PruneReason::Explicit,
                    },
                ));
            } else {
                output::warn(&format!("worktree '{}' not found, skipping", name));
            }
        }

        // Then, add filter-based worktrees (if any filters are specified)
        let pb = output::create_spinner();
        pb.set_message("Checking worktree status...");
        let filter_candidates: Vec<(&WorktreeDescriptor, PruneCandidate)> = worktrees
            .iter()
            .filter_map(|wt| {
                // Skip if already in candidates (from explicit names)
                if candidates.iter().any(|(c, _)| c.name() == wt.name()) {
                    return None;
                }

                // Get the branch name - skip detached worktrees and worktrees with errors
                let branch_name = match wt.branch() {
                    Ok(Some(name)) => name,
                    Ok(None) | Err(_) => return None, // Detached HEAD or error, skip
                };

                // Check if the branch still exists in the main repo
                let branch_exists = repo
                    .find_branch(&branch_name, git2::BranchType::Local)
                    .is_ok();

                if !branch_exists {
                    // Branch is deleted - always prune
                    debug!("'{}': branch deleted, candidate for pruning", branch_name);
                    Some((
                        wt,
                        PruneCandidate {
                            worktree_name: wt.name()?.to_string(),
                            worktree_path: wt.path().to_path_buf(),
                            branch_name,
                            reason: PruneReason::BranchDeleted,
                        },
                    ))
                } else if self.gone {
                    // Branch exists - check if upstream is gone (only if --gone flag is set)
                    match is_upstream_gone(&repo, &branch_name) {
                        Ok(true) => {
                            debug!("'{}': upstream gone, candidate for pruning", branch_name);
                            Some((
                                wt,
                                PruneCandidate {
                                    worktree_name: wt.name()?.to_string(),
                                    worktree_path: wt.path().to_path_buf(),
                                    branch_name,
                                    reason: PruneReason::RemoteGone,
                                },
                            ))
                        }
                        _ => None,
                    }
                } else if let Some(ref merged_target) = self.merged {
                    // Branch exists - check if merged into target (only if --merged flag is set)
                    let target_branch = if merged_target.is_empty() {
                        // Use default branch
                        match get_default_branch(&repo) {
                            Ok(b) => b,
                            Err(_) => return None, // Can't determine default branch
                        }
                    } else {
                        merged_target.clone()
                    };

                    match wt.is_merged_into(&target_branch) {
                        Ok(true) => Some((
                            wt,
                            PruneCandidate {
                                worktree_name: wt.name()?.to_string(),
                                worktree_path: wt.path().to_path_buf(),
                                branch_name,
                                reason: PruneReason::Merged(target_branch),
                            },
                        )),
                        _ => None,
                    }
                } else {
                    debug!("'{}': no prune criteria matched, skipping", branch_name);
                    None
                }
            })
            .collect();
        pb.finish_and_clear();

        // Combine explicit and filter-based candidates
        candidates.extend(filter_candidates);

        // Pre-compute default branch for safety checks
        let default_branch = get_default_branch(&repo).ok();

        // Apply safety checks to filter out unsafe worktrees
        let mut skipped: Vec<(PruneCandidate, String)> = Vec::new();
        let to_prune: Vec<PruneCandidate> = candidates
            .into_iter()
            .filter_map(|(wt, candidate)| {
                // Check if branch is protected
                if !self.force && is_protected(&candidate.branch_name, &protected_patterns) {
                    debug!("'{}': skipped (protected branch)", candidate.branch_name);
                    skipped.push((
                        candidate,
                        "protected by workon.pruneProtectedBranches".to_string(),
                    ));
                    return None;
                }

                // Never prune the default worktree
                if !self.force {
                    if let Some(ref branch) = default_branch {
                        if candidate.branch_name == *branch {
                            skipped.push((candidate, "is the default worktree".to_string()));
                            return None;
                        }
                    }
                }

                // Skip locked worktrees unless --include-locked or --force
                if !self.force && !self.include_locked {
                    if let Ok(true) = wt.is_locked() {
                        skipped.push((
                            candidate,
                            "locked (use --include-locked to override)".to_string(),
                        ));
                        return None;
                    }
                }

                // Check for uncommitted changes.
                // For RemoteGone, only block on tracked-file changes — untracked files
                // (build artifacts, IDE dirs, etc.) are common and not a safety concern.
                if !self.force && !self.allow_dirty {
                    let dirty = if matches!(candidate.reason, PruneReason::RemoteGone) {
                        wt.has_tracked_changes()
                    } else {
                        wt.is_dirty()
                    };
                    match dirty {
                        Ok(true) => {
                            skipped.push((
                                candidate,
                                "has uncommitted changes, use --allow-dirty to override"
                                    .to_string(),
                            ));
                            return None;
                        }
                        Err(_) => {
                            skipped.push((candidate, "could not check status".to_string()));
                            return None;
                        }
                        _ => {}
                    }
                }

                // Check for unmerged commits into the default branch.
                // Skip if branch is already deleted (deletion implies work was handled)
                // or if --merged already verified it's merged into the specified target.
                if !self.force
                    && !self.allow_unmerged
                    && !matches!(
                        candidate.reason,
                        PruneReason::BranchDeleted
                            | PruneReason::RemoteGone
                            | PruneReason::Merged(_)
                    )
                {
                    if let Some(ref branch) = default_branch {
                        if let Ok(false) = wt.is_merged_into(branch) {
                            skipped.push((
                                candidate,
                                "has unmerged commits, use --allow-unmerged to override"
                                    .to_string(),
                            ));
                            return None;
                        }
                    }
                }

                Some(candidate)
            })
            .collect();

        if self.json {
            // JSON mode: skip confirmation, output structured result
            let delete_branch = !self.keep_branch;
            let mut pruned_with_status: Vec<(&PruneCandidate, bool)> = Vec::new();
            let force_locked = self.force || self.include_locked;
            if !self.dry_run {
                for candidate in &to_prune {
                    let branch_deleted =
                        prune_worktree(&repo, candidate, delete_branch, force_locked)?;
                    pruned_with_status.push((candidate, branch_deleted));
                }
            } else {
                for candidate in &to_prune {
                    pruned_with_status.push((candidate, false));
                }
            }

            let result = json!({
                "pruned": pruned_with_status.iter().map(|(c, branch_deleted)| json!({
                    "name": c.worktree_name,
                    "path": c.worktree_path.to_str(),
                    "branch": c.branch_name,
                    "reason": c.reason.to_string(),
                    "branch_deleted": branch_deleted,
                })).collect::<Vec<_>>(),
                "skipped": skipped.iter().map(|(c, reason)| json!({
                    "name": c.worktree_name,
                    "path": c.worktree_path.to_str(),
                    "branch": c.branch_name,
                    "reason": reason,
                })).collect::<Vec<_>>(),
                "dry_run": self.dry_run,
            });

            let output = serde_json::to_string_pretty(&result).into_diagnostic()?;
            println!("{}", output);
            return Ok(None);
        }

        // Display skipped worktrees
        if !skipped.is_empty() {
            output::notice("Skipped worktrees (unsafe to prune):");
            for (candidate, reason) in &skipped {
                output::detail(&format!(
                    "  {} ({})",
                    candidate.worktree_path.display(),
                    reason
                ));
            }
            eprintln!();
        }

        if to_prune.is_empty() {
            output::status("No worktrees to prune");
            return Ok(None);
        }

        // Display what will be pruned
        output::info("Worktrees to prune:");
        for candidate in &to_prune {
            output::detail(&format!(
                "  {} (branch: {}, reason: {})",
                candidate.worktree_path.display(),
                candidate.branch_name,
                candidate.reason
            ));
        }

        if self.dry_run {
            output::notice("\nDry run - no changes made");
            return Ok(None);
        }

        // Confirm with user unless --yes flag is set
        if !self.yes {
            let prompt = if !self.keep_branch {
                format!(
                    "Prune {} worktree(s) and delete their branches?",
                    to_prune.len()
                )
            } else {
                format!("Prune {} worktree(s)?", to_prune.len())
            };
            let confirmed = Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .into_diagnostic()?;

            if !confirmed {
                output::notice("Cancelled");
                return Ok(None);
            }
        }

        // Prune the worktrees
        let delete_branch = !self.keep_branch;
        let force_locked = self.force || self.include_locked;
        for candidate in &to_prune {
            prune_worktree(&repo, candidate, delete_branch, force_locked)?;
        }

        output::success(&format!("Pruned {} worktree(s)", to_prune.len()));
        Ok(None)
    }
}

#[derive(Debug)]
enum PruneReason {
    BranchDeleted,
    RemoteGone,
    Merged(String),
    Explicit,
}

impl std::fmt::Display for PruneReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PruneReason::BranchDeleted => write!(f, "branch deleted"),
            PruneReason::RemoteGone => write!(f, "remote gone"),
            PruneReason::Merged(target) => write!(f, "merged into {}", target),
            PruneReason::Explicit => write!(f, "explicitly requested"),
        }
    }
}

struct PruneCandidate {
    worktree_name: String,
    worktree_path: std::path::PathBuf,
    branch_name: String,
    reason: PruneReason,
}

/// Check if a branch has an upstream that no longer exists (is "gone")
fn is_upstream_gone(repo: &git2::Repository, branch_name: &str) -> Result<bool> {
    // Find the local branch
    let branch = match repo.find_branch(branch_name, BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Branch doesn't exist, not our concern here
    };

    // Check if upstream is configured by looking at the branch config
    // Format: branch.<name>.remote and branch.<name>.merge
    let config = repo.config().into_diagnostic()?;
    let remote_key = format!("branch.{}.remote", branch_name);
    let merge_key = format!("branch.{}.merge", branch_name);

    // If no upstream is configured, not "gone"
    let _remote = match config.get_string(&remote_key) {
        Ok(r) => r,
        Err(_) => return Ok(false), // No remote configured
    };

    let _merge = match config.get_string(&merge_key) {
        Ok(m) => m,
        Err(_) => return Ok(false), // No merge ref configured
    };

    // Try to get the upstream branch - if it fails, upstream is gone
    match branch.upstream() {
        Ok(_) => Ok(false), // Upstream exists
        Err(_) => Ok(true), // Upstream configured but reference is gone
    }
}

fn prune_worktree(
    repo: &git2::Repository,
    candidate: &PruneCandidate,
    delete_branch: bool,
    force_locked: bool,
) -> Result<bool> {
    // Remove the worktree directory first
    if candidate.worktree_path.exists() {
        std::fs::remove_dir_all(&candidate.worktree_path).into_diagnostic()?;
    }

    // Now prune the worktree metadata from git
    let worktree = repo
        .find_worktree(&candidate.worktree_name)
        .into_diagnostic()?;
    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true); // Allow pruning even if worktree is valid
    if force_locked {
        opts.locked(true); // Allow pruning even if worktree is locked
    }
    worktree.prune(Some(&mut opts)).into_diagnostic()?;

    // Optionally delete the local branch ref.
    // Skip if: --keep-branch, branch already deleted, or detached/error sentinel.
    let branch_deleted = if delete_branch
        && !matches!(candidate.reason, PruneReason::BranchDeleted)
        && !candidate.branch_name.starts_with('(')
    {
        match repo.find_branch(&candidate.branch_name, BranchType::Local) {
            Ok(mut branch) => match branch.delete() {
                Ok(()) => true,
                Err(e) => {
                    output::warn(&format!(
                        "could not delete branch '{}': {}",
                        candidate.branch_name, e
                    ));
                    false
                }
            },
            Err(_) => false,
        }
    } else {
        false
    };

    if branch_deleted {
        output::success(&format!(
            "  Pruned {} (branch '{}' deleted)",
            candidate.worktree_path.display(),
            candidate.branch_name
        ));
    } else {
        output::success(&format!("  Pruned {}", candidate.worktree_path.display()));
    }

    Ok(branch_deleted)
}

/// Check if a branch name matches any of the protection patterns
fn is_protected(branch_name: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match(pattern, branch_name) {
            return true;
        }
    }
    false
}

/// Simple glob pattern matching supporting * and ? wildcards
fn glob_match(pattern: &str, text: &str) -> bool {
    // Exact match
    if pattern == text {
        return true;
    }

    // Match all
    if pattern == "*" {
        return true;
    }

    // Prefix match with wildcard (e.g., "release/*")
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return text.starts_with(prefix)
            && text.len() > prefix.len()
            && text[prefix.len()..].starts_with('/');
    }

    // Suffix match with wildcard (e.g., "*/branch")
    if let Some(suffix) = pattern.strip_prefix("*/") {
        return text.ends_with(suffix)
            && text.len() > suffix.len()
            && text[..text.len() - suffix.len()].ends_with('/');
    }

    false
}
