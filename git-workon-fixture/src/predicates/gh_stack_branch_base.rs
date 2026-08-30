use crate::predicates::gh_stack_common::{flatten_branches, gh_stack_doc};
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct GhStackBranchBasePredicate {
    worktree: Option<String>,
    branch: String,
    expected_base: String,
}

impl PredicateReflection for GhStackBranchBasePredicate {}

impl fmt::Display for GhStackBranchBasePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gh-stack file (worktree {:?}) has '{}' base '{}'",
            self.worktree, self.branch, self.expected_base
        )
    }
}

impl Predicate<Repository> for GhStackBranchBasePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Some(doc) = gh_stack_doc(repo, self.worktree.as_deref()) else {
            return false;
        };
        flatten_branches(&doc)
            .into_iter()
            .find(|b| b.get("branch").and_then(|v| v.as_str()) == Some(self.branch.as_str()))
            .and_then(|b| b.get("base"))
            .and_then(|v| v.as_str())
            == Some(self.expected_base.as_str())
    }
}

/// Asserts `branch`'s `base` field equals `expected_base` in `worktree`'s gh-stack file.
pub fn gh_stack_branch_base(
    worktree: Option<&str>,
    branch: impl Into<String>,
    expected_base: impl Into<String>,
) -> GhStackBranchBasePredicate {
    GhStackBranchBasePredicate {
        worktree: worktree.map(str::to_string),
        branch: branch.into(),
        expected_base: expected_base.into(),
    }
}
