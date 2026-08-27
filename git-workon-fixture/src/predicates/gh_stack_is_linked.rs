use crate::predicates::gh_stack_common::{normalize_lexically, resolve_symlink_lexically};
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct GhStackIsLinkedPredicate {
    worktree: String,
}

impl PredicateReflection for GhStackIsLinkedPredicate {}

impl fmt::Display for GhStackIsLinkedPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "worktree '{}' gh-stack path is a symlink resolving to canonical",
            self.worktree
        )
    }
}

impl Predicate<Repository> for GhStackIsLinkedPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let path = repo
            .commondir()
            .join("worktrees")
            .join(&self.worktree)
            .join("gh-stack");
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if !meta.file_type().is_symlink() {
            return false;
        }
        let Some(resolved) = resolve_symlink_lexically(&path) else {
            return false;
        };
        resolved == normalize_lexically(&repo.commondir().join("gh-stack"))
    }
}

/// Asserts `worktree`'s `gh-stack` admin-dir path is a symlink resolving (lexically, so a
/// dangling target is fine) to `<common-dir>/gh-stack`.
pub fn gh_stack_is_linked(worktree: impl Into<String>) -> GhStackIsLinkedPredicate {
    GhStackIsLinkedPredicate {
        worktree: worktree.into(),
    }
}
