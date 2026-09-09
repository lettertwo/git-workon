//! Repo-level signal helpers for branches with no worktree.
//!
//! `prune`'s existing signals (`has_gone_upstream`, `is_merged_into`, `is_at_or_behind`)
//! all live on [`crate::WorktreeDescriptor`] and open the worktree path to get at the
//! repository. A branch with no worktree has no path to open, so a branch-only prune
//! row needs the same checks against a `&git2::Repository` directly.
//!
//! [`crate::WorktreeDescriptor`]'s methods delegate to these functions.

use crate::error::{Result, WorktreeError};

/// Returns true if `name`'s upstream tracking branch is gone (deleted on remote).
///
/// Mirrors [`crate::WorktreeDescriptor::has_gone_upstream`] for a branch with no
/// worktree. Returns false if:
/// - The branch doesn't exist
/// - The branch has no upstream configured (`branch.<name>.remote` unset)
/// - The upstream branch reference exists
///
/// Returns true if upstream is configured but the upstream reference can't be found.
pub fn branch_has_gone_upstream(repo: &git2::Repository, name: &str) -> Result<bool> {
    let branch = match repo.find_branch(name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Branch doesn't exist
    };

    let config = repo.config()?;
    let remote_key = format!("branch.{}.remote", name);

    match config.get_string(&remote_key) {
        Ok(_) => {
            // Upstream is configured - check if the reference exists
            match branch.upstream() {
                Ok(_) => Ok(false), // Upstream exists
                Err(_) => Ok(true), // Upstream configured but ref is gone
            }
        }
        Err(_) => Ok(false), // No upstream configured
    }
}

/// Returns true if `name` has been merged into `target_branch`.
///
/// Mirrors [`crate::WorktreeDescriptor::is_merged_into`] for a branch with no
/// worktree. A branch is merged if its tip equals the target's tip, or is an
/// ancestor of it. Returns false if:
/// - `name` equals `target_branch` (a branch is never merged into itself)
/// - Either branch doesn't exist
/// - `name` has commits not in `target_branch`'s history
pub fn branch_is_merged_into(
    repo: &git2::Repository,
    name: &str,
    target_branch: &str,
) -> Result<bool> {
    if name == target_branch {
        return Ok(false);
    }

    let branch = match repo.find_branch(name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Branch doesn't exist
    };

    let target = match repo.find_branch(target_branch, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Target branch doesn't exist
    };

    let branch_oid = branch
        .get()
        .target()
        .ok_or(WorktreeError::NoCurrentBranchTarget)?;
    let target_oid = target.get().target().ok_or(WorktreeError::NoBranchTarget)?;

    if branch_oid == target_oid {
        return Ok(true);
    }

    // target is a descendant of (or equal to) branch
    Ok(repo.graph_descendant_of(target_oid, branch_oid)?)
}

/// Returns true if `name`'s tip is at or behind `oid`.
///
/// Mirrors [`crate::WorktreeDescriptor::is_at_or_behind`] for a branch with no
/// worktree. "At or behind" means `oid` equals the branch's tip, or `oid` is a
/// descendant of the tip. Returns false if:
/// - The branch doesn't exist or has no target
/// - `oid` does not parse as a commit hash
/// - `oid` does not resolve to a commit in this repository
pub fn branch_tip_at_or_behind(repo: &git2::Repository, name: &str, oid: &str) -> Result<bool> {
    let branch = match repo.find_branch(name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Branch doesn't exist
    };

    let tip_oid = match branch.get().target() {
        Some(o) => o,
        None => return Ok(false),
    };

    tip_at_or_behind(repo, tip_oid, oid)
}

/// Returns true if `tip` is at or behind `oid`.
///
/// The tip-level check shared by [`branch_tip_at_or_behind`] (a branch's tip) and
/// [`crate::WorktreeDescriptor::is_at_or_behind`] (HEAD, which may be detached and
/// so has no branch name to resolve). "At or behind" means `oid` equals `tip`, or
/// `oid` is a descendant of `tip`. Returns false if:
/// - `oid` does not parse as a commit hash
/// - `oid` does not resolve to a commit in this repository
pub fn tip_at_or_behind(repo: &git2::Repository, tip: git2::Oid, oid: &str) -> Result<bool> {
    if tip.to_string() == oid {
        return Ok(true);
    }

    let target_oid = match git2::Oid::from_str(oid) {
        Ok(o) => o,
        Err(_) => return Ok(false),
    };

    if repo.find_commit(target_oid).is_err() {
        return Ok(false);
    }

    Ok(repo.graph_descendant_of(target_oid, tip)?)
}
