use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HeadMatchesPredicate {
    pattern: String,
}

impl PredicateReflection for HeadMatchesPredicate {}

impl fmt::Display for HeadMatchesPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "head matches pattern '{}'", self.pattern)
    }
}

impl Predicate<Repository> for HeadMatchesPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match repo.head() {
            Ok(head) => head
                .name()
                .map_or(false, |name| name.contains(&self.pattern)),
            Err(_) => false,
        }
    }
}

pub fn head_matches<P>(pattern: P) -> HeadMatchesPredicate
where
    P: Into<String>,
{
    HeadMatchesPredicate {
        pattern: pattern.into(),
    }
}
