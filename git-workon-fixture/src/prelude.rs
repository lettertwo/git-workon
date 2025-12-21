pub use crate::{
    assert::{FixtureAssert, IntoFixturePredicate},
    fixture::{CommitBuilder, Fixture},
    fixture_builder::{FixtureBuilder, RemoteSource},
    predicates::{
        BranchPointsToPredicate, HasBranchPredicate, HasConfigPredicate, HasRemoteBranchPredicate,
        HasRemotePredicate, HasRemoteUrlPredicate, HasUpstreamPredicate, HasWorktreePredicate,
        HeadCommitMessageContainsPredicate, HeadCommitParentCountPredicate, HeadMatchesPredicate,
        IsBarePredicate, IsEmptyPredicate, IsWorktreePredicate,
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
            branch_points_to, has_branch, has_config, has_remote, has_remote_branch,
            has_remote_url, has_upstream, has_worktree, head_commit_message_contains,
            head_commit_parent_count, head_matches, is_bare, is_empty, is_worktree,
        };
    }
    // Re-export predicates for convenience
    pub use predicates::prelude::predicate::*;
}
