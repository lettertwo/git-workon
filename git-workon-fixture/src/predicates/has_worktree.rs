use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasWorktreePredicate {
    name: String,
}

impl PredicateReflection for HasWorktreePredicate {}

impl fmt::Display for HasWorktreePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has worktree '{}'", self.name)
    }
}

impl Predicate<Repository> for HasWorktreePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.find_worktree(&self.name).is_ok()
    }
}

pub fn has_worktree<P>(name: P) -> HasWorktreePredicate
where
    P: Into<String>,
{
    HasWorktreePredicate { name: name.into() }
}
