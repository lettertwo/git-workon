use std::fmt;

use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct EmptyPredicate {
    empty: bool,
}

pub fn empty() -> EmptyPredicate {
    EmptyPredicate { empty: true }
}

pub fn not_empty() -> EmptyPredicate {
    EmptyPredicate { empty: false }
}

pub struct ExistsPredicate {
    exists: bool,
}

impl PredicateReflection for EmptyPredicate {}

impl fmt::Display for EmptyPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", if self.empty { "empty" } else { "not empty" })
    }
}

impl Predicate<git2::Repository> for EmptyPredicate {
    fn eval(&self, repo: &git2::Repository) -> bool {
        repo.is_empty().unwrap() == self.empty
    }
}
