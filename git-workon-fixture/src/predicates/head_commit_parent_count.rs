use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HeadCommitParentCountPredicate {
    count: usize,
}

impl PredicateReflection for HeadCommitParentCountPredicate {}

impl fmt::Display for HeadCommitParentCountPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "head commit has {} parent(s)", self.count)
    }
}

impl Predicate<Repository> for HeadCommitParentCountPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match repo.head() {
            Ok(head) => match head.peel_to_commit() {
                Ok(commit) => commit.parent_count() == self.count,
                Err(_) => false,
            },
            Err(_) => false,
        }
    }
}

pub fn head_commit_parent_count(count: usize) -> HeadCommitParentCountPredicate {
    HeadCommitParentCountPredicate { count }
}
