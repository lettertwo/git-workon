//! Model-shape and byte-fidelity tests for `workon_review::model`/`workon_review::acquire`.
//!
//! The EOFNL characterization test pins what git2 0.21 actually emits for a no-trailing-newline
//! file (plan risk #2) — this is normative for CS2/CS3's patch synthesis, not just a sanity
//! check. Fixtures used for byte assertions pin `core.autocrlf=false` so bytes are
//! platform-stable (plan risk #6).

use git2::{BranchType, Oid, Repository};
use git_workon_fixture::prelude::*;
use workon::{assemble_changesets, Changeset, ChangesetSpan, StackModel, UncommittedLayer};
use workon_review::acquire::{diff_changeset, diff_committed, diff_uncommitted, ChangesetDiff};
use workon_review::error::DiffError;
use workon_review::model::{FileStatus, LineKind};

/// Commit `path`/`content` as a child of `parent`, without moving any branch ref — callers
/// reassign a branch to the returned `Oid` via `Fixture::update_branch` themselves. Used where
/// the `deleted_file`/`unstaged_file` baseline builders don't fit (advancing one Graphite
/// branch's tip independent of `main`'s).
fn commit_onto(repo: &Repository, parent: &git2::Commit, path: &str, content: &str) -> Oid {
    let mut treebuilder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
    let blob_oid = repo.blob(content.as_bytes()).unwrap();
    treebuilder
        .insert(path, blob_oid, git2::FileMode::Blob.into())
        .unwrap();
    let tree_oid = treebuilder.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(None, &sig, &sig, "test commit", &tree, &[parent])
        .unwrap()
}

// ── model shape ──────────────────────────────────────────────────────────────

#[test]
fn staged_file_is_added_with_no_hunks_diff_needed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .staged_file("new.txt", "hello\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.staged.files.len(), 1);
    let file = &diffs.staged.files[0];
    assert_eq!(file.path, "new.txt");
    assert_eq!(file.status, FileStatus::Added);
    assert!(!file.is_binary);
    assert_eq!(diffs.unstaged.files.len(), 0);

    Ok(())
}

#[test]
fn unstaged_file_is_modified_with_one_hunk() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file(
            "tracked.txt",
            "line1\nline2\nline3\n",
            "line1\nCHANGED\nline3\n",
        )
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.unstaged.files.len(), 1);
    let file = &diffs.unstaged.files[0];
    assert_eq!(file.path, "tracked.txt");
    assert_eq!(file.status, FileStatus::Modified);
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(diffs.staged.files.len(), 0);

    Ok(())
}

#[test]
fn untracked_file_has_full_content_as_addition() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\nworld\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.unstaged.files.len(), 1);
    let file = &diffs.unstaged.files[0];
    assert_eq!(file.path, "new.txt");
    assert_eq!(file.status, FileStatus::Untracked);
    assert_eq!(file.hunks.len(), 1);
    assert!(file.hunks[0]
        .lines
        .iter()
        .all(|l| l.kind == LineKind::Addition));

    Ok(())
}

#[test]
fn deleted_file_is_deleted_status() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .deleted_file("gone.txt", "content\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.unstaged.files.len(), 1);
    let file = &diffs.unstaged.files[0];
    assert_eq!(file.path, "gone.txt");
    assert_eq!(file.status, FileStatus::Deleted);

    Ok(())
}

