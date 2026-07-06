use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;
use std::path::Path;

pub struct HasIndexModePredicate {
    path: String,
    expected: i32,
}

impl PredicateReflection for HasIndexModePredicate {}

impl fmt::Display for HasIndexModePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "index entry for '{}' has mode {:06o}",
            self.path, self.expected
        )
    }
}

impl Predicate<Repository> for HasIndexModePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Ok(mut index) = repo.index() else {
            return false;
        };
        // See `index_blob_equals`'s doc comment: force a reload so a stale cached index handle
        // doesn't report a mode from before another handle wrote the on-disk index.
        if index.read(true).is_err() {
            return false;
        }
        let Some(entry) = index.get_path(Path::new(&self.path), 0) else {
            return false;
        };
        entry.mode as i32 == self.expected
    }
}

/// Assert that the index entry for `path` has raw octal mode `expected` (e.g. `0o100755` for an
/// executable file) — the regression check for staging an executable file not clobbering its
/// mode back to `0o100644`.
pub fn has_index_mode(path: impl Into<String>, expected: i32) -> HasIndexModePredicate {
    HasIndexModePredicate {
        path: path.into(),
        expected,
    }
}
