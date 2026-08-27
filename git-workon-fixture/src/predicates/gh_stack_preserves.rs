use crate::predicates::gh_stack_common::gh_stack_doc;
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct GhStackPreservesPredicate {
    worktree: Option<String>,
    json_pointer: String,
    expected: String,
}

impl PredicateReflection for GhStackPreservesPredicate {}

impl fmt::Display for GhStackPreservesPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gh-stack file (worktree {:?}) has {} == '{}'",
            self.worktree, self.json_pointer, self.expected
        )
    }
}

impl Predicate<Repository> for GhStackPreservesPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Some(doc) = gh_stack_doc(repo, self.worktree.as_deref()) else {
            return false;
        };
        let Some(value) = doc.pointer(&self.json_pointer) else {
            return false;
        };
        // Compare as strings so both `"id-1"` and bare-number pointers (e.g. `pullRequest/
        // number`) work with one predicate, matching how callers spell `expected`.
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
            == self.expected
    }
}

/// Asserts the value at `json_pointer` (RFC 6901, e.g. `/stacks/0/id`) in `worktree`'s
/// gh-stack file equals `expected` — for proving a write round-trip preserved fields workon
/// never reads (`id`, `pullRequest`) instead of dropping them.
pub fn gh_stack_preserves(
    worktree: Option<&str>,
    json_pointer: impl Into<String>,
    expected: impl Into<String>,
) -> GhStackPreservesPredicate {
    GhStackPreservesPredicate {
        worktree: worktree.map(str::to_string),
        json_pointer: json_pointer.into(),
        expected: expected.into(),
    }
}