#[test]
fn renamed_file_carries_old_path() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .build()?;
    let repo = fixture.repo()?;

    let base = repo.head()?.peel_to_commit()?;
    let base_oid = commit_onto(repo, &base, "old.txt", "line1\nline2\nline3\n");
    let base_commit = repo.find_commit(base_oid)?;

    // Rename: drop old.txt, add new.txt with the same (similar-enough) content.
    let mut treebuilder = repo.treebuilder(Some(&base_commit.tree()?))?;
    treebuilder.remove("old.txt")?;
    let blob_oid = repo.blob(b"line1\nline2\nline3\n")?;
    treebuilder.insert("new.txt", blob_oid, git2::FileMode::Blob.into())?;
    let tree_oid = treebuilder.write()?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = repo.signature()?;
    let head_oid = repo.commit(None, &sig, &sig, "rename", &tree, &[&base_commit])?;

    let model = diff_committed(repo, base_oid, head_oid)?;
    assert_eq!(model.files.len(), 1);
    let file = &model.files[0];
    assert_eq!(file.status, FileStatus::Renamed);
    assert_eq!(file.path, "new.txt");
    assert_eq!(file.old_path.as_deref(), Some("old.txt"));

    Ok(())
}

#[test]
fn binary_file_has_no_hunks() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .bare(true)
        .worktree("main")
        .build()?;

    let base_oid = fixture
        .commit("main")
        .file_bytes("bin.dat", vec![0u8, 1, 2, 3, b'a', 0u8])
        .create("add binary")?;
    let head_oid = fixture
        .commit("main")
        .file_bytes("bin.dat", vec![0u8, 9, 9, 9, b'z', 0u8])
        .create("change binary")?;

    let repo = fixture.repo()?;
    let model = diff_committed(repo, base_oid, head_oid)?;
    assert_eq!(model.files.len(), 1);
    let file = &model.files[0];
    assert_eq!(file.path, "bin.dat");
    assert!(file.is_binary);
    assert!(file.hunks.is_empty());

    Ok(())
}

// ── EOFNL characterization (plan risk #2 — normative for CS2/CS3) ────────────

/// Pins git2 0.21's actual EOFNL behavior for a file with no trailing newline whose middle
/// line changes: git2 emits the trailing context line WITHOUT its newline, immediately
/// followed by a `ContextEOFNL` ('=') marker line whose content is exactly
/// `"\n\\ No newline at end of file\n"`. No pseudo-line lands in the model — the marker sets
/// `missing_newline` on the preceding (already-pushed) context [`HunkLine`].
#[test]
fn eofnl_context_marker_sets_missing_newline_on_preceding_line(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file(
            "f.txt",
            "line1\nline2\nline3",
            "line1\nline2-changed\nline3",
        )
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    assert_eq!(file.hunks.len(), 1);
    let lines = &file.hunks[0].lines;

    // Exactly 4 real lines: no pseudo-line for the EOFNL marker.
    assert_eq!(lines.len(), 4);

    assert_eq!(lines[0].kind, LineKind::Context);
    assert_eq!(lines[0].content, b"line1\n");
    assert!(!lines[0].missing_newline);

    assert_eq!(lines[1].kind, LineKind::Deletion);
    assert_eq!(lines[1].content, b"line2\n");
    assert!(!lines[1].missing_newline);

    assert_eq!(lines[2].kind, LineKind::Addition);
    assert_eq!(lines[2].content, b"line2-changed\n");
    assert!(!lines[2].missing_newline);

    // The trailing context line: git2 hands back content WITHOUT the newline, and the
    // ContextEOFNL marker (content "\n\\ No newline at end of file\n") sets the flag instead
    // of appearing as its own line.
    assert_eq!(lines[3].kind, LineKind::Context);
    assert_eq!(lines[3].content, b"line3");
    assert!(lines[3].missing_newline);

    Ok(())
}

/// Mirror of the context case, but the DELETION side (old file) lacks the trailing newline —
/// git2 emits the marker as `AddEOFNL` ('>'), despite the name marking the OLD/`-` side, not
/// the `+` side. Verified empirically; do not trust the enum name.
#[test]
fn eofnl_marker_on_deletion_side_when_old_file_lacks_trailing_newline(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "line1\nline2\nline3", "line1\nline2\nline3\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    assert_eq!(file.hunks.len(), 1);
    let lines = &file.hunks[0].lines;

    // context, context, deletion(no nl, flagged), addition(with nl) — 4 real lines.
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[2].kind, LineKind::Deletion);
    assert_eq!(lines[2].content, b"line3");
    assert!(lines[2].missing_newline);
    assert_eq!(lines[3].kind, LineKind::Addition);
    assert_eq!(lines[3].content, b"line3\n");
    assert!(!lines[3].missing_newline);

    Ok(())
}

