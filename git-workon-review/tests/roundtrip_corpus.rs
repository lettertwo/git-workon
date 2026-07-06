//! The round-trip verdict corpus (trap 6): every write-path scenario from M2's trap corpus,
//! driven through the `ops.rs` entry points and run against BOTH backends —
//! [`workon_review::apply::CliApplier`] (the oracle) and [`workon_review::apply::Git2Applier`]
//! (the backend under verification).
//!
//! This corpus is the PERMANENT dual-backend guard for the write path: a future libgit2 upgrade
//! flips the verdict recorded in `docs/rfc/workon-review.md` WITH EVIDENCE by making
//! `corpus_against_git2` fail here, not by someone remembering to re-audit the appliers by hand.
//!
//! The corpus asserts END-STATE equivalence only — index bytes, workdir bytes, error taxonomy —
//! never patch-byte equivalence. `Git2Applier`'s `Reverse` direction is `PatchText::invert()`
//! followed by a forward apply (libgit2's `Repository::apply` has no reverse flag), so the
//! bytes it hands to libgit2 for an unstage/discard legitimately differ from what `CliApplier`
//! sends `git apply --reverse` on stdin, even when both backends land the repository in the
//! identical state. Comparing intermediate patch bytes would produce false divergences; the two
//! tests below never do.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use git2::Repository;
use git_workon_fixture::prelude::*;
use workon_review::acquire::{diff_committed, diff_uncommitted};
use workon_review::apply::{Applier, CliApplier, Git2Applier, StageVerb};
use workon_review::error::{ReviewError, SynthesisError};
use workon_review::model::{FileChange, FileStatus, LineKind};
use workon_review::ops::{apply_file, apply_hunk, apply_lines};
use workon_review::synthesis::LineSelection;

/// Find the `hunk.lines` index of the first line of `kind` whose content matches `content`
/// exactly — lets scenarios key a [`LineSelection`] off readable content instead of hard-coded
/// positions (borrowed from `tests/line_synthesis.rs`'s helper of the same name).
fn line_index(file: &FileChange, hunk_idx: usize, kind: LineKind, content: &str) -> usize {
    file.hunks[hunk_idx]
        .lines
        .iter()
        .position(|l| l.kind == kind && l.content == content.as_bytes())
        .unwrap_or_else(|| panic!("no {kind:?} line with content {content:?} in hunk {hunk_idx}"))
}

/// Stage `path`'s current working-tree content directly (bypassing `ops.rs`) as scenario setup
/// — e.g. so an Unstage/Discard scenario's preimage is already staged before the op under test
/// runs.
///
/// `index.read(true)` forces a reload from disk before mutating: `Fixture`'s `Repository` handle
/// can carry an in-memory index cached from before the fixture builder's baseline commit, and
/// `write()` after `add_path` would otherwise silently drop every OTHER path's entries back out
/// of the on-disk index — harmless with one file in a fixture, corrupting with more than one
/// (verified empirically in `staging_storm_ops`'s three-file fixture: the naive form staged all
/// three files as untracked-since-deleted).
fn pre_stage(repo: &Repository, path: &str) -> Result<(), ReviewError> {
    let mut index = repo.index()?;
    index.read(true)?;
    index.add_path(Path::new(path))?;
    index.write()?;
    Ok(())
}

type BuildFn = fn() -> Fixture;
type OpsFn = fn(&Repository, &dyn Applier) -> Result<(), ReviewError>;
type VerifyFn = fn(&Fixture);

