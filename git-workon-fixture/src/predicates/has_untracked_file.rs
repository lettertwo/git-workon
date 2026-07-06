use git2::{Repository, Status, StatusOptions};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasUntrackedFilePredicate {
    path: String,
}

impl PredicateReflection for HasUntrackedFilePredicate {}

impl fmt::Display for HasUntrackedFilePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "has untracked file '{}'", self.path)
    }
}

impl Predicate<Repository> for HasUntrackedFilePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let mut opts = StatusOptions::new();
        // Without recursion, libgit2 reports an untracked file inside a new directory as a
        // single WT_NEW entry for the directory ("sub/"), never for the file itself.
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
            return false;
        };
        statuses.iter().any(|entry| {
            entry.path().ok() == Some(self.path.as_str())
                && entry.status().intersects(Status::WT_NEW)
        })
    }
}

/// Assert that `path` is untracked (`WT_NEW`): never staged, present only in the working tree.
pub fn has_untracked_file(path: impl Into<String>) -> HasUntrackedFilePredicate {
    HasUntrackedFilePredicate { path: path.into() }
}