/// Mirror again: the ADDITION side (new file) lacks the trailing newline — git2 emits
/// `DeleteEOFNL` ('<'), again the mirror of what the name suggests.
#[test]
fn eofnl_marker_on_addition_side_when_new_file_lacks_trailing_newline(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file("f.txt", "line1\nline2\nline3\n", "line1\nline2\nline3")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    let file = &diffs.unstaged.files[0];
    let lines = &file.hunks[0].lines;

    assert_eq!(lines.len(), 4);
    assert_eq!(lines[2].kind, LineKind::Deletion);
    assert_eq!(lines[2].content, b"line3\n");
    assert!(!lines[2].missing_newline);
    assert_eq!(lines[3].kind, LineKind::Addition);
    assert_eq!(lines[3].content, b"line3");
    assert!(lines[3].missing_newline);

    Ok(())
}

// ── byte-fidelity ─────────────────────────────────────────────────────────────

/// Render the hunk-body bytes (hunk header + lines, no file header) straight off
/// `Diff::print(DiffFormat::Patch)`, the same way real diff text is produced — the reference
/// [`Hunk::to_diff_bytes`] is pinned against.
fn print_hunk_bytes(diff: &git2::Diff<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin_value() {
            git2::DiffLineType::FileHeader => {}
            git2::DiffLineType::HunkHeader
            | git2::DiffLineType::ContextEOFNL
            | git2::DiffLineType::AddEOFNL
            | git2::DiffLineType::DeleteEOFNL => {
                out.extend_from_slice(line.content());
            }
            git2::DiffLineType::Context => {
                out.push(b' ');
                out.extend_from_slice(line.content());
            }
            git2::DiffLineType::Addition => {
                out.push(b'+');
                out.extend_from_slice(line.content());
            }
            git2::DiffLineType::Deletion => {
                out.push(b'-');
                out.extend_from_slice(line.content());
            }
            git2::DiffLineType::Binary => {}
        }
        true
    })
    .unwrap();
    out
}

#[test]
fn hunk_to_diff_bytes_matches_diff_print() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .unstaged_file(
            "f.txt",
            "line1\nline2\nline3\nline4\n",
            "line1\nCHANGED\nline3\nline4",
        )
        .build()?;
    let repo = fixture.repo()?;

    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;

    let model = workon_review::model::DiffModel::from_git2(&diff)?;
    assert_eq!(model.files.len(), 1);
    assert_eq!(model.files[0].hunks.len(), 1);

    let expected = print_hunk_bytes(&diff);
    let actual = model.files[0].hunks[0].to_diff_bytes();
    assert_eq!(
        String::from_utf8_lossy(&actual),
        String::from_utf8_lossy(&expected)
    );
    assert_eq!(actual, expected);

    Ok(())
}

// ── combined diff (CS1: HEAD ↔ worktree-with-index, M3's default zoom) ───────

#[test]
fn partially_staged_file_appears_fused_in_combined() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .partially_staged_file(
            "f.txt",
            "line1\nline2\nline3\n",
            "line1\nSTAGED\nline3\n",
            "line1\nSTAGED\nWORKDIR\n",
        )
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    // Split views each see only their own half of the change.
    assert_eq!(diffs.staged.files.len(), 1);
    assert_eq!(diffs.unstaged.files.len(), 1);

    // Combined fuses both onto one file, diffing straight from HEAD to the workdir.
    assert_eq!(diffs.combined.files.len(), 1);
    let file = &diffs.combined.files[0];
    assert_eq!(file.path, "f.txt");
    assert_eq!(file.status, FileStatus::Modified);
    assert_eq!(file.hunks.len(), 1);
    let added: Vec<&[u8]> = file.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Addition)
        .map(|l| l.content.as_slice())
        .collect();
    // Both the staged AND the unstaged edit show up as additions in the one fused hunk.
    assert!(added.contains(&b"STAGED\n".as_slice()));
    assert!(added.contains(&b"WORKDIR\n".as_slice()));

    Ok(())
}

