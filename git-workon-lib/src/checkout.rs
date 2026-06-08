//! In-place branch checkout within an existing worktree.
//!
//! Moves HEAD within a worktree's working directory without creating a new worktree
//! directory. This is the primitive backing [`Resolution::Checkout`] from ADR-024:
//! when a target branch `T` belongs to the same stack as a worktree `W`, checking
//! out `T` inside `W` avoids the git lock that prevents rebasing a branch that is
//! live in another worktree.
//!
//! ## git2 vs porcelain
//!
//! Uses `checkout_tree` + `set_head` rather than shelling out to `git checkout`
//! so the operation is consistent with the rest of the library (which shells out
//! only for `gt`/`gh`, which have no git2 API).
//!
//! `CheckoutBuilder::safe()` carries non-conflicting local changes across the HEAD
//! move "for free" — the same semantics as `git checkout --merge`. Known limitation:
//! SAFE mode does not handle submodules or sparse-checkout patterns; those edge cases
//! are documented as out-of-scope in ADR-024.
//!
//! On conflict HEAD is never moved, so an `Err(CheckoutError::Conflict { .. })` is
//! a clean no-op — the caller can either abort or stash-and-retry (PR-3).

use std::sync::{Arc, Mutex};

use git2::{build::CheckoutBuilder, Repository};

use crate::error::{CheckoutError, Result};
use crate::worktree::WorktreeDescriptor;

/// The outcome of a successful [`checkout_branch_in_worktree`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutOutcome {
    /// HEAD was moved cleanly; any non-conflicting local changes were carried along.
    Clean,
    /// One or more files conflict with the new HEAD. HEAD was **not** moved.
    Conflict { paths: Vec<String> },
}

/// Move HEAD inside `wt`'s working directory to `branch`.
///
/// Opens the worktree as its own [`Repository`] (so HEAD/index target `wt`'s
/// working dir), resolves `refs/heads/<branch>`, performs a safe checkout
/// (`CheckoutBuilder::safe()`), then updates HEAD with `set_head`. On conflict
/// HEAD is left unmoved and [`CheckoutOutcome::Conflict`] is returned rather than
/// `Err`, so the caller can prompt before deciding to stash-and-retry (PR-3) or
/// abort.
///
/// Returns `Err` only for genuine git errors (branch not found, I/O, etc.).
pub fn checkout_branch_in_worktree(
    wt: &WorktreeDescriptor,
    branch: &str,
) -> Result<CheckoutOutcome> {
    let wt_repo = Repository::open(wt.path()).map_err(|e| CheckoutError::RepoOpen {
        path: wt.path().to_owned(),
        source: e,
    })?;

    let branch_ref = wt_repo
        .find_branch(branch, git2::BranchType::Local)
        .map_err(|_| CheckoutError::BranchNotFound {
            branch: branch.to_string(),
        })?;

    let target_commit = branch_ref
        .get()
        .peel_to_commit()
        .map_err(|e| CheckoutError::Git(e))?;
    let target_tree = target_commit.tree().map_err(|e| CheckoutError::Git(e))?;

    // Collect conflicting paths via a notify callback. Use Arc<Mutex<_>> so the
    // closure and this function can both own a handle — the closure borrows through
    // the Arc while builder is live, and we read the collected paths afterward.
    let conflicts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let conflicts_cb = Arc::clone(&conflicts);

    let mut builder = CheckoutBuilder::new();
    builder.safe();
    builder.notify_on(git2::CheckoutNotificationType::CONFLICT);
    builder.notify(move |_kind, path, _baseline, _target, _workdir| {
        if let Some(p) = path {
            if let Some(s) = p.to_str() {
                conflicts_cb.lock().unwrap().push(s.to_string());
            }
        }
        true // continue collecting
    });

    match wt_repo.checkout_tree(target_tree.as_object(), Some(&mut builder)) {
        Ok(()) => {}
        Err(e) if e.code() == git2::ErrorCode::Conflict => {
            let paths = Arc::try_unwrap(conflicts)
                .unwrap_or_else(|a| Mutex::new(a.lock().unwrap().clone()))
                .into_inner()
                .unwrap();
            return Ok(CheckoutOutcome::Conflict { paths });
        }
        Err(e) => return Err(CheckoutError::Git(e).into()),
    }

    wt_repo
        .set_head(&format!("refs/heads/{}", branch))
        .map_err(|e| CheckoutError::Git(e))?;

    Ok(CheckoutOutcome::Clean)
}
