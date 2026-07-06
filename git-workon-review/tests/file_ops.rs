//! Trap 3 (whole-file ops): tripwires proving the naive hunk-patch shapes for
//! deletion/untracked files misbehave (empty-blob-stage / rejection), then the routed
//! `ops.rs`/`file_ops.rs` behavior that exists to route around them.
//!
//! Fixtures pin `core.autocrlf=false` so index/workdir byte assertions are platform-stable
//! (plan risk #6).

use git_workon_fixture::prelude::*;
use workon_review::acquire::diff_uncommitted;
use workon_review::apply::{Applier, ApplyDestination, ApplyDirection, CliApplier, StageVerb};
use workon_review::error::{ReviewError, SynthesisError};
use workon_review::model::LineKind;
use workon_review::ops::{apply_file, apply_hunk, apply_lines};
use workon_review::synthesis::{LineSelection, PatchHunk, PatchLine, PatchText};

/// Hand-build the patch a naive whole-hunk stage of a DELETION would render: a hunk deleting
/// every line, `--- a/<path>` / `+++ b/<path>` (not `/dev/null` — the file still exists at
/// `path` in the index/HEAD, only its content is fully removed). This is what
/// `whole_hunk_patch` would produce if it didn't refuse `FileStatus::Deleted` — there's no live
/// way to ask the real synthesis path for it, so it's reconstructed by hand, mirroring
/// `tests/line_synthesis.rs`'s `naive_unspliced_patch` pattern.
fn naive_deletion_hunk_patch(path: &str, committed_content: &str) -> PatchText {
    let lines: Vec<PatchLine> = committed_content
        .lines()
        .map(|line| PatchLine {
            kind: LineKind::Deletion,
            content: format!("{line}\n").into_bytes(),
            missing_newline: false,
        })
        .collect();
    let count = lines.len() as u32;
    PatchText {
        old_path: Some(path.to_string()),
        new_path: Some(path.to_string()),
        old_mode: 0o100644,
        new_mode: 0o100644,
        hunks: vec![PatchHunk {
            old_start: 1,
            old_count: count,
            new_start: 0,
            new_count: 0,
            header: format!("@@ -1,{count} +0,0 @@\n").into_bytes(),
            lines,
        }],
    }
}

/// Hand-build the patch a naive whole-hunk stage of an UNTRACKED file would render: an
/// all-additions hunk from `/dev/null` to `b/<path>` — what `whole_hunk_patch` would produce if
/// it didn't refuse `FileStatus::Untracked`.
fn naive_untracked_hunk_patch(path: &str, content: &str) -> PatchText {
    let lines: Vec<PatchLine> = content
        .lines()
        .map(|line| PatchLine {
            kind: LineKind::Addition,
            content: format!("{line}\n").into_bytes(),
            missing_newline: false,
        })
        .collect();
    let count = lines.len() as u32;
    PatchText {
        old_path: None,
        new_path: Some(path.to_string()),
        old_mode: 0o100644,
        new_mode: 0o100644,
        hunks: vec![PatchHunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: count,
            header: format!("@@ -0,0 +1,{count} @@\n").into_bytes(),
            lines,
        }],
    }
}

/// TRIPWIRE: a naive whole-hunk stage of a deletion (deleting every line, but keeping the
/// `a/`/`b/` paths as if the file still existed) is ACCEPTED by `git apply --cached` — it
/// stages an EMPTY BLOB for the path instead of removing the index entry. This is exactly the
/// bug `ops.rs`'s routing to `file_ops::stage_file` exists to prevent (trap 3). Verified
/// directly against `CliApplier` (the oracle), bypassing `ops.rs`/`synthesis.rs` entirely,
/// since `whole_hunk_patch` already refuses `FileStatus::Deleted` and can't produce this patch
/// itself.
#[test]
fn naive_hunk_stage_of_deletion_stages_empty_blob() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .deleted_file("gone.txt", "content\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let patch = naive_deletion_hunk_patch("gone.txt", "content\n");
    let result = CliApplier.apply(
        repo,
        &patch,
        ApplyDestination::Index,
        ApplyDirection::Forward,
    );

    assert!(
        result.is_ok(),
        "expected git apply --cached to accept the naive deletion hunk, got {result:?}"
    );
    fixture.assert(predicate::repo::index_blob_equals("gone.txt", b"".to_vec()));
}

/// TRIPWIRE: a naive whole-hunk stage of an untracked file (from `/dev/null`) is REJECTED by
/// `git apply --cached` — the file isn't in the index yet, so there's no preimage to apply the
/// patch's context against ("... does not exist in index"). Verified directly against
/// `CliApplier`, bypassing `ops.rs`/`synthesis.rs` for the same reason as the deletion
/// tripwire above.
#[test]
fn naive_hunk_stage_of_untracked_errors() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let patch = naive_untracked_hunk_patch("new.txt", "hello\n");
    let result = CliApplier.apply(
        repo,
        &patch,
        ApplyDestination::Index,
        ApplyDirection::Forward,
    );

    assert!(
        result.is_err(),
        "expected git apply --cached to reject the naive untracked hunk, got {result:?}"
    );
}

#[test]
fn apply_lines_on_deleted_file_refuses() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .deleted_file("gone.txt", "content\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let sel = LineSelection::default();
    let result = apply_lines(repo, &CliApplier, file, 0, &sel, StageVerb::Stage);

    assert!(
        matches!(
            result,
            Err(ReviewError::Synthesis(
                SynthesisError::LineSelectionUnsupported { .. }
            ))
        ),
        "expected LineSelectionUnsupported, got {result:?}"
    );
}