#[test]
fn untracked_file_appears_as_added_in_combined() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .untracked_file("new.txt", "hello\nworld\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.combined.files.len(), 1);
    let file = &diffs.combined.files[0];
    assert_eq!(file.path, "new.txt");
    // Matches the unstaged side's convention (see `untracked_file_has_full_content_as_addition`):
    // git2 reports untracked deltas as `Delta::Untracked`, not `Delta::Added` — all lines are
    // still additions since there is no pre-image.
    assert_eq!(file.status, FileStatus::Untracked);
    assert!(file.hunks[0]
        .lines
        .iter()
        .all(|l| l.kind == LineKind::Addition));

    Ok(())
}

#[test]
fn renamed_in_worktree_file_surfaces_as_renamed_in_combined(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        // Deleted from the working tree, still in HEAD/index...
        .deleted_file("old.txt", "line1\nline2\nline3\nline4\nline5\n")
        // ...and a same-content untracked file lands under a new name — a worktree rename
        // `find_similar` must pair up.
        .untracked_file("new.txt", "line1\nline2\nline3\nline4\nline5\n")
        .build()?;
    let repo = fixture.repo()?;

    let diffs = diff_uncommitted(repo)?;
    assert_eq!(diffs.combined.files.len(), 1);
    let file = &diffs.combined.files[0];
    assert_eq!(file.status, FileStatus::Renamed);
    assert_eq!(file.path, "new.txt");
    assert_eq!(file.old_path.as_deref(), Some("old.txt"));

    Ok(())
}

// ── diff_changeset over a real assemble_changesets result ─────────────────────

#[test]
fn diff_changeset_over_real_graphite_stack() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .build()?;
    let repo = fixture.repo()?;

    let main_tip = repo
        .find_branch("main", BranchType::Local)?
        .get()
        .target()
        .unwrap();
    let main_commit = repo.find_commit(main_tip)?;
    // Advance "a" independently of "main" so base != head.
    let a_head = commit_onto(repo, &main_commit, "feature.txt", "hello\n");
    fixture.update_branch("a", a_head)?;

    let changesets =
        assemble_changesets(repo, "a", StackModel::Graphite, UncommittedLayer::Include)?;
    let a_cs = changesets
        .iter()
        .find(|c| c.name == "a")
        .expect("assembled changeset for 'a'");

    match diff_changeset(repo, a_cs)? {
        ChangesetDiff::Committed(model) => {
            assert_eq!(model.files.len(), 1);
            assert_eq!(model.files[0].path, "feature.txt");
            assert_eq!(model.files[0].status, FileStatus::Added);
        }
        ChangesetDiff::Uncommitted(_) => panic!("expected a Committed diff for a Graphite node"),
    }

    Ok(())
}

#[test]
fn diff_changeset_with_bad_base_oid_fails_never_empty() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;
    let head = repo.head()?.peel_to_commit()?.id();

    let cs = Changeset {
        name: "bogus".to_string(),
        span: ChangesetSpan::Committed {
            base: Oid::ZERO_SHA1,
            head,
        },
        title: None,
        current: false,
        needs_restack: false,
    };

    let err = diff_changeset(repo, &cs).expect_err("a garbage base Oid must error, not diff empty");
    match err {
        DiffError::ChangesetDiffFailed { name, .. } => assert_eq!(name, "bogus"),
        other => panic!("expected ChangesetDiffFailed, got {other:?}"),
    }

    Ok(())
}
