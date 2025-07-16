pub use crate::{
    assert::{FixtureAssert, IntoFixturePredicate},
    fixture::{Branch, Fixture, Reference, Repository},
    fixture_builder::FixtureBuilder,
    predicates::{
        HasBranchPredicate, HasWorktreePredicate, HeadCommitMessageContainsPredicate,
        HeadCommitParentCountPredicate, HeadMatchesPredicate, IsBarePredicate, IsEmptyPredicate,
        IsWorktreePredicate,
    },
};

pub use assert_fs::prelude::*;

pub use predicates::prelude::{
    Predicate, PredicateBooleanExt, PredicateBoxExt, PredicateFileContentExt, PredicateNameExt,
    PredicateStrExt,
};

// This shadows the predicate module to augment with custom predicates.
pub mod predicate {
    pub mod repo {
        pub use crate::predicates::{
            has_branch, has_worktree, head_commit_message_contains, head_commit_parent_count,
            head_matches, is_bare, is_empty, is_worktree,
        };
    }
    // Re-export predicates for convenience
    pub use predicates::prelude::predicate::*;
}
