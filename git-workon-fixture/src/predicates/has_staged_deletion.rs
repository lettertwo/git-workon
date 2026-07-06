use git2::{Repository, Status, StatusOptions};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasStagedDeletionPredicate {
    path: String,
}

impl PredicateReflection for HasStagedDeletionPredicate {}

impl fmt::Display for HasStagedDeletionPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has staged deletion of '{}'", self.path)
    }
}

impl Predicate<Repository> for HasStagedDeletionPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let mut opts = StatusOptions::new();
        opts.include_untracked(false);
        let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
            return false;
        };
        statuses.iter().any(|entry| {
            entry.path().ok() == Some(self.path.as_str())
                && entry.status().intersects(Status::INDEX_DELETED)
        })
    }
}

/// Assert that `path` is staged for deletion (`INDEX_DELETED`): removed from the index but
/// still present at HEAD.
pub fn has_staged_deletion(path: impl Into<String>) -> HasStagedDeletionPredicate {
    HasStagedDeletionPredicate { path: path.into() }
}
