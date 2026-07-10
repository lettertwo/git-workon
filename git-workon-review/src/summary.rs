//! CS4's summary panel: pure builders for the renderable data `render.rs`'s `render_summary`
//! paints when the outline is OPEN AND FOCUSED and its cursor rests on a
//! [`crate::outline::OutlineItem::Header`]/[`crate::outline::OutlineItem::Dir`] row instead of a
//! file — mirrors [`crate::outline`]'s pure-module posture (no [`crate::app::App`]/git2
//! dependency): everything here is built from `&[FileChange]`-shaped inputs plus a handful of
//! primitives `App::summary_for` supplies.
//!
//! ## What a changeset summary can show
//!
//! `workon::Changeset` (see `git-workon-lib/src/changeset.rs`) exposes only `name`/`title` for a
//! changeset today — no commit body/message. [`changeset_summary`] therefore renders the
//! label (title, falling back to name — the same rule the winbar/outline header already use)
//! plus the diffstat; there is no commit-message row. Surfacing the commit body would need a
//! `repo.find_commit` lookup keyed off the changeset's head OID — left as a follow-up, not part
//! of this changeset's scope.

use crate::model::{FileChange, LineKind};

/// Count added/deleted LINES across `change`'s hunks — `(adds, dels)`. A binary file (no hunks)
/// counts as `(0, 0)`; `LineKind::Context` lines never count toward either total.
pub fn file_diffstat(change: &FileChange) -> (usize, usize) {
    let mut adds = 0usize;
    let mut dels = 0usize;
    for hunk in &change.hunks {
        for line in &hunk.lines {
            match line.kind {
                LineKind::Addition => adds += 1,
                LineKind::Deletion => dels += 1,
                LineKind::Context => {}
            }
        }
    }
    (adds, dels)
}

/// One file's row in a summary panel's per-file list — just enough to render `"path  +N -M"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryFileRow {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
}

/// Build the per-file row list plus its `(total_adds, total_dels)` from `files` — shared by
/// [`changeset_summary`] and [`dir_summary`], the only difference between the two being which
/// files the caller has already filtered down to.
fn file_rows(files: &[FileChange]) -> (Vec<SummaryFileRow>, usize, usize) {
    let rows: Vec<SummaryFileRow> = files
        .iter()
        .map(|f| {
            let (adds, dels) = file_diffstat(f);
            SummaryFileRow {
                path: f.path.clone(),
                adds,
                dels,
            }
        })
        .collect();
    let total_adds = rows.iter().map(|r| r.adds).sum();
    let total_dels = rows.iter().map(|r| r.dels).sum();
    (rows, total_adds, total_dels)
}

/// Renderable summary for a Header-row outline selection: the changeset's own flags/label (the
/// same fields [`crate::outline::OutlineChangeset`] carries) plus a per-file diffstat breakdown.
/// `loading`/`failed` mirror ADR-037's slot state — when either is set, `files` is always empty
/// (a `Pending`/`Failed` [`crate::app::ChangesetView`] never has a real file list), so
/// `render_summary` shows the loading/failure line in place of the file rows rather than an
/// empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetSummary {
    pub label: String,
    pub current: bool,
    pub needs_restack: bool,
    pub loading: bool,
    pub failed: bool,
    /// The acquisition failure message (ADR-037), `Some` only when `failed`.
    pub failure_message: Option<String>,
    pub files: Vec<SummaryFileRow>,
    pub total_adds: usize,
    pub total_dels: usize,
}

/// Build a [`ChangesetSummary`] from a changeset's outline-relevant fields plus its file list.
#[allow(clippy::too_many_arguments)]
pub fn changeset_summary(
    label: String,
    current: bool,
    needs_restack: bool,
    loading: bool,
    failed: bool,
    failure_message: Option<String>,
    files: &[FileChange],
) -> ChangesetSummary {
    let (files, total_adds, total_dels) = file_rows(files);
    ChangesetSummary {
        label,
        current,
        needs_restack,
        loading,
        failed,
        failure_message,
        files,
        total_adds,
        total_dels,
    }
}

/// Renderable summary for a Dir-row outline selection: the aggregate diffstat for every file
/// under `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirSummary {
    pub path: String,
    pub files: Vec<SummaryFileRow>,
    pub total_adds: usize,
    pub total_dels: usize,
}

/// Segment-boundary match: `file_path` is "under" `dir_path` only when `dir_path` is a full path
/// SEGMENT prefix of `file_path` — `"src"` matches `"src/a.rs"` but must NOT match `"src2/b.rs"`
/// (a raw [`str::starts_with`] would wrongly match the latter).
fn path_is_under(file_path: &str, dir_path: &str) -> bool {
    file_path
        .strip_prefix(dir_path)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some()
}

