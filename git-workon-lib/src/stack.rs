//! Stacked diff workflow support.
//!
//! This module provides the infrastructure for detecting and interacting with
//! stacked-diff tools alongside git-workon's worktree management.
//!
//! ## Model
//!
//! Stack awareness is two-dimensional:
//!
//! - [`StackModel`] — which tool manages stacks (v1: Graphite; future: branchless, sapling, spr)
//! - [`Granularity`] — how worktrees map to stacks (v1: [`Granularity::Stack`], one per stack)
//!
//! ## Default-on behavior
//!
//! When `workon.stackModel` resolves to anything other than [`StackModel::None`] (by explicit
//! config or auto-detection), every command that has a meaningful stack-aware variant uses it
//! by default. A `--no-stack` CLI flag (global across subcommands) downgrades any single
//! invocation back to branch-flat behavior.
//!
//! ## Graphite (v1)
//!
//! Stack metadata is read directly from `refs/branch-metadata/*` git refs — blobs containing
//! JSON written by the `gt` CLI. No `gt` process is needed for detection or visualization.
//! `gt track` is invoked only when registering a new branch (in `workon new` when creating a
//! fork off a stack-worktree's branch).

mod graphite;

use git2::Repository;

use crate::error::Result;

/// Which stacked-diff tool is managing stacks in this repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackModel {
    /// No stack tool; today's branch-flat behavior.
    None,
    /// Graphite (`gt`) manages stacks via `refs/branch-metadata/*`.
    Graphite,
}

impl StackModel {
    /// Auto-detect the active stack model from the repository environment.
    ///
    /// Returns [`StackModel::Graphite`] when `gt` is on PATH **and** the repo has been
    /// initialized with `gt init` (`.graphite_repo_config` exists). Otherwise returns
    /// [`StackModel::None`].
    pub fn detect(repo: &Repository) -> Self {
        if graphite::detect_gt() && graphite::is_graphite_repo(repo) {
            Self::Graphite
        } else {
            Self::None
        }
    }
}

/// How worktrees map to stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One worktree hosts an entire stack. The user navigates between branches inside it
    /// using the stack tool's own commands (e.g. `gt up` / `gt down`).
    Stack,
}

/// A stack of branches rooted at a trunk branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// The trunk branch this stack is rooted on (e.g., `"main"`).
    pub trunk: String,
    /// All non-trunk branches in the stack, in BFS order from bottom to top.
    pub branches: Vec<String>,
    /// The branch that is currently HEAD in the worktree.
    pub current: String,
}

/// Return the stack for the worktree whose HEAD is `head_branch`, or `None` if the branch
/// is not part of a tracked stack under `model`.
///
/// The returned [`Stack`] includes all branches reachable from the same stack root, not just
/// the ancestors of `head_branch`, so branching stacks are fully represented.
pub fn current_stack(
    repo: &Repository,
    head_branch: &str,
    model: StackModel,
) -> Result<Option<Stack>> {
    match model {
        StackModel::None => Ok(None),
        StackModel::Graphite => graphite::current_stack(repo, head_branch).map_err(Into::into),
    }
}

/// Returns `true` if `gt` is on PATH and this repository has been Graphite-initialized.
pub fn is_graphite_active(repo: &Repository) -> bool {
    graphite::detect_gt() && graphite::is_graphite_repo(repo)
}
