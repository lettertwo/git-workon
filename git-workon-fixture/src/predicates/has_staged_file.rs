use git2::{Repository, Status, StatusOptions};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasStagedFilePredicate {
    path: String,
}

impl PredicateReflection for HasStagedFilePredicate {}

impl fmt::Display for HasStagedFilePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has staged file '{}'", self.path)
    }
}

impl Predicate<Repository> for HasStagedFilePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let mut opts = StatusOptions::new();
        opts.include_untracked(false);
        let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
            return false;
        };
        statuses.iter().any(|entry| {
            entry.path().ok() == Some(self.path.as_str())
                && entry
                    .status()
                    .intersects(Status::INDEX_NEW | Status::INDEX_MODIFIED)
        })
    }
}

/// Assert that `path` is staged: added to the index (`INDEX_NEW`) or modified in the index
/// (`INDEX_MODIFIED`), but not yet committed.
pub fn has_staged_file(path: impl Into<String>) -> HasStagedFilePredicate {
    HasStagedFilePredicate { path: path.into() }
}
