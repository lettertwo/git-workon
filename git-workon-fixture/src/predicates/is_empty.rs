use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct IsEmptyPredicate {}

impl PredicateReflection for IsEmptyPredicate {}

impl fmt::Display for IsEmptyPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "repository is empty",)
    }
}

impl Predicate<Repository> for IsEmptyPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.is_empty().unwrap()
    }
}

pub fn is_empty() -> IsEmptyPredicate {
    IsEmptyPredicate {}
}