/// One row of the corpus: builds a fixture, drives it through `ops.rs`, then asserts end-state.
/// `ops`/`verify` are plain function pointers (no captured state) — a scenario that needs to
/// thread specifics from build to verify bakes them into its own functions as literals rather
/// than widening this struct.
struct Scenario {
    name: &'static str,
    build: BuildFn,
    ops: OpsFn,
    verify: VerifyFn,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "whole_hunk_stage",
            build: whole_hunk_build,
            ops: whole_hunk_stage_ops,
            verify: whole_hunk_stage_verify,
        },
        Scenario {
            name: "whole_hunk_unstage",
            build: whole_hunk_build,
            ops: whole_hunk_unstage_ops,
            verify: whole_hunk_unstage_verify,
        },
        Scenario {
            name: "whole_hunk_discard",
            build: whole_hunk_build,
            ops: whole_hunk_discard_ops,
            verify: whole_hunk_discard_verify,
        },
        Scenario {
            name: "partial_stage_adds_only",
            build: adds_only_build,
            ops: partial_stage_adds_only_ops,
            verify: partial_stage_adds_only_verify,
        },
        Scenario {
            name: "partial_stage_dels_only",
            build: dels_only_build,
            ops: partial_stage_dels_only_ops,
            verify: partial_stage_dels_only_verify,
        },
        Scenario {
            name: "partial_stage_mixed",
            build: two_change_build,
            ops: partial_stage_mixed_ops,
            verify: partial_stage_mixed_verify,
        },
        Scenario {
            name: "partial_unstage",
            build: two_change_build,
            ops: partial_unstage_ops,
            verify: partial_unstage_verify,
        },
        Scenario {
            name: "partial_discard",
            build: two_change_build,
            ops: partial_discard_ops,
            verify: partial_discard_verify,
        },
        Scenario {
            name: "eofnl_whole_hunk_stage",
            build: eofnl_whole_build,
            ops: eofnl_whole_stage_ops,
            verify: eofnl_whole_stage_verify,
        },
        Scenario {
            name: "eofnl_whole_hunk_unstage",
            build: eofnl_whole_build,
            ops: eofnl_whole_unstage_ops,
            verify: eofnl_whole_unstage_verify,
        },
        Scenario {
            name: "eofnl_whole_hunk_discard",
            build: eofnl_whole_build,
            ops: eofnl_whole_discard_ops,
            verify: eofnl_whole_discard_verify,
        },
        Scenario {
            name: "eofnl_partial_splice_stage",
            build: eofnl_splice_build,
            ops: eofnl_splice_stage_ops,
            verify: eofnl_splice_stage_verify,
        },
        Scenario {
            name: "multi_hunk_single_hunk_staged",
            build: multi_hunk_build,
            ops: multi_hunk_ops,
            verify: multi_hunk_verify,
        },
        Scenario {
            name: "space_in_filename_stage",
            build: space_in_filename_build,
            ops: space_in_filename_ops,
            verify: space_in_filename_verify,
        },
        Scenario {
            name: "rename_read_only",
            build: rename_build,
            ops: rename_ops,
            verify: rename_verify,
        },
        Scenario {
            name: "untracked_stage",
            build: untracked_build,
            ops: untracked_stage_ops,
            verify: untracked_stage_verify,
        },
        Scenario {
            name: "deleted_stage",
            build: deleted_build,
            ops: deleted_stage_ops,
            verify: deleted_stage_verify,
        },
        Scenario {
            name: "discard_untracked",
            build: untracked_build,
            ops: discard_untracked_ops,
            verify: discard_untracked_verify,
        },
        Scenario {
            name: "unstage_staged_new",
            build: staged_new_build,
            ops: unstage_staged_new_ops,
            verify: unstage_staged_new_verify,
        },
        Scenario {
            name: "refusal_lines_on_untracked",
            build: untracked_build,
            ops: refusal_lines_on_untracked_ops,
            verify: refusal_lines_on_untracked_verify,
        },
        Scenario {
            name: "refusal_lines_on_deleted",
            build: deleted_build,
            ops: refusal_lines_on_deleted_ops,
            verify: refusal_lines_on_deleted_verify,
        },
        Scenario {
            name: "staging_storm",
            build: staging_storm_build,
            ops: staging_storm_ops,
            verify: staging_storm_verify,
        },
        Scenario {
            name: "executable_whole_hunk_stage",
            build: executable_build,
            ops: executable_whole_hunk_stage_ops,
            verify: executable_whole_hunk_stage_verify,
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// whole-hunk stage/unstage/discard
// ---------------------------------------------------------------------------------------------

const WHOLE_HUNK_COMMITTED: &str = "line1\nline2\nline3\n";
const WHOLE_HUNK_MODIFIED: &str = "line1\nCHANGED\nline3\n";

fn whole_hunk_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", WHOLE_HUNK_COMMITTED, WHOLE_HUNK_MODIFIED)
        .build()
        .expect("fixture build")
}

