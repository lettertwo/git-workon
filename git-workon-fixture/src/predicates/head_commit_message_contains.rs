use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HeadCommitMessageContainsPredicate {
    pattern: String,
}

impl PredicateReflection for HeadCommitMessageContainsPredicate {}

impl fmt::Display for HeadCommitMessageContainsPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "head commit message contains '{}'", self.pattern)
    }
}

impl Predicate<Repository> for HeadCommitMessageContainsPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match repo.head() {
            Ok(head) => match head.peel_to_commit() {
                Ok(commit) => commit
                    .message()
                    .map_or(false, |msg| msg.contains(&self.pattern)),
                Err(_) => false,
            },
            Err(_) => false,
        }
    }
}

pub fn head_commit_message_contains<P>(pattern: P) -> HeadCommitMessageContainsPredicate
where
    P: Into<String>,
{
    HeadCommitMessageContainsPredicate {
        pattern: pattern.into(),
    }
}
