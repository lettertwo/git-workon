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

/// TRIPWIRE: a creation patch WITHOUT a `new file mode` header line (the shape
/// `PatchText::to_bytes` used to render for a one-sided patch — mode-suffixed `index` line,
/// `/dev/null` old side, but no mode line) is REJECTED by `git apply --cached`: git only sets
/// its is-new flag from the `new file mode` line, so this parses as a MODIFICATION of `new.txt`
/// and fails against the absent index preimage ("... does not exist in index"). This is the
/// exact rejection that motivated the one-sided header fix — the bytes are hand-crafted here
/// because `to_bytes` can no longer produce this broken shape (see
/// `creation_patch_with_proper_headers_is_accepted_by_both_appliers` below for the fixed one).
#[test]
fn naive_hunk_stage_of_untracked_errors() {
    use std::io::Write;

    let raw: &[u8] = b"diff --git a/new.txt b/new.txt\n\
index 0000000..0000000 100644\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1,1 @@\n\
+hello\n";

    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");
    let workdir = repo.workdir().expect("workdir");

    let mut child = std::process::Command::new("git")
        .args(["apply", "--cached"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn git apply");
    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(raw)
        .expect("write patch to stdin");
    let output = child.wait_with_output().expect("wait for git apply");

    assert!(
        !output.status.success(),
        "expected git apply --cached to reject the mode-line-less creation patch, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Go/no-go (fork 1 of `docs/handoffs/2026-07-17-line-ops-one-sided-files.md`): a
/// properly-headed creation patch — `new file mode`, bare `index 0000000..0000000` (no mode
/// suffix), `/dev/null` old side, the canonical `git diff --no-index /dev/null file` shape — is
/// accepted by BOTH appliers. Deliberately bypasses `PatchText`/`Applier`: this pins the
/// MECHANISM (git accepts these headers) as a standing regression test, independent of whether
/// `PatchText::to_bytes` renders this shape (see `creation_patch_renders_new_file_mode_and_bare_index_line`
/// in `src/synthesis.rs` for that). Companion to (not a replacement of)
/// `naive_hunk_stage_of_untracked_errors` above, which pins the OLD (rejected) header shape.
#[test]
fn creation_patch_with_proper_headers_is_accepted_by_both_appliers() {
    let raw: &[u8] = b"diff --git a/new.txt b/new.txt\n\
new file mode 100644\n\
index 0000000..0000000\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1,2 @@\n\
+hello\n\
+world\n";

    // git2::Diff::from_buffer + Repository::apply(ApplyLocation::Index).
    {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("new.txt", "hello\nworld\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let diff = git2::Diff::from_buffer(raw).expect("git2 parses the proper creation header");
        repo.apply(&diff, git2::ApplyLocation::Index, None)
            .expect("git2 applies the proper creation header to the index");

        fixture.assert(predicate::repo::index_blob_equals(
            "new.txt",
            b"hello\nworld\n".to_vec(),
        ));
    }

    // `git apply --cached` directly (CliApplier's mechanism, bypassing PatchText).
    {
        use std::io::Write;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("new.txt", "hello\nworld\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let workdir = repo.workdir().expect("workdir");

        let mut child = std::process::Command::new("git")
            .args(["apply", "--cached"])
            .current_dir(workdir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn git apply");
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(raw)
            .expect("write patch to stdin");
        let output = child.wait_with_output().expect("wait for git apply");
        assert!(
            output.status.success(),
            "git apply --cached rejected the proper creation header: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        fixture.assert(predicate::repo::index_blob_equals(
            "new.txt",
            b"hello\nworld\n".to_vec(),
        ));
    }
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

/// Flipped (was `apply_lines_on_untracked_file_refuses`): the old naive-header bug, not a real
/// git limitation (see the go/no-go test above and `src/synthesis.rs`'s one-sided-patch-header
/// rendering) — `apply_lines` now synthesizes a one-sided creation patch of just the kept lines.
#[test]
fn apply_lines_on_untracked_file_stages_only_the_selected_lines() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\nworld\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    let keep_add = file.hunks[0]
        .lines
        .iter()
        .position(|l| l.kind == LineKind::Addition && l.content == b"hello\n")
        .expect("hello line present");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [].into(),
    };
    let result = apply_lines(repo, &CliApplier, file, 0, &sel, StageVerb::Stage);

    assert!(
        result.is_ok(),
        "expected a line stage of an untracked file to succeed, got {result:?}"
    );
    fixture.assert(predicate::repo::index_blob_equals(
        "new.txt",
        b"hello\n".to_vec(),
    ));
    // Index-only apply: the untracked worktree file is untouched (still both lines).
    fixture.assert(predicate::repo::workdir_file_equals(
        "new.txt",
        b"hello\nworld\n".to_vec(),
    ));
}

/// Flipped (was `apply_lines_on_added_file_refuses`) per fork 3: Added-file line-UNSTAGE is IN
/// SCOPE — the base=New machinery built for Untracked discard is exactly what unstage needs. A
/// partially staged untracked file immediately shows as Added in the staged pane, so this is
/// the same mechanism, just entered from the other side.
#[test]
fn apply_lines_on_added_file_unstages_only_the_selected_lines() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .staged_file("added.txt", "hello\nworld\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.staged.files[0];
    let keep_add = file.hunks[0]
        .lines
        .iter()
        .position(|l| l.kind == LineKind::Addition && l.content == b"world\n")
        .expect("world line present");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [].into(),
    };
    let result = apply_lines(repo, &CliApplier, file, 0, &sel, StageVerb::Unstage);

    assert!(
        result.is_ok(),
        "expected a line unstage of an Added file to succeed, got {result:?}"
    );
    // "world\n" is unstaged (removed from the index); "hello\n" stays staged.
    fixture.assert(predicate::repo::index_blob_equals(
        "added.txt",
        b"hello\n".to_vec(),
    ));
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

/// Regression guard: `is_hunk_patchable`/`apply_hunk`'s routing is deliberately UNCHANGED by the
/// line-ops-on-one-sided-files handoff — a hunk-level `s`/`d` (as opposed to a line-precise
/// selection) on an untracked file still falls back to the whole-file stage, since "the one hunk
/// IS the file" for these statuses.
#[test]
fn apply_hunk_on_untracked_file_still_stages_the_whole_file() {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\nworld\n")
        .build()
        .expect("fixture build");
    let repo = fixture.repo().expect("repo");

    let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, &CliApplier, file, 0, StageVerb::Stage).expect("apply_hunk");

    fixture.assert(predicate::repo::index_blob_equals(
        "new.txt",
        b"hello\nworld\n".to_vec(),
    ));
}
