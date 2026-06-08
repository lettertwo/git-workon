//! Labeled autostash for stack-aware checkout.
//!
//! `refs/stash` is per-worktree (stored under `.git/worktrees/<name>/refs/stash`),
//! so entries are scoped to the worktree that created them. Entries are identified
//! by a label embedded in the stash message:
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
//! **Apply, never pop** — on conflict the entry is kept intact so the user can
//! recover manually. No work is ever silently discarded.

use git2::{Repository, Signature, StashFlags};

use crate::error::{CheckoutError, Result};

fn label(branch: &str, worktree: &str) -> String {
    format!("workon-autostash: {} @ {}", branch, worktree)
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
    let want = label(branch, worktree);
    let mut found: Option<usize> = None;
    repo.stash_foreach(|index, message, _oid| {
        if found.is_none() && message.contains(&want) {
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
    /// The stash was applied. The entry is **not** dropped — it remains in the
    /// stash list until the user or a future tool drops it explicitly.
    Applied,
    /// The apply conflicted; the stash entry is kept intact for manual recovery.
    Conflict,
    /// No stash entry matched the label.
    NotFound,
}

/// Apply (but never pop) the stash labeled for `(branch, worktree)`.
///
/// On `Applied` the entry stays in the stash list. On `Conflict` it is also
/// kept so the user can recover manually. The caller is responsible for
/// user-facing messaging.
pub fn apply_labeled_stash(
    repo: &mut Repository,
    branch: &str,
    worktree: &str,
) -> Result<StashRestore> {
    let Some(index) = find_labeled_stash(repo, branch, worktree)? else {
        return Ok(StashRestore::NotFound);
    };
    match repo.stash_apply(index, None) {
        Ok(()) => Ok(StashRestore::Applied),
        Err(e) if e.code() == git2::ErrorCode::Conflict => Ok(StashRestore::Conflict),
        Err(e) => Err(CheckoutError::Git(e).into()),
    }
}

/// List all labeled stash entries belonging to `worktree`.
///
/// Used by the prune command (PR-4) to warn about orphaned stashes before a
/// worktree is removed.
pub fn list_labeled_for_worktree(repo: &mut Repository, worktree: &str) -> Result<Vec<String>> {
    let suffix = format!("@ {}", worktree);
    let mut entries = Vec::new();
    repo.stash_foreach(|_index, message, _oid| {
        if message.contains("workon-autostash:") && message.contains(&suffix) {
            entries.push(message.to_string());
        }
        true
    })
    .map_err(CheckoutError::Git)?;
    Ok(entries)
}
