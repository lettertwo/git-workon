use git2::{Repository, Status, StatusOptions};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasWorkdirDeletionPredicate {
    path: String,
}

impl PredicateReflection for HasWorkdirDeletionPredicate {}

impl fmt::Display for HasWorkdirDeletionPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has workdir deletion of '{}'", self.path)
    }
}

impl Predicate<Repository> for HasWorkdirDeletionPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let mut opts = StatusOptions::new();
        opts.include_untracked(false);
        let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
            return false;
        };
        statuses.iter().any(|entry| {
            entry.path().ok() == Some(self.path.as_str())
                && entry.status().intersects(Status::WT_DELETED)
        })
    }
}

/// Assert that `path` is deleted from the working tree (`WT_DELETED`): removed on disk but
/// still present in the index.
pub fn has_workdir_deletion(path: impl Into<String>) -> HasWorkdirDeletionPredicate {
    HasWorkdirDeletionPredicate { path: path.into() }
}
