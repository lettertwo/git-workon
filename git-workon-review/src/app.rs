//! App state: the file list being reviewed, per-file view data (full text + alignment +
//! highlight cache + word-diff cache), and navigation/scroll state.
//!
//! Ported from the `review-tui-spike` prototype's `model.rs` — renamed here because `model`
//! already means the diff model in this crate (see the M3 plan's naming rule).
//!
//! Renders the **combined** (`HEAD` ↔ worktree) diff only (locked design decision #2 in the M3
//! plan) — the staged/unstaged split zoom is M4. [`App`] owns its own [`git2::Repository`]
//! handle so it can lazily read blob/worktree content per file as the user navigates to it,
//! independent of whatever handle acquired the [`DiffModel`] it was built from.

use std::collections::HashMap;
use std::path::Path;

use git2::Repository;

use crate::align::{align_file, collapse_gaps, CellKind, DisplayRow, Row};
use crate::highlight::{FgSpan, TsHighlighter};
use crate::model::{DiffModel, FileChange, FileStatus};
use crate::wordiff::{word_diff_spans, Span};

/// Loaded, aligned, highlighted view of one file's combined diff.
///
/// Full text is read once per side, from whichever source the file's status says still exists:
///
/// | status               | old-side source                    | new-side source            |
/// |-----------------------|-------------------------------------|-----------------------------|
/// | Added / Untracked     | none (empty)                        | worktree file on disk       |
/// | Deleted               | `HEAD` blob at `path`                | none (empty)                |
/// | Renamed / Copied      | `HEAD` blob at `old_path`             | worktree file at `path`     |
/// | Modified / Unmerged    | `HEAD` blob at `path`                | worktree file at `path`     |
///
/// The new side reads from the **worktree file on disk**, not the index blob — unstaged
/// content isn't in the object database; reading the staged (index) blob is an M4 concern (the
/// staged/unstaged split zoom).
pub struct FileView {
    old_text: String,
    new_text: String,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    /// The gap-collapsed row list the renderer walks. Word-diff spans and scroll coordinates
    /// are indexed against THIS vector, not the pre-collapse `AlignedRow` vector — collapsing
    /// only removes uninteresting context, so the underlying [`Row`]/[`CellKind`] pairing for
    /// any surviving row is unchanged.
    pub display: Vec<DisplayRow>,
    /// Index into [`Self::display`] of the first hunk's first row (or 0 for a file with no
    /// hunks), for the initial scroll jump.
    pub first_hunk_row: usize,
    pub old_hl: Option<Vec<Vec<FgSpan>>>,
    pub new_hl: Option<Vec<Vec<FgSpan>>>,
    /// Lazily computed word-diff spans, keyed by DISPLAY row index — the only coordinate the
    /// renderer's viewport walks once gaps are collapsed.
    word_spans: HashMap<usize, (Vec<Span>, Vec<Span>)>,
}

impl FileView {
    fn load(
        repo: &Repository,
        head_tree: &git2::Tree<'_>,
        file: &FileChange,
        ts: &mut TsHighlighter,
    ) -> Self {
        let old_source_path = file.old_path.as_deref().unwrap_or(file.path.as_str());
        let old_text = match file.status {
            FileStatus::Added | FileStatus::Untracked => String::new(),
            _ => read_head_blob(repo, head_tree, old_source_path),
        };

        let new_text = match file.status {
            FileStatus::Deleted => String::new(),
            _ => read_workdir_file(repo, &file.path),
        };

        let old_lines: Vec<String> = old_text.lines().map(str::to_string).collect();
        let new_lines: Vec<String> = new_text.lines().map(str::to_string).collect();

        let aligned = align_file(&file.hunks, old_lines.len(), new_lines.len());
        let display = collapse_gaps(&aligned.rows);
        let first_hunk_row = display
            .iter()
            .position(|row| {
                matches!(
                    row,
                    DisplayRow::Row(r) if !(r.old_kind == CellKind::Context && r.new_kind == CellKind::Context)
                )
            })
            .unwrap_or(0);

        let old_hl = ts.highlight_file(old_source_path, &old_text);
        let new_hl = ts.highlight_file(&file.path, &new_text);

        Self {
            old_text,
            new_text,
            old_lines,
            new_lines,
            display,
            first_hunk_row,
            old_hl,
            new_hl,
            word_spans: HashMap::new(),
        }
    }

