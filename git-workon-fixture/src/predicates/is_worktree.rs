use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct IsWorktreePredicate {}

impl PredicateReflection for IsWorktreePredicate {}

impl fmt::Display for IsWorktreePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "is worktree")
    }
}
impl Predicate<Repository> for IsWorktreePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.is_worktree()
    }
}

pub fn is_worktree() -> IsWorktreePredicate {
    IsWorktreePredicate {}
}
