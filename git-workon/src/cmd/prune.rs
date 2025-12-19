use dialoguer::Confirm;
use git2::BranchType;
use miette::{IntoDiagnostic, Result};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Prune;

use super::Run;

impl Run for Prune {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;

        // Find worktrees that should be pruned
        let to_prune: Vec<PruneCandidate> = worktrees
            .iter()
            .filter_map(|wt| {
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
                    Some(PruneCandidate {
                        worktree_name: wt.name()?.to_string(),
                        worktree_path: wt.path().to_path_buf(),
                        branch_name,
                        reason: PruneReason::BranchDeleted,
                    })
                } else if self.gone {
                    // Branch exists - check if upstream is gone (only if --gone flag is set)
                    match is_upstream_gone(&repo, &branch_name) {
                        Ok(true) => Some(PruneCandidate {
                            worktree_name: wt.name()?.to_string(),
                            worktree_path: wt.path().to_path_buf(),
                            branch_name,
                            reason: PruneReason::RemoteGone,
                        }),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect();

        if to_prune.is_empty() {
            println!("No worktrees to prune");
            return Ok(None);
        }

        // Display what will be pruned
        println!("Worktrees to prune:");
        for candidate in &to_prune {
            println!(
                "  {} (branch: {}, reason: {})",
                candidate.worktree_path.display(),
                candidate.branch_name,
                candidate.reason
            );
        }

        if self.dry_run {
            println!("\nDry run - no changes made");
            return Ok(None);
        }

        // Confirm with user unless --yes flag is set
        if !self.yes {
            let confirmed = Confirm::new()
                .with_prompt(format!("Prune {} worktree(s)?", to_prune.len()))
                .default(false)
                .interact()
                .into_diagnostic()?;

            if !confirmed {
                println!("Cancelled");
                return Ok(None);
            }
        }

        // Prune the worktrees
        for candidate in &to_prune {
            prune_worktree(&repo, candidate)?;
        }

        println!("Pruned {} worktree(s)", to_prune.len());
        Ok(None)
    }
}

#[derive(Debug)]
enum PruneReason {
    BranchDeleted,
    RemoteGone,
}

impl std::fmt::Display for PruneReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PruneReason::BranchDeleted => write!(f, "branch deleted"),
            PruneReason::RemoteGone => write!(f, "remote gone"),
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

fn prune_worktree(repo: &git2::Repository, candidate: &PruneCandidate) -> Result<()> {
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
    worktree.prune(Some(&mut opts)).into_diagnostic()?;

    println!("  Pruned worktree: {}", candidate.worktree_path.display());
    Ok(())
}
