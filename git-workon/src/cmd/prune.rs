use dialoguer::Confirm;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Prune;

use super::Run;

// Cleanup:
//
//   `git worktree remove ../lettertwo/some-thing \
//     && git branch -d lettertwo/some-thing`
//
// Cleanup remote:
//
//   `git worktree remove ../bdo/browser-reporter \
//     && git branch -d bdo/browser-reporter \
//     && git push --delete origin bdo/browser-reporter`

impl Run for Prune {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;

        // Find worktrees that should be pruned (branch no longer exists)
        let to_prune: Vec<PruneCandidate> = worktrees
            .iter()
            .filter_map(|wt| {
                // Get the worktree path
                let path = wt.path();

                // Try to open the worktree as a repository to get its branch
                let wt_repo = match Repository::open(path) {
                    Ok(r) => r,
                    Err(_) => {
                        return None; // Can't open, skip it
                    }
                };

                // Get the branch name from HEAD
                let branch_name = match get_branch_name(&wt_repo) {
                    Some(name) => name,
                    None => {
                        return None; // No branch (detached HEAD?), skip
                    }
                };

                // Check if the branch still exists in the main repo
                let branch_exists = repo
                    .find_branch(&branch_name, git2::BranchType::Local)
                    .is_ok();

                if !branch_exists {
                    Some(PruneCandidate {
                        worktree_name: wt.name()?.to_string(),
                        worktree_path: path.to_path_buf(),
                        branch_name,
                    })
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
            println!("  {} (branch: {})", candidate.worktree_path.display(), candidate.branch_name);
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

struct PruneCandidate {
    worktree_name: String,
    worktree_path: std::path::PathBuf,
    branch_name: String,
}

fn get_branch_name(repo: &Repository) -> Option<String> {
    // Read HEAD file directly to handle cases where the reference doesn't exist
    use std::fs;
    let head_path = repo.path().join("HEAD");
    let head_content = fs::read_to_string(head_path).ok()?;

    // HEAD file contains either a direct SHA or "ref: refs/heads/branch-name"
    if let Some(ref_line) = head_content.strip_prefix("ref: ") {
        let ref_name = ref_line.trim();
        ref_name.strip_prefix("refs/heads/").map(|s| s.to_string())
    } else {
        // Direct SHA - detached HEAD, skip it
        None
    }
}

fn prune_worktree(repo: &Repository, candidate: &PruneCandidate) -> Result<()> {
    // Remove the worktree directory first
    if candidate.worktree_path.exists() {
        std::fs::remove_dir_all(&candidate.worktree_path).into_diagnostic()?;
    }

    // Now prune the worktree metadata from git
    let worktree = repo.find_worktree(&candidate.worktree_name).into_diagnostic()?;
    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true); // Allow pruning even if worktree is valid
    worktree.prune(Some(&mut opts)).into_diagnostic()?;

    println!("  Pruned worktree: {}", candidate.worktree_path.display());
    Ok(())
}