#[test]
fn apply_lines_on_untracked_file_refuses() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let sel = LineSelection::default();
    let result = apply_lines(repo, &CliApplier, file, 0, &sel, StageVerb::Stage);

    assert!(
        matches!(
            result,
            Err(ReviewError::Synthesis(
                SynthesisError::LineSelectionUnsupported { .. }
            ))
        ),
        "expected LineSelectionUnsupported, got {result:?}"
    );
}

#[test]
fn apply_lines_on_added_file_refuses() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .staged_file("added.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.staged.files[0];
    let sel = LineSelection::default();
    let result = apply_lines(repo, &CliApplier, file, 0, &sel, StageVerb::Stage);

    assert!(
        matches!(
            result,
            Err(ReviewError::Synthesis(
                SynthesisError::LineSelectionUnsupported { .. }
            ))
        ),
        "expected LineSelectionUnsupported, got {result:?}"
    );
}

#[test]
fn apply_file_stage_on_deleted_file_stages_the_deletion() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .deleted_file("gone.txt", "content\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Stage).expect("apply_file");

    fixture.assert(predicate::repo::has_staged_deletion("gone.txt"));
}

#[test]
fn apply_file_stage_on_untracked_file_stages_its_content() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Stage).expect("apply_file");

    fixture.assert(predicate::repo::has_staged_file("new.txt"));
    fixture.assert(predicate::repo::index_blob_equals(
        "new.txt",
        b"hello\n".to_vec(),
    ));
}

#[test]
fn apply_file_discard_on_untracked_file_removes_it() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Discard).expect("apply_file");

    assert!(!repo.workdir().unwrap().join("new.txt").exists());
}

#[test]
fn apply_file_unstage_on_staged_new_file_becomes_untracked_again() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .staged_file("added.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.staged.files[0];
    apply_file(repo, file, StageVerb::Unstage).expect("apply_file");

    fixture.assert(predicate::repo::has_untracked_file("added.txt"));
    let mut index = repo.index().expect("index");
    index.read(true).expect("index reload");
    assert!(
        index
            .get_path(std::path::Path::new("added.txt"), 0)
            .is_none(),
        "expected no index entry for added.txt after unstage"
    );
}

#[test]
fn apply_file_discard_on_tracked_modified_file_reverts_content() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Discard).expect("apply_file");

    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        b"line1\nline2\nline3\n".to_vec(),
    ));
}

#[test]
fn apply_hunk_on_binary_modified_file_routes_to_file_level_stage() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .bare(true)
        .worktree("main")
        .build()
        .expect("fixture build");

    fixture
        .commit("main")
        .file_bytes("bin.dat", vec![0u8, 1, 2, 3, b'a', 0u8])
        .create("add binary")
        .expect("commit binary");

    let repo = fixture.repo().expect("repo");
    let new_bytes = vec![0u8, 9, 9, 9, b'z', 0u8];
    std::fs::write(repo.workdir().unwrap().join("bin.dat"), &new_bytes)
        .expect("overwrite binary file");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    assert!(file.is_binary, "expected the modified file to be binary");

    apply_hunk(repo, &CliApplier, file, 0, StageVerb::Stage).expect("apply_hunk");

    fixture.assert(predicate::repo::index_blob_equals("bin.dat", new_bytes));
}

/// Regression for the `discard_file` bug: on a PARTIALLY staged file (`HEAD` = "committed",
/// index = "staged", workdir = "workdir" — three distinct states), discarding must revert the
/// workdir to the INDEX's content ("staged"), matching `git restore <path>` — NOT blow past it
/// to `HEAD`'s content ("committed"), which is what the old `checkout_head`-based
/// implementation did, silently wiping staged work off disk.
#[test]
fn apply_file_discard_on_partially_staged_file_reverts_to_index_not_head() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .partially_staged_file("f.txt", "committed\n", "staged\n", "workdir\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Discard).expect("apply_file");

    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        b"staged\n".to_vec(),
    ));
    // The index itself must be untouched by a discard.
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"staged\n".to_vec(),
    ));
}

/// Regression for the `stage_file` bug: an untracked BROKEN symlink is a real working-tree
/// entry (`git add` stages it, storing the link text as the blob, exactly like any other
/// symlink) — but the old `Path::exists()` check follows the link, sees nothing at the
/// (nonexistent) target, and silently takes the `remove_path` branch instead: a no-op that
/// returns `Ok` without staging anything.
#[cfg(unix)]
#[test]
fn apply_file_stage_on_untracked_broken_symlink_stages_it() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_symlink("broken-link", "nonexistent-target")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Stage).expect("apply_file");

    let mut index = repo.index().expect("index");
    index.read(true).expect("index reload");
    assert!(
        index
            .get_path(std::path::Path::new("broken-link"), 0)
            .is_some(),
        "expected an index entry for the staged broken symlink"
    );
}

#[test]
fn apply_hunk_on_modified_text_file_passes_through_to_whole_hunk_stage() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nCHANGED\nline3\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, &CliApplier, file, 0, StageVerb::Stage).expect("apply_hunk");

    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"line1\nCHANGED\nline3\n".to_vec(),
    ));
}
