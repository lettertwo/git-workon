//! Prune-fetch helpers for keeping remote-tracking refs in sync.
//!
//! "Gone" upstream detection in `prune --gone` relies on `Branch::upstream()` returning
//! `Err` for a branch whose tracking ref was deleted on the remote. That only happens
//! after a *prune-fetch* — a fetch that also removes stale `refs/remotes/<remote>/*`
//! entries. Nothing in the codebase performed such a fetch before this module.
//!
//! Two public functions are provided:
//!
//! - [`remotes_tracked_by_worktrees`] — discover which remotes are relevant (deduplicated
//!   list of `branch.<name>.remote` values across all worktrees).
//! - [`prune_fetch`] — run the equivalent of `git fetch --prune <remote>` for a single
//!   remote, deleting stale remote-tracking refs and then fetching.

use git2::{FetchOptions, FetchPrune};

use crate::error::Result;
use crate::get_remote_callbacks::get_remote_callbacks;
use crate::worktree::WorktreeDescriptor;

/// Returns the deduplicated list of remote names tracked by the given worktrees.
///
/// For each worktree with a local branch that has an upstream configured, reads
/// `branch.<name>.remote` from git config. Results are deduplicated and returned in
/// stable (first-seen) order. Worktrees with detached HEAD or no upstream are
/// silently skipped.
pub fn remotes_tracked_by_worktrees(
    repo: &git2::Repository,
    worktrees: &[WorktreeDescriptor],
) -> Result<Vec<String>> {
    let config = repo.config()?;
    let mut remotes: Vec<String> = Vec::new();

    for wt in worktrees {
        let branch_name = match wt.branch()? {
            Some(name) => name,
            None => continue, // detached HEAD
        };

        let remote_key = format!("branch.{}.remote", branch_name);
        if let Ok(remote) = config.get_string(&remote_key) {
            if !remotes.contains(&remote) {
                remotes.push(remote);
            }
        }
    }

    Ok(remotes)
}

/// Prune-fetch from a single remote: remove stale remote-tracking refs, then fetch.
///
/// Equivalent to `git fetch --prune <remote>`. After this call, any
/// `refs/remotes/<remote>/<branch>` ref that no longer exists on the remote is
/// deleted locally, making `Branch::upstream()` correctly return `Err` (i.e. "gone")
/// for branches that were deleted upstream.
///
/// Returns `Err` on auth failure, network error, or unknown remote. Callers that
/// want warn-and-continue behaviour should catch the error and proceed without
/// aborting the rest of the prune.
pub fn prune_fetch(repo: &git2::Repository, remote_name: &str) -> Result<()> {
    let mut remote = repo.find_remote(remote_name)?;

    // Stash the URL before we borrow remote mutably.
    // url() returns Result<&str, _> in this git2 version; ignore errors (URL may be absent).
    let remote_url = remote.url().ok().map(str::to_owned);
    let auth = get_remote_callbacks(repo, remote_url.as_deref())?;

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.prune(FetchPrune::On); // equivalent to --prune: delete stale tracking refs
    fetch_opts.remote_callbacks(auth.callbacks());

    // Empty refspec slice = use the remote's configured refspecs (same as `git fetch origin`).
    remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None)?;

    Ok(())
}
