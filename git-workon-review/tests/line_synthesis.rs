//! Line-precise patch synthesis round-trips (trap 1: direction-dependent drop rules), run
//! against both appliers via `for_each_applier` — see `tests/apply.rs` for the pattern this
//! borrows (fresh fixture per applier backend).
//!
//! Trap-2 (the EOFNL splice) round-trips land in a follow-up commit.

use git_workon_fixture::prelude::*;
use workon_review::acquire::diff_uncommitted;
use workon_review::apply::{
    Applier, ApplyDestination, ApplyDirection, CliApplier, Git2Applier, StageVerb,
};
use workon_review::model::{FileChange, LineKind};
use workon_review::synthesis::{partial_hunk_patch, LineSelection, PatchBase};

/// Run `test` once per applier backend. Each invocation gets its own closure body so callers
/// build a fresh fixture inside `test` rather than sharing one across backends (appliers mutate
/// live repository state).
fn for_each_applier(mut test: impl FnMut(&dyn Applier)) {
    test(&Git2Applier);
    test(&CliApplier);
}

/// Find the `hunk.lines` index of the first line of `kind` whose content matches `content`
/// exactly — lets tests key a [`LineSelection`] off readable content instead of hard-coded
/// positions that would silently drift if git2's line ordering ever changed.
fn line_index(file: &FileChange, hunk_idx: usize, kind: LineKind, content: &str) -> usize {
    file.hunks[hunk_idx]
        .lines
        .iter()
        .position(|l| l.kind == kind && l.content == content.as_bytes())
        .unwrap_or_else(|| panic!("no {kind:?} line with content {content:?} in hunk {hunk_idx}"))
}

/// Two separate changes ("old2"->"new2", "old4"->"new4") in one hunk, separated by a context
/// line — the shape that exercises the direction rules: keeping one change and dropping the
/// other must not affect the untouched change identically under both directions.
fn two_change_fixture() -> FixtureBuilder<'static> {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file(
            "f.txt",
            "line1\nold2\nline3\nold4\nline5\n",
            "line1\nnew2\nline3\nnew4\nline5\n",
        )
}

#[test]
fn stage_partial_updates_only_the_kept_change() {
    for_each_applier(|applier| {
        let fixture = two_change_fixture().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        let keep_add = line_index(file, 0, LineKind::Addition, "new2\n");
        let keep_del = line_index(file, 0, LineKind::Deletion, "old2\n");
        let sel = LineSelection {
            keep_adds: [keep_add].into(),
            keep_dels: [keep_del].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::Old).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Stage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        // Only the first change landed in the index; the second change is still absent there.
        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nnew2\nline3\nold4\nline5\n".to_vec(),
        ));
        // The Index-only apply must not touch the working tree, which still carries the full
        // unstaged modification (both changes).
        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            b"line1\nnew2\nline3\nnew4\nline5\n".to_vec(),
        ));
    });
}

#[test]
fn unstage_partial_removes_only_the_kept_change_from_the_index() {
    for_each_applier(|applier| {
        let fixture = two_change_fixture().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");

        // Stage the FULL modification first (index := workdir for this path), so the staged
        // (tree_to_index) model — the correct preimage for an Unstage patch (plan risk #3) —
        // sees both changes.
        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new("f.txt"))
            .expect("add_path");
        index.write().expect("index write");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.staged.files[0];
        let keep_add = line_index(file, 0, LineKind::Addition, "new2\n");
        let keep_del = line_index(file, 0, LineKind::Deletion, "old2\n");
        let sel = LineSelection {
            keep_adds: [keep_add].into(),
            keep_dels: [keep_del].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::New).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Unstage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        // "Kept" means "operated on": unstaging keep={old2/new2} removes just that change from
        // the index, reverting it to committed content, while the second (dropped/untouched)
        // change stays staged.
        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nold2\nline3\nnew4\nline5\n".to_vec(),
        ));
    });
}

#[test]
fn discard_partial_reverts_only_the_kept_change_in_the_workdir() {
    for_each_applier(|applier| {
        let fixture = two_change_fixture().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        let keep_add = line_index(file, 0, LineKind::Addition, "new2\n");
        let keep_del = line_index(file, 0, LineKind::Deletion, "old2\n");
        let sel = LineSelection {
            keep_adds: [keep_add].into(),
            keep_dels: [keep_del].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::New).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Discard.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            b"line1\nold2\nline3\nnew4\nline5\n".to_vec(),
        ));
        // Nothing was ever staged in this scenario — the index still matches HEAD verbatim.
        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"line1\nold2\nline3\nold4\nline5\n".to_vec(),
        ));
    });
}

/// Trap-1's guard: a `base=Old` partial patch encodes a DIFFERENT, incompatible set of drop
/// rules than a `base=New` one (dropped-add-omitted/dropped-del-context vs. the mirror). Forcing
/// it through a reverse apply must fail outright rather than silently produce a wrong result —
/// this is what proves the direction rules are load-bearing, not cosmetic.
#[test]
fn base_old_partial_patch_fails_under_reverse_apply() {
    let fixture = two_change_fixture().build().expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let keep_add = line_index(file, 0, LineKind::Addition, "new2\n");
    let keep_del = line_index(file, 0, LineKind::Deletion, "old2\n");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [keep_del].into(),
    };
    // Deliberately wrong: synthesize with base=Old (Stage's rules), then force a reverse apply
    // (Unstage's direction) instead of a forward one.
    let patch = partial_hunk_patch(file, 0, &sel, PatchBase::Old).expect("partial_hunk_patch");

    let result = CliApplier.apply(
        repo,
        &patch,
        ApplyDestination::Index,
        ApplyDirection::Reverse,
    );

    assert!(
        matches!(
            result,
            Err(workon_review::error::ApplyError::CliApplyFailed { .. })
        ),
        "expected reverse-applying a base=Old partial patch to fail, got {result:?}"
    );
}
