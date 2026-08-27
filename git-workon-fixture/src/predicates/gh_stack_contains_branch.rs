use crate::predicates::gh_stack_common::{flatten_branches, gh_stack_doc};
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct GhStackContainsBranchPredicate {
    worktree: Option<String>,
    branch: String,
    position: usize,
}

impl PredicateReflection for GhStackContainsBranchPredicate {}

impl fmt::Display for GhStackContainsBranchPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gh-stack file (worktree {:?}) has '{}' at branches[{}]",
            self.worktree, self.branch, self.position
        )
    }
}

impl Predicate<Repository> for GhStackContainsBranchPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Some(doc) = gh_stack_doc(repo, self.worktree.as_deref()) else {
            return false;
        };
        let branches = flatten_branches(&doc);
        branches
            .get(self.position)
            .and_then(|b| b.get("branch"))
            .and_then(|b| b.as_str())
            == Some(self.branch.as_str())
    }
}

/// Asserts `branch` appears at index `position` among every `branches[]` entry flattened
/// across `stacks[]`, in file order — catching append-order regressions, not just presence.
pub fn gh_stack_contains_branch(
    worktree: Option<&str>,
    branch: impl Into<String>,
    position: usize,
) -> GhStackContainsBranchPredicate {
    GhStackContainsBranchPredicate {
        worktree: worktree.map(str::to_string),
        branch: branch.into(),
        position,
    }
}
