use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct WorkdirFileEqualsPredicate {
    path: String,
    expected: Vec<u8>,
}

impl PredicateReflection for WorkdirFileEqualsPredicate {}

impl fmt::Display for WorkdirFileEqualsPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "working tree file '{}' has content {:?} ({} bytes)",
            self.path,
            String::from_utf8_lossy(&self.expected),
            self.expected.len()
        )
    }
}

impl Predicate<Repository> for WorkdirFileEqualsPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Some(workdir) = repo.workdir() else {
            return false;
        };
        let Ok(bytes) = std::fs::read(workdir.join(&self.path)) else {
            return false;
        };
        bytes == self.expected
    }
}

/// Assert that the working tree file at `path` has EXACT byte content `expected` —
/// byte-typed (not string) so tests can pin binary content or exact newline handling.
pub fn workdir_file_equals(
    path: impl Into<String>,
    expected: impl Into<Vec<u8>>,
) -> WorkdirFileEqualsPredicate {
    WorkdirFileEqualsPredicate {
        path: path.into(),
        expected: expected.into(),
    }
}