fn whole_hunk_stage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Stage)
}

fn whole_hunk_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        WHOLE_HUNK_MODIFIED.as_bytes().to_vec(),
    ));
    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        WHOLE_HUNK_MODIFIED.as_bytes().to_vec(),
    ));
}

fn whole_hunk_unstage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    // Setup: stage the full modification directly so the staged model sees it — the Unstage
    // patch's preimage is the index (plan risk #3), not what's under test here. See
    // `pre_stage`'s docs for why `index.read(true)` is load-bearing, not defensive.
    pre_stage(repo, "f.txt")?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.staged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Unstage)
}

fn whole_hunk_unstage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        WHOLE_HUNK_COMMITTED.as_bytes().to_vec(),
    ));
}

fn whole_hunk_discard_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Discard)
}

fn whole_hunk_discard_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        WHOLE_HUNK_COMMITTED.as_bytes().to_vec(),
    ));
}

// ---------------------------------------------------------------------------------------------
// partial stage: adds-only / dels-only / mixed; partial unstage; partial discard
// ---------------------------------------------------------------------------------------------

const ADDS_ONLY_COMMITTED: &str = "line1\nline2\nline3\n";
const ADDS_ONLY_MODIFIED: &str = "line1\nNEW\nline2\nline3\n";

fn adds_only_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", ADDS_ONLY_COMMITTED, ADDS_ONLY_MODIFIED)
        .build()
        .expect("fixture build")
}

fn partial_stage_adds_only_ops(
    repo: &Repository,
    applier: &dyn Applier,
) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let keep_add = line_index(file, 0, LineKind::Addition, "NEW\n");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [].into(),
    };
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage)
}

fn partial_stage_adds_only_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        ADDS_ONLY_MODIFIED.as_bytes().to_vec(),
    ));
}

const DELS_ONLY_COMMITTED: &str = "line1\nline2\nline3\n";
const DELS_ONLY_MODIFIED: &str = "line1\nline3\n";

fn dels_only_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", DELS_ONLY_COMMITTED, DELS_ONLY_MODIFIED)
        .build()
        .expect("fixture build")
}

fn partial_stage_dels_only_ops(
    repo: &Repository,
    applier: &dyn Applier,
) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let keep_del = line_index(file, 0, LineKind::Deletion, "line2\n");
    let sel = LineSelection {
        keep_adds: [].into(),
        keep_dels: [keep_del].into(),
    };
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage)
}

fn partial_stage_dels_only_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        DELS_ONLY_MODIFIED.as_bytes().to_vec(),
    ));
}

/// Two separate changes ("old2"->"new2", "old4"->"new4") in one hunk, separated by a context
/// line — the shape the direction rules (trap 1) need: keeping one change and dropping the
/// other must not treat the dropped one uniformly across stage/unstage/discard.
const TWO_CHANGE_COMMITTED: &str = "line1\nold2\nline3\nold4\nline5\n";
const TWO_CHANGE_MODIFIED: &str = "line1\nnew2\nline3\nnew4\nline5\n";

fn two_change_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", TWO_CHANGE_COMMITTED, TWO_CHANGE_MODIFIED)
        .build()
        .expect("fixture build")
}

fn first_change_selection(file: &FileChange) -> LineSelection {
    let keep_add = line_index(file, 0, LineKind::Addition, "new2\n");
    let keep_del = line_index(file, 0, LineKind::Deletion, "old2\n");
    LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [keep_del].into(),
    }
}

fn partial_stage_mixed_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let sel = first_change_selection(file);
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage)
}

fn partial_stage_mixed_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"line1\nnew2\nline3\nold4\nline5\n".to_vec(),
    ));
}