    pub fn old_line(&self, n: usize) -> &str {
        self.old_lines
            .get(n.saturating_sub(1))
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn new_line(&self, n: usize) -> &str {
        self.new_lines
            .get(n.saturating_sub(1))
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn old_line_count(&self) -> usize {
        self.old_lines.len()
    }

    pub fn new_line_count(&self) -> usize {
        self.new_lines.len()
    }

    /// Full text loaded for the old/new side, for callers that need the whole blob rather than
    /// line-by-line access (e.g. re-running highlighting at a different width is NOT needed
    /// today, but tests assert against this directly).
    pub fn old_text(&self) -> &str {
        &self.old_text
    }

    pub fn new_text(&self) -> &str {
        &self.new_text
    }

    /// Lazily compute (and cache) word-diff spans for a paired display row. Returns empty spans
    /// (and does not populate the cache) for a row that isn't a `(Del, Add)` pair — callers
    /// check [`crate::align::AlignedRow::is_word_diff_pair`] first in the common case, but this
    /// stays total so it's safe to call unconditionally.
    pub fn word_spans_for_row(&mut self, display_idx: usize) -> (Vec<Span>, Vec<Span>) {
        if let Some(cached) = self.word_spans.get(&display_idx) {
            return cached.clone();
        }
        let pair = match self.display.get(display_idx) {
            Some(DisplayRow::Row(row)) if row.is_word_diff_pair() => Some((row.old, row.new)),
            _ => None,
        };
        match pair {
            Some((Row::Line(o), Row::Line(n))) => {
                let spans = word_diff_spans(self.old_line(o), self.new_line(n));
                self.word_spans.insert(display_idx, spans.clone());
                spans
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// Read-only peek at an already-cached word-diff span pair (empty if uncached). Used by the
    /// renderer's second (immutable) pass after a first mutable pass has populated the cache
    /// for the visible viewport via [`Self::word_spans_for_row`].
    pub fn peek_word_spans(&self, display_idx: usize) -> (Vec<Span>, Vec<Span>) {
        self.word_spans
            .get(&display_idx)
            .cloned()
            .unwrap_or_default()
    }
}

fn read_head_blob(repo: &Repository, tree: &git2::Tree<'_>, path: &str) -> String {
    tree.get_path(Path::new(path))
        .and_then(|entry| entry.to_object(repo))
        .ok()
        .and_then(|obj| obj.into_blob().ok())
        .map(|blob| String::from_utf8_lossy(blob.content()).into_owned())
        .unwrap_or_default()
}

fn read_workdir_file(repo: &Repository, path: &str) -> String {
    repo.workdir()
        .map(|wd| wd.join(path))
        .and_then(|p| std::fs::read(p).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Review session state: the combined diff's file list, per-file lazily loaded views, and
/// navigation/scroll state. One long-lived [`TsHighlighter`] lives here (not per file) — its
/// language-config cache is keyed per-instance, so a fresh highlighter per file would rebuild
/// every grammar config on every navigation.
pub struct App {
    repo: Repository,
    /// The combined diff's files. git2 enumerates these in path order (verified in
    /// `tests`), so "current file index" is a stable alphabetical position, not an
    /// arrival/discovery order that could reshuffle under the user.
    pub files: Vec<FileChange>,
    views: Vec<Option<FileView>>,
    pub current: usize,
    pub scroll: usize,
    pub pane_height: usize,
    /// Label for the old side of the diff, shown next to a rename's `old_path` in the header.
    /// M3 only reviews the combined (`HEAD` ↔ worktree) diff, so this is always `"HEAD"` today;
    /// M4's committed-changeset zoom will want to set this to the changeset's actual base rev.
    pub base_label: String,
    highlighter: TsHighlighter,
}

impl App {
    pub fn new(repo: Repository, combined: DiffModel) -> Self {
        let n = combined.files.len();
        Self {
            repo,
            files: combined.files,
            views: (0..n).map(|_| None).collect(),
            current: 0,
            scroll: 0,
            pane_height: 20,
            base_label: "HEAD".to_string(),
            highlighter: TsHighlighter::new(),
        }
    }

    /// Load (and cache) the [`FileView`] for `idx`, unless the file is binary — binary files
    /// skip content loading entirely (no blob read, no worktree read, no highlighting): there
    /// is nothing for the SBS renderer to align, so [`crate::render`] shows a placeholder
    /// without ever calling this.
    pub fn ensure_loaded(&mut self, idx: usize) {
        let Some(file) = self.files.get(idx) else {
            return;
        };
        if file.is_binary {
            return;
        }
        if self.views[idx].is_none() {
            // Re-peeled per call rather than cached on `App`: HEAD can move between file loads
            // (a fine risk in M3's read-only TUI) and the tree is cheap to re-peel.
            let Ok(head_tree) = self.repo.head().and_then(|h| h.peel_to_tree()) else {
                return;
            };
            let view = FileView::load(
                &self.repo,
                &head_tree,
                &self.files[idx],
                &mut self.highlighter,
            );
            self.views[idx] = Some(view);
        }
    }

    pub fn current_view(&mut self) -> Option<&mut FileView> {
        self.ensure_loaded(self.current);
        self.views.get_mut(self.current).and_then(|v| v.as_mut())
    }

    pub fn current_view_ref(&self) -> Option<&FileView> {
        self.views.get(self.current).and_then(|v| v.as_ref())
    }

    /// Jump the scroll position to the current file's first hunk (or the top, for a file with
    /// no hunks or that isn't loaded yet).
    pub fn jump_to_first_hunk(&mut self) {
        self.scroll = self
            .views
            .get(self.current)
            .and_then(|v| v.as_ref())
            .map(|v| v.first_hunk_row)
            .unwrap_or(0);
    }

    /// Load the current file (if not binary) and jump to its first hunk.
    pub fn open_current(&mut self) {
        self.ensure_loaded(self.current);
        self.jump_to_first_hunk();
    }

    pub fn next_file(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.files.len();
        self.open_current();
    }

    pub fn prev_file(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.current = (self.current + self.files.len() - 1) % self.files.len();
        self.open_current();
    }

    fn row_count(&self) -> usize {
        self.current_view_ref()
            .map(|v| v.display.len())
            .unwrap_or(0)
    }

    fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.pane_height.max(1))
    }

    /// Relative scroll, clamped to `[0, max_scroll()]` — except it never snaps backward past
    /// the current position when that position is itself beyond `max_scroll()` (e.g. right
    /// after [`Self::next_hunk_row`]/[`Self::prev_hunk_row`] jumped the hunk to the top of a
    /// pane taller than the remaining display). A relative scroll past a jump-placed position
    /// is a no-op in the over-scrolled direction rather than a backward leap; scrolling back the
    /// other way still works normally.
    pub fn scroll_by(&mut self, delta: i64) {
        let max = self.max_scroll() as i64;
        let cur = self.scroll as i64;
        self.scroll = (cur + delta).clamp(0, max.max(cur)) as usize;
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }

    /// Scroll to the next hunk-start row after the current scroll position (`]h`). A no-op if
    /// there is no later hunk, or the current file has no loaded view.
    pub fn next_hunk_row(&mut self) {
        let Some(view) = self.current_view_ref() else {
            return;
        };
        if let Some(row) = find_next_hunk_row(&view.display, self.scroll) {
            self.scroll = row;
        }
    }

    /// Scroll to the previous hunk-start row before the current scroll position (`[h`). A no-op
    /// if there is no earlier hunk, or the current file has no loaded view.
    pub fn prev_hunk_row(&mut self) {
        let Some(view) = self.current_view_ref() else {
            return;
        };
        if let Some(row) = find_prev_hunk_row(&view.display, self.scroll) {
            self.scroll = row;
        }
    }
}

/// True for a display row that carries change content (Del/Add/Filler on either side) rather
/// than pure context — the unit hunk navigation jumps between.
fn is_hunk_content_row(row: &DisplayRow) -> bool {
    matches!(
        row,
        DisplayRow::Row(r) if !(r.old_kind == CellKind::Context && r.new_kind == CellKind::Context)
    )
}

/// Row index of the next "hunk start" strictly after `after` — a hunk start is a content row
/// whose preceding row is context/gap/absent (i.e. a transition INTO a hunk, not every changed
/// row). Returns `None` if there is no such row.
fn find_next_hunk_row(display: &[DisplayRow], after: usize) -> Option<usize> {
    (after + 1..display.len()).find(|&i| {
        is_hunk_content_row(&display[i]) && (i == 0 || !is_hunk_content_row(&display[i - 1]))
    })
}

/// Row index of the previous "hunk start" strictly before `before`. See [`find_next_hunk_row`].
fn find_prev_hunk_row(display: &[DisplayRow], before: usize) -> Option<usize> {
    (0..before.min(display.len())).rev().find(|&i| {
        is_hunk_content_row(&display[i]) && (i == 0 || !is_hunk_content_row(&display[i - 1]))
    })
}

/// Test-only helper for building an [`App`] straight from a fixture, shared by `app.rs`'s own
/// tests and `render.rs`'s frame tests. `App` owns its `Repository` handle, but
/// [`git_workon_fixture::fixture::Fixture::repo`] only lends a borrowed one — so this opens a
/// second, independent handle on the same workdir.
#[cfg(test)]
pub(crate) mod test_support {
    use git2::Repository;
    use git_workon_fixture::fixture::Fixture;

    use super::App;
    use crate::acquire::diff_uncommitted;

    pub(crate) fn app_from_fixture(fixture: &Fixture) -> App {
        let repo = fixture.repo().expect("fixture repo");
        let combined = diff_uncommitted(repo).expect("diff_uncommitted").combined;
        let owned = Repository::open(repo.workdir().expect("fixture has a workdir"))
            .expect("reopen fixture repo");
        App::new(owned, combined)
    }
}

#[cfg(test)]
mod tests {
    use git_workon_fixture::prelude::*;

    use super::test_support::app_from_fixture;
    use super::{find_next_hunk_row, find_prev_hunk_row};
    use crate::align::{AlignedRow, CellKind, DisplayRow, Row};
    use crate::model::FileStatus;

    #[test]
    fn combined_files_arrive_path_sorted() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("z_new.txt", "hello\n")
            .unstaged_file("a_tracked.txt", "one\n", "one\nCHANGED\n")
            .untracked_file("m_mid.txt", "middle\n")
            .build()
            .unwrap();

        let app = app_from_fixture(&fixture);
        let paths: Vec<&str> = app.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a_tracked.txt", "m_mid.txt", "z_new.txt"]);
    }

    #[test]
    fn ensure_loaded_reads_head_and_worktree_sources_for_modified_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("tracked.txt", "line1\nline2\n", "line1\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.ensure_loaded(0);
        let view = app.current_view_ref().unwrap();
        assert_eq!(view.old_text(), "line1\nline2\n");
        assert_eq!(view.new_text(), "line1\nCHANGED\n");
    }

    #[test]
    fn ensure_loaded_leaves_added_file_old_side_empty() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("new.txt", "hello\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert_eq!(app.files[0].status, FileStatus::Added);
        app.ensure_loaded(0);
        let view = app.current_view_ref().unwrap();
        assert_eq!(view.old_text(), "");
        assert_eq!(view.new_text(), "hello\n");
    }

    #[test]
    fn ensure_loaded_leaves_deleted_file_new_side_empty() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .deleted_file("gone.txt", "bye\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert_eq!(app.files[0].status, FileStatus::Deleted);
        app.ensure_loaded(0);
        let view = app.current_view_ref().unwrap();
        assert_eq!(view.old_text(), "bye\n");
        assert_eq!(view.new_text(), "");
    }

    #[test]
    fn ensure_loaded_reads_old_path_for_renamed_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("old_name.txt", "same content\n", "same content\n")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap();
        std::fs::rename(workdir.join("old_name.txt"), workdir.join("new_name.txt")).unwrap();

        let mut app = app_from_fixture(&fixture);
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.files[0].status, FileStatus::Renamed);
        assert_eq!(app.files[0].old_path.as_deref(), Some("old_name.txt"));
        app.ensure_loaded(0);
        let view = app.current_view_ref().unwrap();
        assert_eq!(view.old_text(), "same content\n");
        assert_eq!(view.new_text(), "same content\n");
    }

    #[test]
    fn ensure_loaded_skips_binary_files() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("bin.dat", "hello\n")
            .build()
            .unwrap();
        // Overwrite the worktree copy with binary content post-build (the fixture staged plain
        // text) so the combined diff's content-sniffing sees NUL bytes and flags it binary.
        let repo = fixture.repo().unwrap();
        std::fs::write(repo.workdir().unwrap().join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();

        let mut app = app_from_fixture(&fixture);
        assert!(app.files[0].is_binary);
        app.ensure_loaded(0);
        assert!(app.current_view_ref().is_none());
    }

    // Hunk-nav helpers below operate purely over `DisplayRow` vectors — no fixture repo needed.

    fn ctx_row(n: usize) -> DisplayRow {
        DisplayRow::Row(AlignedRow {
            old: Row::Line(n),
            new: Row::Line(n),
            old_kind: CellKind::Context,
            new_kind: CellKind::Context,
        })
    }

    fn change_row(n: usize) -> DisplayRow {
        DisplayRow::Row(AlignedRow {
            old: Row::Line(n),
            new: Row::Line(n),
            old_kind: CellKind::Del,
            new_kind: CellKind::Add,
        })
    }

    fn gap_row(skipped: usize) -> DisplayRow {
        DisplayRow::Gap { skipped }
    }

    #[test]
    fn find_next_hunk_row_skips_within_a_hunk_and_stops_at_the_next_start() {
        // ctx, change, change (same hunk — not a new "start"), ctx, ctx, change (next hunk).
        let display = vec![
            ctx_row(1),
            change_row(2),
            change_row(3),
            ctx_row(4),
            ctx_row(5),
            change_row(6),
        ];
        assert_eq!(find_next_hunk_row(&display, 0), Some(1));
        // From inside the first hunk, the next START is the second hunk, not row 2 itself.
        assert_eq!(find_next_hunk_row(&display, 1), Some(5));
        assert_eq!(find_next_hunk_row(&display, 5), None);
    }

    #[test]
    fn find_prev_hunk_row_mirrors_next() {
        let display = vec![
            ctx_row(1),
            change_row(2),
            change_row(3),
            ctx_row(4),
            ctx_row(5),
            change_row(6),
        ];
        assert_eq!(find_prev_hunk_row(&display, 6), Some(5));
        assert_eq!(find_prev_hunk_row(&display, 5), Some(1));
        assert_eq!(find_prev_hunk_row(&display, 1), None);
    }

    #[test]
    fn hunk_row_helpers_treat_gap_rows_as_context() {
        let display = vec![change_row(1), gap_row(10), change_row(12)];
        assert_eq!(find_next_hunk_row(&display, 0), Some(2));
        assert_eq!(find_prev_hunk_row(&display, 2), Some(0));
    }

    #[test]
    fn app_next_and_prev_hunk_row_scroll_between_hunks() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "many.txt",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n",
                "1\nCHANGED\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\nCHANGED_TOO\n",
            )
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let first_hunk_row = app.scroll;

        app.next_hunk_row();
        assert!(
            app.scroll > first_hunk_row,
            "should scroll to the later hunk"
        );
        let second_hunk_row = app.scroll;

        // No hunk after the last one: no-op.
        app.next_hunk_row();
        assert_eq!(app.scroll, second_hunk_row);

        app.prev_hunk_row();
        assert_eq!(app.scroll, first_hunk_row);

        // No hunk before the first one: no-op.
        app.prev_hunk_row();
        assert_eq!(app.scroll, first_hunk_row);
    }

    #[test]
    fn scroll_by_does_not_snap_backward_past_a_hunk_jump() {
        // A small file (fits in the default pane height) with two hunks: `max_scroll() == 0`,
        // but `next_hunk_row` still jumps `scroll` to the second hunk's row unclamped, leaving
        // the view over-scrolled relative to `max_scroll()`.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "small.txt",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
                "1\nCHANGED\n3\n4\n5\n6\n7\n8\n9\nCHANGED_TOO\n",
            )
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(app.max_scroll(), 0, "whole file must fit in one pane");

        app.next_hunk_row();
        let over_scrolled = app.scroll;
        assert!(
            over_scrolled > 0,
            "hunk jump should place scroll past max_scroll"
        );

        // `j` (scroll_by(1)) at an over-scrolled position is a no-op, not a backward snap.
        app.scroll_by(1);
        assert_eq!(app.scroll, over_scrolled);

        // `k` (scroll_by(-1)) still scrolls up normally.
        app.scroll_by(-1);
        assert_eq!(app.scroll, over_scrolled - 1);
    }
}
