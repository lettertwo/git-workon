//! Shared lookups behind the gh-stack predicates.
//!
//! Mirrors [`super::metadata_common`]'s role for Graphite: the JSON-loading and path
//! resolution plumbing lives here once instead of duplicated per predicate file.

use git2::Repository;
use std::path::PathBuf;

/// `<common-dir>/gh-stack` (canonical) or `<common-dir>/worktrees/<name>/gh-stack`
/// (per-worktree), matching `git-workon-lib`'s `stack::gh_stack::canonical_path` layout.
pub(crate) fn gh_stack_path(repo: &Repository, worktree: Option<&str>) -> PathBuf {
    match worktree {
        None => repo.commondir().join("gh-stack"),
        Some(name) => repo
            .commondir()
            .join("worktrees")
            .join(name)
            .join("gh-stack"),
    }
}

/// Read and parse the gh-stack file at `worktree`'s target, if it exists and is valid JSON.
pub(crate) fn gh_stack_doc(repo: &Repository, worktree: Option<&str>) -> Option<serde_json::Value> {
    let path = gh_stack_path(repo, worktree);
    let content = std::fs::read(&path).ok()?;
    serde_json::from_slice(&content).ok()
}

/// Flatten every `branches[]` entry (across every `stacks[]` entry, in file order) from a
/// parsed gh-stack document.
pub(crate) fn flatten_branches(doc: &serde_json::Value) -> Vec<&serde_json::Value> {
    doc.get("stacks")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|stack| stack.get("branches").and_then(|b| b.as_array()))
        .flatten()
        .collect()
}