fn partial_unstage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    // Setup: stage the full modification first so the staged model (the correct preimage for
    // an Unstage patch) sees both changes. See `pre_stage`'s docs for why `index.read(true)` is
    // load-bearing.
    pre_stage(repo, "f.txt")?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.staged.files[0];
    let sel = first_change_selection(file);
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Unstage)
}

fn partial_unstage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"line1\nold2\nline3\nnew4\nline5\n".to_vec(),
    ));
}

fn partial_discard_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let sel = first_change_selection(file);
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Discard)
}

fn partial_discard_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        b"line1\nold2\nline3\nnew4\nline5\n".to_vec(),
    ));
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        TWO_CHANGE_COMMITTED.as_bytes().to_vec(),
    ));
}

// ---------------------------------------------------------------------------------------------
// EOFNL per verb (whole-hunk) + the trap-2 splice case (partial)
// ---------------------------------------------------------------------------------------------

const EOFNL_WHOLE_COMMITTED: &str = "line1\nline2\nline3\n";
const EOFNL_WHOLE_MODIFIED: &str = "line1\nline2\nline3"; // no trailing newline

fn eofnl_whole_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", EOFNL_WHOLE_COMMITTED, EOFNL_WHOLE_MODIFIED)
        .build()
        .expect("fixture build")
}

fn eofnl_whole_stage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Stage)
}

fn eofnl_whole_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        EOFNL_WHOLE_MODIFIED.as_bytes().to_vec(),
    ));
}

fn eofnl_whole_unstage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    // See `pre_stage`'s docs for why `index.read(true)` is load-bearing.
    pre_stage(repo, "f.txt")?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.staged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Unstage)
}

fn eofnl_whole_unstage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        EOFNL_WHOLE_COMMITTED.as_bytes().to_vec(),
    ));
}

fn eofnl_whole_discard_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Discard)
}

fn eofnl_whole_discard_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        EOFNL_WHOLE_COMMITTED.as_bytes().to_vec(),
    ));
}

/// Trap-2's precondition: the committed file's last line ("last") has no trailing newline; the
/// modification deletes that line and adds two new ones, the last of which ("more\n") DOES end
/// in a newline. Keeping only "more\n" drops the "last" deletion to context while it still
/// carries `missing_newline` — the shape `splice_eofnl_context_lines` exists to rewrite.
const EOFNL_SPLICE_COMMITTED: &str = "a\nb\nlast";
const EOFNL_SPLICE_MODIFIED: &str = "a\nb\nreplaced\nmore\n";

fn eofnl_splice_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", EOFNL_SPLICE_COMMITTED, EOFNL_SPLICE_MODIFIED)
        .build()
        .expect("fixture build")
}

fn eofnl_splice_stage_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let keep_add = line_index(file, 0, LineKind::Addition, "more\n");
    let sel = LineSelection {
        keep_adds: [keep_add].into(),
        keep_dels: [].into(),
    };
    apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage)
}

fn eofnl_splice_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        b"a\nb\nlast\nmore\n".to_vec(),
    ));
}

// ---------------------------------------------------------------------------------------------
// multi-hunk file: two changes far enough apart to form separate hunks; stage only one
// ---------------------------------------------------------------------------------------------

fn multi_hunk_committed() -> String {
    (1..=20).map(|n| format!("line{n}\n")).collect()
}

fn multi_hunk_modified() -> String {
    (1..=20)
        .map(|n| match n {
            2 => "CHANGED2\n".to_string(),
            18 => "CHANGED18\n".to_string(),
            n => format!("line{n}\n"),
        })
        .collect()
}

fn multi_hunk_hunk0_only() -> String {
    (1..=20)
        .map(|n| match n {
            2 => "CHANGED2\n".to_string(),
            n => format!("line{n}\n"),
        })
        .collect()
}

fn multi_hunk_build() -> Fixture {
    let committed = multi_hunk_committed();
    let modified = multi_hunk_modified();
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", &committed, &modified)
        .build()
        .expect("fixture build")
}

