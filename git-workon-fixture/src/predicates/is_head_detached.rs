use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct IsHeadDetachedPredicate {}

impl PredicateReflection for IsHeadDetachedPredicate {}

impl fmt::Display for IsHeadDetachedPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "is head detached")
    }
}
impl Predicate<Repository> for IsHeadDetachedPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.head_detached().unwrap_or(false)
    }
}

pub fn is_head_detached() -> IsHeadDetachedPredicate {
    IsHeadDetachedPredicate {}
}
