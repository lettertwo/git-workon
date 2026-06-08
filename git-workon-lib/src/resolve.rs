//! Stack-aware worktree resolution for `workon <name>`.
//!
//! Implements the four-rule ordered cascade from ADR-024:
//!
//! 1. **T has its own worktree** → navigate (`cd`, no new directory).
//! 2. **Current worktree's branch shares T's stack** → checkout T in place. *(added in PR-2)*
//! 3. **Deepest non-trunk ancestor of T has a worktree** → checkout T there. *(added in PR-2)*
//! 4. **Otherwise** → materialize (auto-attach existing branch or create new branch).
//!
//! Non-stack users ([`StackModel::None`]) only ever see [`Resolution::Navigate`],
//! [`Resolution::Materialize`], or [`Resolution::NotFound`] — rules 2 and 3 never fire,
//! preserving the original worktree-per-branch behavior with no behavior change.

use git2::{BranchType, Repository};

use crate::stack::{current_stack, StackModel};
use crate::worktree::{current_worktree, find_worktree};

/// The resolved action for `workon <T>`.
///
/// The caller maps this to a `Cmd`:
/// - [`Navigate`] and [`NotFound`] → fall through to `Cmd::Find`
/// - [`Materialize`] → `Cmd::New`
/// - [`Checkout`] → `Cmd::Checkout` *(added in PR-2)*
/// - [`DeletedNode`] → structured error *(added in PR-7)*; currently treated as `NotFound`
///
/// [`Navigate`]: Resolution::Navigate
/// [`NotFound`]: Resolution::NotFound
/// [`Materialize`]: Resolution::Materialize
/// [`Checkout`]: Resolution::Checkout
/// [`DeletedNode`]: Resolution::DeletedNode
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// T has its own worktree — navigate to it (`cd`).
    Navigate,
    /// T belongs to a stack whose home worktree is `host` — checkout T in place there.
    ///
    /// The `host` string is the worktree name (not path). Added in PR-2.
    Checkout { host: String },
    /// Materialize a worktree for T (auto-attach existing branch, or create new branch).
    Materialize,
    /// T exists in stack metadata but its branch ref was deleted.
    ///
    /// Per ADR-024: errors with guidance pointing at `gt` rather than attempting to
    /// recreate the branch. Surfaced as `StackError::DeletedBranchNode` in PR-7;
    /// until then, falls through to `Find` (which shows a "no match" error).
    DeletedNode { branch: String },
    /// No worktree, no branch, no metadata match — fall through to `Find`.
    NotFound,
}

/// Resolve the action for `workon <name>` under the given stack model.
///
/// Implements the four-rule cascade from ADR-024.
///
/// # Rule summary
///
/// 1. T has its own worktree → [`Navigate`]. git's lock makes this correctly first.
/// 2. Current worktree's branch shares T's stack → [`Checkout`] in current worktree. *(PR-2)*
/// 3. Deepest non-trunk ancestor of T with a worktree → [`Checkout`] there. *(PR-2)*
/// 4. Branch exists with no worktree → [`Materialize`]; metadata without a ref →
///    [`DeletedNode`]; nothing matches → [`NotFound`].
///
/// Under [`StackModel::None`] rules 2–3 are skipped and only rule 1 / rule 4 fire.
///
/// [`Navigate`]: Resolution::Navigate
/// [`Checkout`]: Resolution::Checkout
/// [`Materialize`]: Resolution::Materialize
/// [`DeletedNode`]: Resolution::DeletedNode
/// [`NotFound`]: Resolution::NotFound
pub fn resolve_action(repo: &Repository, name: &str, model: StackModel) -> Resolution {
    // ── Rule 1 ───────────────────────────────────────────────────────────────
    // T has its own worktree → navigate.
    // `find_worktree` matches by worktree name OR branch name, so a worktree named
    // "wt-feat" checked out on branch "feat" is found by either identifier.
    // Subsumes the trunk case: `main` lives in the `main` worktree, so `workon main`
    // always navigates there. git's lock (it forbids checkout of a branch that is
    // live in another worktree) makes this check correctly first.
    if find_worktree(repo, name).is_ok() {
        return Resolution::Navigate;
    }

    // ── Non-stack degradation ─────────────────────────────────────────────────
    // Under StackModel::None (or --no-stack), every branch is a stack-of-one.
    // Rules 2–3 never fire → unchanged worktree-per-branch behavior.
    if model == StackModel::None {
        return if branch_exists(repo, name) {
            Resolution::Materialize
        } else {
            Resolution::NotFound
        };
    }

    // ── Rules 2 and 3 ────────────────────────────────────────────────────────
    // In-place checkout (move HEAD within an existing worktree, no new directory).
    let t_stack = current_stack(repo, name, model).ok().flatten();

    // Rule 2: current worktree's branch shares T's stack → checkout T in place.
    if let (Ok(cur), Some(ts)) = (current_worktree(repo), &t_stack) {
        if let Ok(Some(b)) = cur.branch() {
            if b == ts.trunk || ts.diffs.contains(&b) {
                if let Some(n) = cur.name() {
                    return Resolution::Checkout {
                        host: n.to_string(),
                    };
                }
            }
        }
    }

    // Rule 3: deepest non-trunk ancestor of T with a worktree → checkout T there.
    // Walk the parent chain nearest-first; first ancestor that has a worktree wins.
    if let Some(ts) = &t_stack {
        let mut node = name.to_string();
        while let Some(parent) = ts.parents.get(&node) {
            if *parent == ts.trunk {
                break; // never host on the trunk worktree
            }
            if let Ok(wt) = find_worktree(repo, parent) {
                if let Some(n) = wt.name() {
                    return Resolution::Checkout {
                        host: n.to_string(),
                    };
                }
            }
            node = parent.clone();
        }
    }

    // ── Rule 4 ───────────────────────────────────────────────────────────────
    // Materialize if a branch ref exists (auto-attach); otherwise distinguish a
    // deleted ◯ node (metadata exists, no ref) from a plain typo.
    if branch_exists(repo, name) {
        return Resolution::Materialize;
    }

    // A branch can appear in stack metadata (it was `gt track`-ed) but have its
    // local ref deleted. ADR-024 says this should error pointing at gt rather than
    // silently trying to recreate the branch.
    if current_stack(repo, name, model).ok().flatten().is_some() {
        return Resolution::DeletedNode {
            branch: name.to_string(),
        };
    }

    Resolution::NotFound
}

/// Returns `true` if `name` matches a local branch or the short name of any
/// remote tracking branch (the part after the first `/`).
///
/// Moved from `git-workon/src/main.rs` into the library so [`resolve_action`]
/// can use it without re-opening the repository.
pub fn branch_exists(repo: &Repository, name: &str) -> bool {
    if repo.find_branch(name, BranchType::Local).is_ok() {
        return true;
    }
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for branch in branches.flatten() {
            if let Ok(Some(full_name)) = branch.0.name() {
                // Remote branch names are "remote/branch" — match the part after the first "/".
                if let Some((_, branch_name)) = full_name.split_once('/') {
                    if branch_name == name {
                        return true;
                    }
                }
            }
        }
    }
    false
}