fn multi_hunk_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    assert_eq!(
        file.hunks.len(),
        2,
        "expected two separate hunks for the multi-hunk fixture, got {}",
        file.hunks.len()
    );
    apply_hunk(repo, applier, file, 0, StageVerb::Stage)
}

fn multi_hunk_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "f.txt",
        multi_hunk_hunk0_only().into_bytes(),
    ));
    fixture.assert(predicate::repo::workdir_file_equals(
        "f.txt",
        multi_hunk_modified().into_bytes(),
    ));
}

// ---------------------------------------------------------------------------------------------
// space-in-filename: proves header path handling through both parsers
// ---------------------------------------------------------------------------------------------

const SPACE_FILENAME_COMMITTED: &str = "line1\nline2\nline3\n";
const SPACE_FILENAME_MODIFIED: &str = "line1\nCHANGED\nline3\n";

fn space_in_filename_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file(
            "my file.txt",
            SPACE_FILENAME_COMMITTED,
            SPACE_FILENAME_MODIFIED,
        )
        .build()
        .expect("fixture build")
}

fn space_in_filename_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Stage)
}

fn space_in_filename_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "my file.txt",
        SPACE_FILENAME_MODIFIED.as_bytes().to_vec(),
    ));
}

// ---------------------------------------------------------------------------------------------
// rename: READ-side only. Rename patches only arise from tree_to_tree diffs (diff_committed),
// never from uncommitted diffs — staging a rename hunk against the index is not a v1 write op,
// so this scenario has no write side to drive through ops.rs; `ops` is a deliberate no-op and
// the assertions live entirely in `verify`.
// ---------------------------------------------------------------------------------------------

const RENAME_CONTENT: &str = "shared content across the rename\nline two\nline three\n";

fn rename_build() -> Fixture {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .bare(true)
        .worktree("main")
        .build()
        .expect("fixture build");

    fixture
        .commit("main")
        .file("old.txt", RENAME_CONTENT)
        .create("add old.txt")
        .expect("commit old.txt");

    // CommitBuilder only adds files; build the rename commit by hand (remove old.txt, add
    // new.txt with identical content so `find_similar` detects it as a rename, not a
    // delete+add pair). Scoped so every borrow of `fixture`/`repo` ends before it's returned.
    {
        let repo = fixture.repo().expect("repo");
        let workdir = repo.workdir().expect("workdir");
        std::fs::remove_file(workdir.join("old.txt")).expect("remove old.txt");
        std::fs::write(workdir.join("new.txt"), RENAME_CONTENT).expect("write new.txt");

        let mut index = repo.index().expect("index");
        index
            .remove_path(Path::new("old.txt"))
            .expect("remove_path");
        index.add_path(Path::new("new.txt")).expect("add_path");
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("write_tree");
        let tree = repo.find_tree(tree_id).expect("find_tree");
        let sig = git2::Signature::now("Test User", "test@example.com").expect("signature");
        let parent = repo.head().expect("head").peel_to_commit().expect("peel");
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "rename old.txt to new.txt",
            &tree,
            &[&parent],
        )
        .expect("commit rename");
    }

    fixture
}

fn rename_ops(_repo: &Repository, _applier: &dyn Applier) -> Result<(), ReviewError> {
    Ok(())
}

fn rename_verify(fixture: &Fixture) {
    let repo = fixture.repo().expect("repo");
    let head_commit = repo.head().expect("head").peel_to_commit().expect("peel");
    let head_oid = head_commit.id();
    let base_oid = head_commit.parent(0).expect("parent commit").id();

    let diff = diff_committed(repo, base_oid, head_oid).expect("diff_committed");
    let file = diff
        .files
        .iter()
        .find(|f| f.path == "new.txt")
        .expect("renamed file present as new.txt");

    assert_eq!(
        file.status,
        FileStatus::Renamed,
        "expected Renamed status, got {:?}",
        file.status
    );
    assert_eq!(file.old_path.as_deref(), Some("old.txt"));
}

// ---------------------------------------------------------------------------------------------
// untracked / added / deleted file ops
// ---------------------------------------------------------------------------------------------

fn untracked_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\n")
        .build()
        .expect("fixture build")
}

