use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;
use std::path::Path;

pub struct IndexBlobEqualsPredicate {
    path: String,
    expected: Vec<u8>,
}

impl PredicateReflection for IndexBlobEqualsPredicate {}

impl fmt::Display for IndexBlobEqualsPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "index entry for '{}' has blob content {:?} ({} bytes)",
            self.path,
            String::from_utf8_lossy(&self.expected),
            self.expected.len()
        )
    }
}

impl Predicate<Repository> for IndexBlobEqualsPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Ok(mut index) = repo.index() else {
            return false;
        };
        // `repo`'s index handle may have been cached before another handle (e.g. the fixture
        // builder's) wrote the on-disk index; force a reload so we see the current state.
        if index.read(true).is_err() {
            return false;
        }
        // Stage 0: the non-conflicted entry. Conflicted entries (stages 1-3) are out of
        // scope for this predicate.
        let Some(entry) = index.get_path(Path::new(&self.path), 0) else {
            return false;
        };
        let Ok(blob) = repo.find_blob(entry.id) else {
            return false;
        };
        blob.content() == self.expected.as_slice()
    }
}

/// Assert that the index entry for `path` points at a blob with EXACT byte content
/// `expected` — byte-typed (not string) so trap-2/trap-3 tests can pin corrupt or binary
/// blobs precisely, including trailing-newline differences a string comparison would blur.
pub fn index_blob_equals(
    path: impl Into<String>,
    expected: impl Into<Vec<u8>>,
) -> IndexBlobEqualsPredicate {
    IndexBlobEqualsPredicate {
        path: path.into(),
        expected: expected.into(),
    }
}
