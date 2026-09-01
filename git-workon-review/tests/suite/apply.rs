//! Whole-hunk apply round-trips, run against BOTH `Git2Applier` and `CliApplier` via
//! `for_each_applier` (git2-vs-CLI round-trip verdict: CLI is the oracle, git2 is re-verified against it). Each
//! test builds a FRESH fixture per applier — appliers mutate live repository state, so sharing
//! one fixture across both runs would let the second applier's assertions depend on the
//! first's side effects.
//!
//! Fixtures pin `core.autocrlf=false` so index/workdir byte assertions are platform-stable
//! (plan risk #6).

use std::path::Path;

use git_workon_fixture::prelude::*;
use workon_review::acquire::diff_uncommitted;
use workon_review::apply::{is_lock_contention, Applier, CliApplier, Git2Applier, StageVerb};
use workon_review::error::ApplyError;
use workon_review::synthesis::whole_hunk_patch;

/// Run `test` once per applier backend. Each invocation gets its own closure body so callers
/// build a fresh fixture inside `test` rather than sharing one across backends.
fn for_each_applier(mut test: impl FnMut(&dyn Applier)) {
    test(&Git2Applier);
    test(&CliApplier);
}

#[test]
fn stage_whole_hunk_updates_index_and_leaves_workdir_untouched() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        let patch = whole_hunk_patch(file, 0).expect("whole_hunk_patch");

        let (_, dest, dir) = StageVerb::Stage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nCHANGED\nline3\n".to_vec(),
        ));
        // The Index-only apply must not touch the working tree, which still carries the
        // original unstaged modification.
        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            b"line1\nCHANGED\nline3\n".to_vec(),
        ));
    });
}

#[test]
fn unstage_whole_hunk_reverts_index_to_head_content() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        // Stage the modification directly (index := workdir content for this path) so the
        // staged (tree_to_index) model sees the same hunk the unstaged model saw — the
        // unstage patch's preimage must be the INDEX, per plan risk #3.
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("f.txt")).expect("add_path");
        index.write().expect("index write");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.staged.files[0];
        let patch = whole_hunk_patch(file, 0).expect("whole_hunk_patch");

        let (_, dest, dir) = StageVerb::Unstage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nline2\nline3\n".to_vec(),
        ));
    });
}

#[test]
fn discard_whole_hunk_reverts_workdir_to_committed_content() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        let patch = whole_hunk_patch(file, 0).expect("whole_hunk_patch");

        let (_, dest, dir) = StageVerb::Discard.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            b"line1\nline2\nline3\n".to_vec(),
        ));
    });
}

/// First live proof that the marker rendering ([`workon_review::synthesis::PatchHunk`],
/// carrying `\ No newline at end of file`) applies cleanly through a real applier: stage a
/// hunk whose new side lacks the trailing newline, and assert the exact (newline-less) index
/// bytes.
#[test]
fn stage_whole_hunk_with_missing_trailing_newline_on_new_side() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nline2\nline3")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        let patch = whole_hunk_patch(file, 0).expect("whole_hunk_patch");

        let (_, dest, dir) = StageVerb::Stage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nline2\nline3".to_vec(),
        ));
    });
}

/// Plan risk #8: lock classification spans `ErrorCode::Locked` and class `Index`/`Os` with
/// "lock" in the message (git2), or `"index.lock"` in stderr (CLI). `Repository::apply` locks
/// the index only while writing it, so a pre-existing `index.lock` file may or may not trip
/// git2's own preflight — if it doesn't, this manufactures the error git2 would raise on real
/// contention and asserts the classifier handles it, documenting the observed behavior either
/// way rather than silently no-op'ing.
#[test]
fn index_lock_contention_is_classified() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let patch = whole_hunk_patch(file, 0).expect("whole_hunk_patch");

    let lock_path = repo.path().join("index.lock");
    std::fs::write(&lock_path, b"").expect("create index.lock");

    let (_, dest, dir) = StageVerb::Stage.plan();
    let result = Git2Applier.apply(repo, &patch, dest, dir);

    std::fs::remove_file(&lock_path).ok();

    match result {
        Err(err) => {
            assert!(
                is_lock_contention(&err),
                "expected a lock-contention error, got {err:?}"
            );
        }
        Ok(()) => {
            // Repository::apply didn't trip over the pre-existing lock file (it locks only
            // while writing, and this apply may not have needed to touch the index lock at
            // the moment it checked) — manufacture the error libgit2 raises on real
            // contention and prove the classifier itself is correct.
            let manufactured = ApplyError::Git(git2::Error::new(
                git2::ErrorCode::Locked,
                git2::ErrorClass::Index,
                "failed to lock file for writing",
            ));
            assert!(is_lock_contention(&manufactured));
        }
    }
}
