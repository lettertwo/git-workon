use crate::predicates::gh_stack_common::gh_stack_doc;
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasGhStackPredicate {
    worktree: Option<String>,
}

impl PredicateReflection for HasGhStackPredicate {}

impl fmt::Display for HasGhStackPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.worktree {
            None => write!(f, "has canonical gh-stack file"),
            Some(name) => write!(f, "has gh-stack file in worktree '{name}'"),
        }
    }
}

impl Predicate<Repository> for HasGhStackPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        gh_stack_doc(repo, self.worktree.as_deref())
            .and_then(|doc| doc.get("stacks").cloned())
            .map(|stacks| stacks.is_array())
            .unwrap_or(false)
    }
}

/// Asserts a valid gh-stack JSON file exists at `worktree`'s target (`None` = canonical).
pub fn has_gh_stack(worktree: Option<&str>) -> HasGhStackPredicate {
    HasGhStackPredicate {
        worktree: worktree.map(str::to_string),
    }
}
