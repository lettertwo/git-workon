pub use crate::{
    assert::{FixtureAssert, IntoFixturePredicate},
    fixture::{CommitBuilder, Fixture},
    fixture_builder::{FixtureBuilder, MetadataFormat, RemoteSource},
    predicates::{
        BranchPointsToPredicate, HasBranchMetadataPredicate, HasBranchPredicate,
        HasConfigPredicate, HasGraphiteConfigPredicate, HasMetadataParentRevisionPredicate,
        HasRemoteBranchPredicate, HasRemotePredicate, HasRemoteUrlPredicate,
        HasSqliteBranchMetadataPredicate, HasStagedFilePredicate, HasStashPredicate,
        HasUnstagedFilePredicate, HasUntrackedFilePredicate, HasUpstreamPredicate,
        HasWorktreePredicate, HeadCommitMessageContainsPredicate, HeadCommitParentCountPredicate,
        HeadMatchesPredicate, IsBarePredicate, IsEmptyPredicate, IsHeadDetachedPredicate,
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
            branch_points_to, has_branch, has_branch_metadata, has_config, has_graphite_config,
            has_metadata_parent_revision, has_no_stash, has_remote, has_remote_branch,
            has_remote_url, has_sqlite_branch_metadata, has_staged_file, has_stash,
            has_unstaged_file, has_untracked_file, has_upstream, has_worktree,
            head_commit_message_contains, head_commit_parent_count, head_matches, is_bare,
            is_empty, is_head_detached, is_worktree,
        };
    }
    // Re-export predicates for convenience
    pub use predicates::prelude::predicate::*;
}