fn untracked_stage_ops(repo: &Repository, _applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Stage)
}

fn untracked_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::has_staged_file("new.txt"));
    fixture.assert(predicate::repo::index_blob_equals(
        "new.txt",
        b"hello\n".to_vec(),
    ));
}

fn deleted_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .deleted_file("gone.txt", "content\n")
        .build()
        .expect("fixture build")
}

fn deleted_stage_ops(repo: &Repository, _applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Stage)
}

fn deleted_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::has_staged_deletion("gone.txt"));
}

fn discard_untracked_ops(repo: &Repository, _applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_file(repo, file, StageVerb::Discard)
}

fn discard_untracked_verify(fixture: &Fixture) {
    let repo = fixture.repo().expect("repo");
    assert!(!repo.workdir().unwrap().join("new.txt").exists());
}

fn staged_new_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .staged_file("added.txt", "hello\n")
        .build()
        .expect("fixture build")
}

fn unstage_staged_new_ops(repo: &Repository, _applier: &dyn Applier) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.staged.files[0];
    apply_file(repo, file, StageVerb::Unstage)
}

fn unstage_staged_new_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::has_untracked_file("added.txt"));
    let repo = fixture.repo().expect("repo");
    let mut index = repo.index().expect("index");
    index.read(true).expect("index reload");
    assert!(
        index.get_path(Path::new("added.txt"), 0).is_none(),
        "expected no index entry for added.txt after unstage"
    );
}

// ---------------------------------------------------------------------------------------------
// refusals: apply_lines on untracked/deleted files never reaches an applier, so these can never
// diverge between backends — kept for grid completeness per the plan.
// ---------------------------------------------------------------------------------------------

fn refusal_lines_on_untracked_ops(
    repo: &Repository,
    applier: &dyn Applier,
) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let sel = LineSelection::default();
    let result = apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage);
    assert!(
        matches!(
            result,
            Err(ReviewError::Synthesis(
                SynthesisError::LineSelectionUnsupported { .. }
            ))
        ),
        "expected LineSelectionUnsupported, got {result:?}"
    );
    Ok(())
}

fn refusal_lines_on_untracked_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::has_untracked_file("new.txt"));
}

fn refusal_lines_on_deleted_ops(
    repo: &Repository,
    applier: &dyn Applier,
) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let sel = LineSelection::default();
    let result = apply_lines(repo, applier, file, 0, &sel, StageVerb::Stage);
    assert!(
        matches!(
            result,
            Err(ReviewError::Synthesis(
                SynthesisError::LineSelectionUnsupported { .. }
            ))
        ),
        "expected LineSelectionUnsupported, got {result:?}"
    );
    Ok(())
}

fn refusal_lines_on_deleted_verify(_fixture: &Fixture) {
    // Nothing was ever applied — the refusal itself (asserted in `ops`) is the whole scenario.
}

// ---------------------------------------------------------------------------------------------
// staging storm: stage a subset, unstage a different subset, discard the rest in one sequence,
// then assert the exact three-way (index/workdir) end state. Driven through `apply_hunk` (not
// `apply_file`) so the sequence exercises the `Applier` on all three files, not just index
// plumbing — `StagingQueue` integration was considered (plan explicitly allows it) but skipped:
// queue.rs's own test suite already covers FIFO/live-index semantics, and wiring `StagingOp`
// here would test queue plumbing instead of the storm's actual end-state, which is the point of
// this scenario.
// ---------------------------------------------------------------------------------------------

const STORM_A_COMMITTED: &str = "a1\na2\na3\n";
const STORM_A_MODIFIED: &str = "a1\nCHANGED_A\na3\n";
const STORM_B_COMMITTED: &str = "b1\nb2\nb3\n";
const STORM_B_MODIFIED: &str = "b1\nCHANGED_B\nb3\n";
const STORM_C_COMMITTED: &str = "c1\nc2\nc3\n";
const STORM_C_MODIFIED: &str = "c1\nCHANGED_C\nc3\n";

