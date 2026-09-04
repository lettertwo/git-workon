pub use crate::{
    assert::{FixtureAssert, IntoFixturePredicate},
    fixture::{CommitBuilder, Fixture},
    fixture_builder::{FixtureBuilder, MetadataFormat, RemoteSource},
    path_stub::PathStub,
    predicates::{
        BranchPointsToPredicate, GhStackBranchBasePredicate, GhStackContainsBranchPredicate,
        GhStackIsLinkedPredicate, GhStackPreservesPredicate, HasBranchMetadataPredicate,
        HasBranchPredicate, HasConfigPredicate, HasGhStackPredicate, HasGraphiteConfigPredicate,
        HasMetadataParentRevisionPredicate, HasRemoteBranchPredicate, HasRemotePredicate,
        HasRemoteUrlPredicate, HasSqliteBranchMetadataPredicate, HasStagedDeletionPredicate,
        HasStagedFilePredicate, HasStashPredicate, HasUnstagedFilePredicate,
        HasUntrackedFilePredicate, HasUpstreamPredicate, HasWorktreePredicate,
        HeadCommitMessageContainsPredicate, HeadCommitParentCountPredicate, HeadMatchesPredicate,
        IndexBlobEqualsPredicate, IsBarePredicate, IsEmptyPredicate, IsHeadDetachedPredicate,
        IsWorktreePredicate, WorkdirFileEqualsPredicate,
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
            branch_points_to, gh_stack_branch_base, gh_stack_contains_branch, gh_stack_is_linked,
            gh_stack_preserves, has_branch, has_branch_metadata, has_config, has_gh_stack,
            has_graphite_config, has_index_mode, has_metadata_parent_revision, has_no_stash,
            has_remote, has_remote_branch, has_remote_url, has_sqlite_branch_metadata,
            has_staged_deletion, has_staged_file, has_stash, has_unstaged_file, has_untracked_file,
            has_upstream, has_workdir_deletion, has_worktree, head_commit_message_contains,
            head_commit_parent_count, head_matches, index_blob_equals, is_bare, is_empty,
            is_head_detached, is_worktree, workdir_file_equals,
        };
    }
    // Re-export predicates for convenience
    pub use predicates::prelude::predicate::*;
}
