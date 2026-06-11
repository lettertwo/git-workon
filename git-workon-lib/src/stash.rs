//! Labeled autostash for stack-aware checkout.
//!
//! `refs/stash` is shared across all worktrees (it lives in the common git dir),
//! so entries from every worktree appear in the same list. Entries are scoped by
//! a label embedded in the stash message and matched on the exact
//! `(branch, worktree)` pair:
//!
//! ```text
//! workon-autostash: <branch> @ <worktree>
//! ```
//!
//! This scheme requires no additional state file — the label is the only
//! coordination mechanism. All operations require a `&mut Repository` opened on
//! the **host worktree's path** (not the bare root), so that HEAD/index target
//! that worktree's working directory.
//!
//! A clean apply drops the entry (the work now lives in the working tree); on
//! conflict the entry is kept intact so the user can recover manually. No work
//! is ever silently discarded.

use git2::{Repository, Signature, StashFlags};

use crate::error::{CheckoutError, Result};

fn label(branch: &str, worktree: &str) -> String {
    format!("workon-autostash: {} @ {}", branch, worktree)
}

/// Parse a stash message into its `(branch, worktree)` label pair.
///
/// Stash messages carry a git-generated `On <branch>: ` prefix before the
/// label, so the label is located by marker rather than by position. The
/// worktree is the part after the *last* ` @ `, tolerating branch names that
/// contain the separator. Returns `None` for non-workon stash entries.
fn parse_label(message: &str) -> Option<(&str, &str)> {
    let (_, rest) = message.split_once("workon-autostash: ")?;
    rest.rsplit_once(" @ ")
}

/// Create a labeled stash in `wt_repo` (a worktree-specific `&mut Repository`).
///
/// `branch` is the branch whose dirty state is being shelved; `worktree` is the
/// host worktree name. Together they form the label used by [`apply_labeled_stash`].
pub fn create_labeled_stash(
    wt_repo: &mut Repository,
    branch: &str,
    worktree: &str,
) -> Result<git2::Oid> {
    let sig = wt_repo
        .signature()
        .or_else(|_| Signature::now("workon", "workon@localhost"))
        .map_err(CheckoutError::Git)?;
    let msg = label(branch, worktree);
    wt_repo
        .stash_save2(&sig, Some(&msg), Some(StashFlags::INCLUDE_UNTRACKED))
        .map_err(CheckoutError::Git)
        .map_err(Into::into)
}

/// Find the stash-list index for the `(branch, worktree)` entry.
///
/// Returns `None` when no matching entry exists.
pub fn find_labeled_stash(
    repo: &mut Repository,
    branch: &str,
    worktree: &str,
) -> Result<Option<usize>> {
    let mut found: Option<usize> = None;
    repo.stash_foreach(|index, message, _oid| {
        if found.is_none() && parse_label(message) == Some((branch, worktree)) {
            found = Some(index);
        }
        true
    })
    .map_err(CheckoutError::Git)?;
    Ok(found)
}

/// Outcome of [`apply_labeled_stash`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashRestore {
    /// The stash was applied cleanly and the entry dropped — the work now
    /// lives in the working tree, so keeping the entry would re-apply it on
    /// every subsequent restore.
    Applied,
    /// The apply conflicted; the stash entry is kept intact for manual
    /// recovery. Conflict markers may be present in the working tree (same
    /// behavior as `git stash apply`).
    Conflict,
    /// No stash entry matched the label.
    NotFound,
}

/// Apply the stash labeled for `(branch, worktree)`, dropping it on a clean apply.
///
/// On `Conflict` the entry is kept so the user can recover manually. The
/// caller is responsible for user-facing messaging.
pub fn apply_labeled_stash(
    repo: &mut Repository,
    branch: &str,
    worktree: &str,
) -> Result<StashRestore> {
    let Some(index) = find_labeled_stash(repo, branch, worktree)? else {
        return Ok(StashRestore::NotFound);
    };
    match repo.stash_apply(index, None) {
        Ok(()) => {
            // A merge conflict is NOT an error: stash_apply writes conflict
            // markers, leaves a conflicted index, and returns success. Check
            // the index before dropping, or the only recovery copy is lost.
            if repo.index().map_err(CheckoutError::Git)?.has_conflicts() {
                return Ok(StashRestore::Conflict);
            }
            repo.stash_drop(index).map_err(CheckoutError::Git)?;
            Ok(StashRestore::Applied)
        }
        // GIT_ECONFLICT: dirty local files block the apply; GIT_EMERGECONFLICT:
        // the merge itself cannot proceed. Both keep the entry intact.
        Err(e)
            if matches!(
                e.code(),
                git2::ErrorCode::Conflict | git2::ErrorCode::MergeConflict
            ) =>
        {
            Ok(StashRestore::Conflict)
        }
        Err(e) => Err(CheckoutError::Git(e).into()),
    }
}

/// List all labeled stash entries belonging to `worktree`.
///
/// Used by the prune command (PR-4) to warn about orphaned stashes before a
/// worktree is removed.
pub fn list_labeled_for_worktree(repo: &mut Repository, worktree: &str) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    repo.stash_foreach(|_index, message, _oid| {
        if parse_label(message).is_some_and(|(_, wt)| wt == worktree) {
            entries.push(message.to_string());
        }
        true
    })
    .map_err(CheckoutError::Git)?;
    Ok(entries)
}