fn staging_storm_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("fileA.txt", STORM_A_COMMITTED, STORM_A_MODIFIED)
        .unstaged_file("fileB.txt", STORM_B_COMMITTED, STORM_B_MODIFIED)
        .unstaged_file("fileC.txt", STORM_C_COMMITTED, STORM_C_MODIFIED)
        .build()
        .expect("fixture build")
}

fn staging_storm_ops(repo: &Repository, applier: &dyn Applier) -> Result<(), ReviewError> {
    // Setup: pre-stage fileB's modification (not part of the storm itself) so the storm can
    // unstage it. See `pre_stage`'s docs for why `index.read(true)` is load-bearing here, not
    // defensive — this fixture's three-file baseline commit is exactly the multi-path case that
    // bites without the reload.
    pre_stage(repo, "fileB.txt")?;

    let diffs = diff_uncommitted(repo)?;
    let file_a = diffs
        .unstaged
        .files
        .iter()
        .find(|f| f.path == "fileA.txt")
        .expect("fileA.txt in unstaged diff");
    let file_c = diffs
        .unstaged
        .files
        .iter()
        .find(|f| f.path == "fileC.txt")
        .expect("fileC.txt in unstaged diff");
    let file_b = diffs
        .staged
        .files
        .iter()
        .find(|f| f.path == "fileB.txt")
        .expect("fileB.txt in staged diff");

    apply_hunk(repo, applier, file_a, 0, StageVerb::Stage)?;
    apply_hunk(repo, applier, file_b, 0, StageVerb::Unstage)?;
    apply_hunk(repo, applier, file_c, 0, StageVerb::Discard)?;
    Ok(())
}

fn staging_storm_verify(fixture: &Fixture) {
    // Staged: index has the change, workdir untouched (Stage targets the index only).
    fixture.assert(predicate::repo::index_blob_equals(
        "fileA.txt",
        STORM_A_MODIFIED.as_bytes().to_vec(),
    ));
    fixture.assert(predicate::repo::workdir_file_equals(
        "fileA.txt",
        STORM_A_MODIFIED.as_bytes().to_vec(),
    ));
    // Unstaged: index reverted to HEAD, workdir untouched (Unstage targets the index only).
    fixture.assert(predicate::repo::index_blob_equals(
        "fileB.txt",
        STORM_B_COMMITTED.as_bytes().to_vec(),
    ));
    fixture.assert(predicate::repo::workdir_file_equals(
        "fileB.txt",
        STORM_B_MODIFIED.as_bytes().to_vec(),
    ));
    // Discarded: workdir reverted to HEAD; index was never touched (still matches HEAD).
    fixture.assert(predicate::repo::workdir_file_equals(
        "fileC.txt",
        STORM_C_COMMITTED.as_bytes().to_vec(),
    ));
    fixture.assert(predicate::repo::index_blob_equals(
        "fileC.txt",
        STORM_C_COMMITTED.as_bytes().to_vec(),
    ));
}

// ---------------------------------------------------------------------------------------------
// executable file (100755) whole-hunk stage — pins the exec-bit-mode divergence class the
// 2026-07-06 stack review found: `PatchText::to_bytes` used to hardcode `index 0000000..0000000
// 100644` on the synthesized patch's index line, so staging any hunk of an executable file via
// `Git2Applier` silently reset its index mode to `100644` — a real divergence `CliApplier` never
// had (it reads the mode from the working tree). Fixed by threading the real mode through
// `FileChange`/`PatchText` (see `synthesis.rs`).
// ---------------------------------------------------------------------------------------------

const EXECUTABLE_COMMITTED: &str = "#!/bin/sh\necho committed\n";
const EXECUTABLE_MODIFIED: &str = "#!/bin/sh\necho modified\n";

fn executable_build() -> Fixture {
    FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .executable_unstaged_file("run.sh", EXECUTABLE_COMMITTED, EXECUTABLE_MODIFIED)
        .build()
        .expect("fixture build")
}

fn executable_whole_hunk_stage_ops(
    repo: &Repository,
    applier: &dyn Applier,
) -> Result<(), ReviewError> {
    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    apply_hunk(repo, applier, file, 0, StageVerb::Stage)
}

