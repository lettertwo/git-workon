use std::fmt;

use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct IsBarePredicate {}

impl PredicateReflection for IsBarePredicate {}

impl fmt::Display for IsBarePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "is bare repository")
    }
}
impl Predicate<git2::Repository> for IsBarePredicate {
    fn eval(&self, repo: &git2::Repository) -> bool {
        repo.is_bare()
    }
}

pub fn is_bare() -> IsBarePredicate {
    IsBarePredicate {}
}