/// Build a [`DirSummary`] for `path`, filtering `files` (already scoped by the caller to
/// whichever changeset(s) the selected [`crate::outline::OutlineItem::Dir`] row's `cs_idx`
/// covers — see `App::summary_for`) down to the ones under `path`.
pub fn dir_summary(path: String, files: &[FileChange]) -> DirSummary {
    let scoped: Vec<FileChange> = files
        .iter()
        .filter(|f| path_is_under(&f.path, &path))
        .cloned()
        .collect();
    let (files, total_adds, total_dels) = file_rows(&scoped);
    DirSummary {
        path,
        files,
        total_adds,
        total_dels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileStatus, Hunk, HunkLine};

    fn hunk_line(kind: LineKind) -> HunkLine {
        HunkLine {
            kind,
            content: b"x\n".to_vec(),
            old_lnum: None,
            new_lnum: None,
            missing_newline: false,
        }
    }

    fn file(path: &str, adds: usize, dels: usize, contexts: usize) -> FileChange {
        let mut lines = Vec::new();
        for _ in 0..adds {
            lines.push(hunk_line(LineKind::Addition));
        }
        for _ in 0..dels {
            lines.push(hunk_line(LineKind::Deletion));
        }
        for _ in 0..contexts {
            lines.push(hunk_line(LineKind::Context));
        }
        FileChange {
            path: path.to_string(),
            old_path: None,
            status: FileStatus::Modified,
            is_binary: false,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks: vec![Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                header: Vec::new(),
                lines,
            }],
        }
    }

    #[test]
    fn file_diffstat_counts_adds_and_dels_but_not_context() {
        let f = file("a.rs", 3, 2, 5);
        assert_eq!(file_diffstat(&f), (3, 2));
    }

    #[test]
    fn file_diffstat_is_zero_for_a_binary_file_with_no_hunks() {
        let f = FileChange {
            path: "bin.png".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            is_binary: true,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks: Vec::new(),
        };
        assert_eq!(file_diffstat(&f), (0, 0));
    }

    #[test]
    fn changeset_summary_totals_every_files_diffstat() {
        let files = vec![file("a.rs", 2, 1, 0), file("b.rs", 0, 3, 0)];
        let summary = changeset_summary(
            "My Title".to_string(),
            true,
            false,
            false,
            false,
            None,
            &files,
        );
        assert_eq!(summary.label, "My Title");
        assert!(summary.current);
        assert!(!summary.needs_restack);
        assert_eq!(summary.files.len(), 2);
        assert_eq!(summary.total_adds, 2);
        assert_eq!(summary.total_dels, 4);
    }

    #[test]
    fn changeset_summary_loading_carries_no_files() {
        let summary = changeset_summary(
            "Pending CS".to_string(),
            false,
            false,
            true,
            false,
            None,
            &[],
        );
        assert!(summary.loading);
        assert!(summary.files.is_empty());
        assert_eq!(summary.total_adds, 0);
    }

    #[test]
    fn changeset_summary_failed_carries_the_message() {
        let summary = changeset_summary(
            "Failed CS".to_string(),
            false,
            false,
            false,
            true,
            Some("boom".to_string()),
            &[],
        );
        assert!(summary.failed);
        assert_eq!(summary.failure_message.as_deref(), Some("boom"));
    }

    #[test]
    fn dir_summary_filters_by_segment_boundary_not_raw_prefix() {
        let files = vec![
            file("src/a.rs", 1, 0, 0),
            file("src/b.rs", 0, 1, 0),
            file("src2/b.rs", 5, 5, 0),
            file("top.rs", 1, 1, 0),
        ];
        let summary = dir_summary("src".to_string(), &files);
        let paths: Vec<&str> = summary.files.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["src/a.rs", "src/b.rs"],
            "must match src/* but NOT src2/* (segment-boundary, not raw string prefix)"
        );
        assert_eq!(summary.total_adds, 1);
        assert_eq!(summary.total_dels, 1);
    }

    #[test]
    fn dir_summary_matches_nested_paths_under_the_dir() {
        let files = vec![file("src/a/b.rs", 2, 0, 0), file("src/c.rs", 0, 2, 0)];
        let summary = dir_summary("src".to_string(), &files);
        assert_eq!(summary.files.len(), 2);
        assert_eq!(summary.total_adds, 2);
        assert_eq!(summary.total_dels, 2);
    }
}