fn executable_whole_hunk_stage_verify(fixture: &Fixture) {
    fixture.assert(predicate::repo::index_blob_equals(
        "run.sh",
        EXECUTABLE_MODIFIED.as_bytes().to_vec(),
    ));
    // The regression: staging must NOT clobber the index entry's mode back to 0o100644.
    fixture.assert(predicate::repo::has_index_mode("run.sh", 0o100755));
}

// ---------------------------------------------------------------------------------------------
// The verdict tests
// ---------------------------------------------------------------------------------------------

/// The ORACLE: every scenario must pass cleanly against `CliApplier`. No skip-if-missing — the
/// `git` binary is required. A panic here is a real bug in the corpus or the write path, not a
/// divergence to collect.
#[test]
fn corpus_against_cli() {
    for scenario in scenarios() {
        let fixture = (scenario.build)();
        let repo = fixture.repo().expect("repo");
        (scenario.ops)(repo, &CliApplier)
            .unwrap_or_else(|err| panic!("{}: CLI ops failed: {err}", scenario.name));
        (scenario.verify)(&fixture);
    }
}

/// Run one scenario's ops+verify against `Git2Applier`, catching panics so one scenario's
/// failure doesn't abort the rest of the corpus.
///
/// SOUNDNESS of `AssertUnwindSafe`: `&Repository` isn't `UnwindSafe`, so the closure capturing
/// `repo`/`fixture` can't be passed to `catch_unwind` without asserting it. This is sound
/// because each scenario's fixture is built fresh, used exactly once, and dropped immediately
/// after — whether or not a panic occurs — so there is no unwind-poisoned shared state for a
/// later scenario (or a later assertion on the SAME fixture) to observe.
fn run_scenario_against_git2(scenario: &Scenario) -> Result<(), String> {
    let fixture = (scenario.build)();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let repo = fixture.repo().expect("repo");
        (scenario.ops)(repo, &Git2Applier).map_err(|err| format!("git2 ops errored: {err}"))?;
        (scenario.verify)(&fixture);
        Ok::<(), String>(())
    }));
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(detail)) => Err(detail),
        Err(payload) => Err(format!(
            "git2 pass panicked: {}",
            panic_payload_message(&payload)
        )),
    }
}

/// Extract a human-readable message from a `catch_unwind` panic payload — `panic!("{msg}")` and
/// `assert!`/`unwrap`/`expect` failures carry it as `&'static str` or `String` depending on
/// whether the message was formatted; anything else (a non-string payload) falls back to a
/// fixed placeholder so a [`KNOWN_DIVERGENCES`] entry can still cite whatever detail IS
/// available instead of a uniform, evidence-free string for every panic.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "(non-string panic payload)".to_string()
    }
}

/// Known divergences between `Git2Applier` and the `CliApplier` oracle, one entry per
/// `"<scenario name>: <what diverged>"`. Kept explicit (rather than a bare `assert!(is_empty())`)
/// so a future divergence must be added here WITH the evidence of what it is, not silently
/// swallowed by loosening this test.
const KNOWN_DIVERGENCES: &[&str] = &[];

/// The VERDICT: renders the git2-vs-CLI comparison for `docs/rfc/workon-review.md`'s "M2
/// verdict" section. Collects divergences instead of panicking per-scenario so the full set is
/// visible in one run.
#[test]
fn corpus_against_git2() {
    let mut divergences: Vec<String> = Vec::new();
    for scenario in scenarios() {
        if let Err(detail) = run_scenario_against_git2(&scenario) {
            divergences.push(format!("{}: {detail}", scenario.name));
        }
    }
    divergences.sort();

    let mut expected: Vec<String> = KNOWN_DIVERGENCES.iter().map(|s| s.to_string()).collect();
    expected.sort();

    assert_eq!(
        divergences, expected,
        "git2 divergence set changed vs. KNOWN_DIVERGENCES — update the allowlist WITH evidence, \
         don't just widen it to pass"
    );
}
