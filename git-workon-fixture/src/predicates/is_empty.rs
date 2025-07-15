use std::fmt;

use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct IsEmptyPredicate {}

impl PredicateReflection for IsEmptyPredicate {}

impl fmt::Display for IsEmptyPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "repository is empty",)
    }
}

impl Predicate<git2::Repository> for IsEmptyPredicate {
    fn eval(&self, repo: &git2::Repository) -> bool {
        repo.is_empty().unwrap()
    }
}

pub fn is_empty() -> IsEmptyPredicate {
    IsEmptyPredicate {}
}
