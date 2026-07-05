use git2::{Repository, Status, StatusOptions};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasUnstagedFilePredicate {
    path: String,
}

impl PredicateReflection for HasUnstagedFilePredicate {}

impl fmt::Display for HasUnstagedFilePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has unstaged modification to '{}'", self.path)
    }
}

impl Predicate<Repository> for HasUnstagedFilePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let mut opts = StatusOptions::new();
        opts.include_untracked(false);
        let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
            return false;
        };
        statuses.iter().any(|entry| {
            entry.path().ok() == Some(self.path.as_str())
                && entry.status().intersects(Status::WT_MODIFIED)
        })
    }
}

/// Assert that `path` has an unstaged modification (working tree differs from the index,
/// `WT_MODIFIED`) but is tracked.
pub fn has_unstaged_file(path: impl Into<String>) -> HasUnstagedFilePredicate {
    HasUnstagedFilePredicate { path: path.into() }
}
