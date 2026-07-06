//! Line-precise patch synthesis round-trips (trap 1: direction-dependent drop rules; trap 2:
//! the EOFNL del-to-context splice), run against both appliers via `for_each_applier` — see
//! `tests/apply.rs` for the pattern this borrows (fresh fixture per applier backend).

use git_workon_fixture::prelude::*;
use workon_review::acquire::diff_uncommitted;
use workon_review::apply::{
    Applier, ApplyDestination, ApplyDirection, CliApplier, Git2Applier, StageVerb,
};
use workon_review::model::{FileChange, LineKind};
use workon_review::synthesis::{
    partial_hunk_patch, LineSelection, PatchBase, PatchHunk, PatchLine, PatchText,
};

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

/// Fixture for the trap-2 (EOFNL splice) tests: the committed file's last line ("last") has NO
/// trailing newline; the modification deletes that line and adds two new ones, the last of
/// which ("more\n") DOES end in a newline (so the file gains a trailing newline overall). This
/// is the shape that produces a deletion carrying `missing_newline` with kept lines after it —
/// trap 2's precondition.
fn eofnl_fixture() -> FixtureBuilder<'static> {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "a\nb\nlast", "a\nb\nreplaced\nmore\n")
}

/// Hand-built patch replicating what `partial_hunk_patch` would have produced BEFORE the trap-2
/// splice: the dropped deletion of "last" rendered as a plain context line, still carrying its
/// `missing_newline` marker, immediately followed by the kept "+more" addition. This is a
/// test-only stand-in for the naive (pre-fix) code path — there's no live way to ask the current
/// `partial_hunk_patch` for it, since the splice isn't optional.
fn naive_unspliced_patch() -> PatchText {
    PatchText {
        old_path: Some("f.txt".to_string()),
        new_path: Some("f.txt".to_string()),
        hunks: vec![PatchHunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 4,
            header: b"@@ -1,3 +1,4 @@\n".to_vec(),
            lines: vec![
                PatchLine {
                    kind: LineKind::Context,
                    content: b"a\n".to_vec(),
                    missing_newline: false,
                },
                PatchLine {
                    kind: LineKind::Context,
                    content: b"b\n".to_vec(),
                    missing_newline: false,
                },
                PatchLine {
                    kind: LineKind::Context,
                    content: b"last".to_vec(),
                    missing_newline: true,
                },
                PatchLine {
                    kind: LineKind::Addition,
                    content: b"more\n".to_vec(),
                    missing_newline: false,
                },
            ],
        }],
    }
}

/// THE TRIPWIRE, first: documents that `git apply` does NOT reject the naive (unspliced) form —
/// it exits 0 and silently concatenates "more" directly onto "last" with no separating newline,
/// corrupting the blob. Verified against the system `git` binary; pinned here exactly so a
/// future git version that starts rejecting (or otherwise changes) this shape is caught by a
/// test failure instead of a passing suite over a stale assumption.
#[test]
fn naive_unspliced_eofnl_patch_silently_corrupts_the_index() {
    let fixture = eofnl_fixture().build().expect("fixture build");
    let repo = fixture.repo().expect("repo");
    let patch = naive_unspliced_patch();

    let result = CliApplier.apply(
        repo,
        &patch,
        ApplyDestination::Index,
        ApplyDirection::Forward,
    );

    assert!(
        result.is_ok(),
        "expected git apply to silently accept the naive form, got {result:?}"
    );
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"a\nb\nlastmore\n".to_vec(),
    ));
}

#[test]
fn spliced_eofnl_patch_stages_correct_bytes() {
    for_each_applier(|applier| {
        let fixture = eofnl_fixture().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        // Keep only "more\n"; drop the "last" deletion (context, missing_newline) and the
        // "replaced\n" addition (omitted under base=Old) — trap 2's exact precondition.
        let keep_add = line_index(file, 0, LineKind::Addition, "more\n");
        let sel = LineSelection {
            keep_adds: [keep_add].into(),
            keep_dels: [].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::Old).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Stage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"a\nb\nlast\nmore\n".to_vec(),
        ));
    });
}

/// The spliced patch must still be a well-formed unified diff, not just something `git apply`
/// happens to tolerate — `git2::Diff::from_buffer` parses stricter (plan risk #4).
#[test]
fn spliced_eofnl_patch_reparses_via_git2_from_buffer() {
    let fixture = eofnl_fixture().build().expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let keep_add = line_index(file, 0, LineKind::Addition, "more\n");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [].into(),
    };
    let patch = partial_hunk_patch(file, 0, &sel, PatchBase::Old).expect("partial_hunk_patch");

    git2::Diff::from_buffer(&patch.to_bytes()).expect("spliced patch reparses");
}

/// The splice-NOT-needed case: the dropped no-newline deletion is the LAST emitted line (nothing
/// kept comes after it), so it renders as plain context and applies cleanly with no splice.
#[test]
fn dropped_eofnl_deletion_as_last_line_needs_no_splice() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "x\ny\nlast", "X\ny\nreplacedlast")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        // Keep the "x"->"X" change; drop the "last"->"replacedlast" change entirely (its
        // deletion converts to context under base=Old, its addition is omitted) — the dropped
        // deletion ends up as the LAST emitted line.
        let keep_add = line_index(file, 0, LineKind::Addition, "X\n");
        let keep_del = line_index(file, 0, LineKind::Deletion, "x\n");
        let sel = LineSelection {
            keep_adds: [keep_add].into(),
            keep_dels: [keep_del].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::Old).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Stage.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::index_blob_equals(
            "f.txt",
            b"X\ny\nlast".to_vec(),
        ));
    });
}

/// The base=New mirror: a dropped ADDITION carrying `missing_newline` converts to context. Per
/// [`workon_review::synthesis`]'s doc comment on `splice_eofnl_context_lines`, this can never
/// have kept lines after it — `missing_newline` is only ever set on a file's true last line, and
/// synthesis never reorders `hunk.lines` — so it needs no splice. This test is the evidence for
/// that reasoning: the committed file HAS a trailing newline, the modification drops it (the
/// addition "replaced" is the new EOF); discarding while dropping that addition must reproduce
/// the original content without corruption.
#[test]
fn dropped_eofnl_addition_as_context_needs_no_splice_under_base_new() {
    for_each_applier(|applier| {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "a\nb\nlast\n", "a\nb\nreplaced")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let file = &diffs.unstaged.files[0];
        // Keep the deletion of "last\n" (restore it on discard); drop the addition "replaced"
        // (base=New converts it to context — it's the file's new EOF, so nothing follows it).
        let keep_del = line_index(file, 0, LineKind::Deletion, "last\n");
        let sel = LineSelection {
            keep_adds: [].into(),
            keep_dels: [keep_del].into(),
        };
        let patch = partial_hunk_patch(file, 0, &sel, PatchBase::New).expect("partial_hunk_patch");

        let (_, dest, dir) = StageVerb::Discard.plan();
        applier.apply(repo, &patch, dest, dir).expect("apply");

        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            b"a\nb\nlast\nreplaced".to_vec(),
        ));
    });
}
