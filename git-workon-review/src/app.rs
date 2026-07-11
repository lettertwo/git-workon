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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use git2::Repository;
use workon::{Changeset, ChangesetSpan};

use crate::acquire::{ChangesetDiff, WorktreeDiffs};
use crate::align::{
    align_file, collapse_gaps_with_expansions, gap_hidden_range, inline_rows, AlignedRow, CellKind,
    DisplayRow, GapExpansion, InlineRow, Row,
};
use crate::apply::{Git2Applier, StageVerb};
use crate::config::RawViewConfig;
use crate::highlight::{lang_key_for_ext, FgSpan, TsHighlighter};
use crate::icons::IconMode;
use crate::model::{DiffModel, FileChange, FileStatus, Hunk, LineKind};
use crate::ops;
use crate::outline::{self, OutlineChangeset, OutlineFile, OutlineItem, OutlineMode, OutlineOrder};
use crate::queue::{OpOutcome, StagingOp, StagingQueue};
use crate::refresh::{IndexSignature, RefreshCoordinator};
use crate::scope::enclosing_scope_lines;
use crate::source::{resolve_source, Source};
use crate::stage_op::{FileStagingOp, LineSelectionOp};
use crate::summary;
use crate::synthesis::LineSelection;
use crate::wordiff::{word_diff_spans, Span};

/// Minimum rows kept between the cursor and the top/bottom of the pane while scrolling — see
/// [`App::derive_scroll`].
const SCROLLOFF: usize = 2;

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
#[derive(Debug)]
pub struct FileView {
    /// The pre-collapse row list [`Self::display`]/[`Self::inline`] derive from — retained (CS8)
    /// so a gap can be re-collapsed with a wider [`GapExpansion`] window without re-diffing the
    /// file. `AlignedRow` is small/`Copy`, so cloning the whole vector per expansion is cheap
    /// relative to re-running `align_file`.
    aligned: Vec<AlignedRow>,
    /// Per-gap expansion requests, keyed by the hidden run's start index in [`Self::aligned`]
    /// (the same key [`DisplayRow::Gap`]/[`InlineRow::Gap`] carry). Reset to empty on every
    /// [`Self::load`] — expansions are NOT preserved across a refresh; the view rebuilds from
    /// scratch and every gap re-collapses to its base window. See [`Self::expand_gap`].
    expansions: HashMap<usize, GapExpansion>,
    /// The file's hunks, retained (CS8) alongside [`Self::aligned`] so [`Self::rebuild_rows`] can
    /// recompute [`Self::display_hunk`]/[`Self::inline_hunk`] after an expansion without needing
    /// the original [`FileChange`] back.
    hunks: Vec<Hunk>,
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
    /// hunks), for the initial cursor jump under [`Layout::Sbs`].
    pub first_hunk_row: usize,
    /// Inline-layout analog of [`Self::first_hunk_row`]: index into [`Self::inline`] instead of
    /// `display`. A separate field (rather than translating one into the other) because the two
    /// row vectors track the same content in different shapes — same rationale as the two
    /// word-span caches below.
    pub first_inline_hunk_row: usize,
    pub old_hl: Option<Vec<Vec<FgSpan>>>,
    pub new_hl: Option<Vec<Vec<FgSpan>>>,
    /// Lazily computed word-diff spans, keyed by DISPLAY row index — the only coordinate the
    /// renderer's viewport walks once gaps are collapsed.
    word_spans: HashMap<usize, (Vec<Span>, Vec<Span>)>,
    /// The inline layout's row list, derived from [`Self::display`] via
    /// [`crate::align::inline_rows`] — see that function's doc comment for why this is a
    /// separate vector rather than a re-collapse over its own row type.
    pub inline: Vec<InlineRow>,
    /// Word-diff span cache for the inline layout, keyed by [`Self::inline`]'s row index — a
    /// SEPARATE coordinate space from [`Self::word_spans`] (a paired del/add block becomes two
    /// `InlineRow` entries at different indices instead of one `AlignedRow`), so the two caches
    /// cannot share keys.
    inline_word_spans: HashMap<usize, (Vec<Span>, Vec<Span>)>,
    /// Which hunk (index into the file's `hunks`) each [`Self::display`] row belongs to, or
    /// `None` for a row outside every hunk's span (a collapsed gap, or the leading/trailing
    /// context that lies beyond any `@@` block). Computed once at [`Self::load`] so staging ops
    /// can resolve "the hunk under the cursor" without re-walking the diff — see
    /// [`Self::hunk_at_display_row`]. A SEPARATE vector per coordinate space, exactly like the
    /// two word-span caches, since `display` and `inline` disagree on row count/index.
    display_hunk: Vec<Option<usize>>,
    /// Inline-coordinate analog of [`Self::display_hunk`], indexed against [`Self::inline`].
    inline_hunk: Vec<Option<usize>>,
}

impl FileView {
    /// `role` decides where each side's text comes from — the hunks (which rows are changes) are
    /// already role-correct because `file` is that role's own [`FileChange`], but the surrounding
    /// text must match the same two revisions the hunks were diffed against, or context lines
    /// render one revision on one side and a different one on the other:
    /// - old side: [`Role::Combined`]/[`Role::Staged`] read the `HEAD` blob; [`Role::Unstaged`]
    ///   reads the INDEX blob (unstaged is index ↔ worktree).
    /// - new side: [`Role::Combined`]/[`Role::Unstaged`] read the worktree file when `new_tree`
    ///   is `None` (the uncommitted layer); for a committed changeset `new_tree` is the changeset's
    ///   `head` commit tree, whose blob is read instead (its new side is `base..head`, not the
    ///   current worktree). [`Role::Staged`] reads the INDEX blob (staged is `HEAD` ↔ index).
    fn load(
        repo: &Repository,
        head_tree: &git2::Tree<'_>,
        new_tree: Option<&git2::Tree<'_>>,
        file: &FileChange,
        role: Role,
        ts: &mut TsHighlighter,
    ) -> Self {
        let old_source_path = file.old_path.as_deref().unwrap_or(file.path.as_str());
        let old_text = match file.status {
            FileStatus::Added | FileStatus::Untracked => String::new(),
            _ => match role {
                Role::Combined | Role::Staged => read_head_blob(repo, head_tree, old_source_path),
                Role::Unstaged => read_index_blob(repo, old_source_path),
            },
        };

        let new_text = match file.status {
            FileStatus::Deleted => String::new(),
            _ => match role {
                Role::Combined | Role::Unstaged => match new_tree {
                    Some(tree) => read_head_blob(repo, tree, &file.path),
                    None => read_workdir_file(repo, &file.path),
                },
                Role::Staged => read_index_blob(repo, &file.path),
            },
        };

        let old_lines: Vec<String> = old_text.lines().map(str::to_string).collect();
        let new_lines: Vec<String> = new_text.lines().map(str::to_string).collect();

        let aligned = align_file(&file.hunks, old_lines.len(), new_lines.len()).rows;
        let old_hl = ts.highlight_file(old_source_path, &old_text);
        let new_hl = ts.highlight_file(&file.path, &new_text);

        let mut view = Self {
            aligned,
            expansions: HashMap::new(),
            hunks: file.hunks.clone(),
            old_text,
            new_text,
            old_lines,
            new_lines,
            display: Vec::new(),
            first_hunk_row: 0,
            first_inline_hunk_row: 0,
            old_hl,
            new_hl,
            word_spans: HashMap::new(),
            inline: Vec::new(),
            inline_word_spans: HashMap::new(),
            display_hunk: Vec::new(),
            inline_hunk: Vec::new(),
        };
        view.rebuild_rows();
        view
    }

    /// Recompute [`Self::display`]/[`Self::inline`] (and everything derived from them) from
    /// [`Self::aligned`] + [`Self::expansions`] — called once at [`Self::load`] and again after
    /// every [`Self::expand_gap`]. Row-keyed word-span caches are cleared: an expansion changes
    /// which display/inline index a given content row lands at, so a cached span keyed by the OLD
    /// index would silently mismatch the row it renders under. The highlight caches
    /// ([`Self::old_hl`]/[`Self::new_hl`]) are source-line-indexed (one entry per line of the full
    /// old/new text), not row-indexed, so an expansion — which only changes how many already-hl'd
    /// lines are VISIBLE — never invalidates them.
    fn rebuild_rows(&mut self) {
        self.display = collapse_gaps_with_expansions(&self.aligned, &self.expansions);
        self.first_hunk_row = self
            .display
            .iter()
            .position(|row| {
                matches!(
                    row,
                    DisplayRow::Row(r) if !(r.old_kind == CellKind::Context && r.new_kind == CellKind::Context)
                )
            })
            .unwrap_or(0);

        self.inline = inline_rows(&self.display);
        self.first_inline_hunk_row = self
            .inline
            .iter()
            .position(is_inline_hunk_content_row)
            .unwrap_or(0);

        self.display_hunk = self
            .display
            .iter()
            .map(|row| {
                let (old, new) = display_row_linenos(row);
                hunk_for_linenos(&self.hunks, old, new)
            })
            .collect();
        self.inline_hunk = self
            .inline
            .iter()
            .map(|row| {
                let (old, new) = inline_row_linenos(row);
                hunk_for_linenos(&self.hunks, old, new)
            })
            .collect();

        self.word_spans.clear();
        self.inline_word_spans.clear();
    }

    /// Accumulate an expansion request for the gap keyed `key` (CS8's progressive reveal) and
    /// rebuild the derived rows. `more_before`/`more_after` ADD to whatever was already revealed
    /// at that edge (repeated `Enter` presses widen further); `full` is sticky — once set for this
    /// gap it stays set. A `key` with no matching gap in the current `display` is harmless: the
    /// entry simply sits unused in the map until a gap with that key exists again (it never will,
    /// since keys are stable pre-collapse indices — this is just defensive, not reachable from
    /// [`App::expand_gap_at_cursor`], which validates the cursor row first).
    pub fn expand_gap(&mut self, key: usize, more_before: usize, more_after: usize, full: bool) {
        let entry = self.expansions.entry(key).or_default();
        entry.before += more_before;
        entry.after += more_after;
        entry.full |= full;
        self.rebuild_rows();
    }

    /// CS9's scope-reveal: widen the gap keyed `key` to uncover a tree-sitter scope range
    /// `[scope_start, scope_end]` (1-based, inclusive — as returned by
    /// [`crate::scope::enclosing_scope_lines`]) that encloses the gap's anchor line, in
    /// `anchor_prefers_new`'s frame (new-side lineno when `true`, old-side when `false` — see
    /// [`App::expand_gap_at_cursor`]'s anchor selection). Only the gap's TRAILING edge (`after`)
    /// is ever widened: the anchor sits at the gap's following edge and `scope_start` is what
    /// climbs upward from it toward the gap; `scope_end` falls among rows already visible after
    /// the gap by construction (the anchor line is inside the scope), so the leading edge never
    /// has anything new to reveal here.
    ///
    /// Returns `true` when this widened the gap (grew `after`, or revealed the whole run because
    /// the scope covers it entirely); `false` when the scope added nothing new — either the gap
    /// is already fully revealed/not a gap at all, or `scope_start` doesn't reach far enough
    /// upward to uncover any currently-hidden row. The caller's signal to fall back to the flat
    /// +10 reveal, so repeated presses always widen.
    pub fn scope_expand_gap(
        &mut self,
        key: usize,
        scope_start: usize,
        anchor_prefers_new: bool,
    ) -> bool {
        let Some((hidden_start, hidden_end)) =
            gap_hidden_range(&self.aligned, key, &self.expansions)
        else {
            return false;
        };
        // Non-empty by construction: `gap_hidden_range` returns `None` (never an empty range)
        // once an expansion covers the whole run — see its `effective_before + effective_after
        // >= run_len` arm.
        let hidden = &self.aligned[hidden_start..hidden_end];

        let lineno_of =
            |row: &AlignedRow| row_lineno(if anchor_prefers_new { row.new } else { row.old });
        // Context rows always carry a lineno on both sides (see the module doc's lineno
        // invariant), and linenos increase monotonically through a run, so counting from the
        // trailing edge backward while the scope still covers each row is safe.
        let count = hidden
            .iter()
            .rev()
            .take_while(|row| lineno_of(row).is_some_and(|n| n >= scope_start))
            .count();

        if count == 0 {
            return false;
        }
        if count >= hidden.len() {
            self.expand_gap(key, 0, 0, true);
        } else {
            self.expand_gap(key, 0, count, false);
        }
        true
    }

    /// The hunk (index into the file's `hunks`) whose span covers display row `row`, or `None`
    /// for a row outside every hunk (a gap, or context beyond any `@@` block). A context line git
    /// kept inside a hunk's header counts as "in" that hunk (matching the prototype's `hunk_at`).
    pub(crate) fn hunk_at_display_row(&self, row: usize) -> Option<usize> {
        self.display_hunk.get(row).copied().flatten()
    }

    /// Inline-coordinate analog of [`Self::hunk_at_display_row`].
    pub(crate) fn hunk_at_inline_row(&self, row: usize) -> Option<usize> {
        self.inline_hunk.get(row).copied().flatten()
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

    /// Inline-layout analog of [`Self::word_spans_for_row`], keyed by [`Self::inline`]'s row
    /// index instead of [`Self::display`]'s. A `Del`/`Add` row with no paired counterpart (an
    /// unpaired excess line) returns empty spans without populating the cache, same as the SBS
    /// version.
    pub fn inline_word_spans_for_row(&mut self, inline_idx: usize) -> (Vec<Span>, Vec<Span>) {
        if let Some(cached) = self.inline_word_spans.get(&inline_idx) {
            return cached.clone();
        }
        let pair = match self.inline.get(inline_idx) {
            Some(InlineRow::Del {
                old,
                paired_new: Some(new),
            }) => Some((*old, *new)),
            Some(InlineRow::Add {
                new,
                paired_old: Some(old),
            }) => Some((*old, *new)),
            _ => None,
        };
        match pair {
            Some((old, new)) => {
                let spans = word_diff_spans(self.old_line(old), self.new_line(new));
                self.inline_word_spans.insert(inline_idx, spans.clone());
                spans
            }
            None => (Vec::new(), Vec::new()),
        }
    }

    /// Read-only peek at an already-cached inline word-diff span pair (empty if uncached). See
    /// [`Self::peek_word_spans`].
    pub fn peek_inline_word_spans(&self, inline_idx: usize) -> (Vec<Span>, Vec<Span>) {
        self.inline_word_spans
            .get(&inline_idx)
            .cloned()
            .unwrap_or_default()
    }
}

/// The tree a COMBINED-role [`FileView`]'s old side reads from (see [`FileView::load`]'s role
/// table): the changeset's `base` commit for a committed changeset, or the live `HEAD` for the
/// uncommitted layer — the only case M2–M4 ever had, and what [`App::base_label`] already
/// names. A committed changeset's combined role is `base..head` (there is no staged/unstaged
/// split to disagree with it — see [`DiffState::from_committed`]), so the old side must read
/// `base`'s blob, not whatever `HEAD` happens to be right now.
///
/// A free function (not an `App` method) so its returned [`git2::Tree`] borrows only `repo`,
/// not all of `App` — a `&self` method here would make the borrow checker treat the tree as
/// blocking every OTHER field access (e.g. `&mut self.highlighter`) for its whole lifetime, even
/// though the two never actually conflict.
fn old_side_tree_for(repo: &Repository, span: ChangesetSpan) -> Option<git2::Tree<'_>> {
    match span {
        ChangesetSpan::Committed { base, .. } => repo.find_commit(base).and_then(|c| c.tree()).ok(),
        // Root commit reviewed on its own: the old side is the empty tree. `treebuilder(None)`
        // builds (and `write` persists, idempotently — git's well-known empty-tree object) an
        // empty tree without needing a real parent commit to peel.
        ChangesetSpan::CommittedRoot { .. } => repo
            .treebuilder(None)
            .and_then(|b| b.write())
            .and_then(|oid| repo.find_tree(oid))
            .ok(),
        ChangesetSpan::Uncommitted => repo.head().and_then(|h| h.peel_to_tree()).ok(),
    }
}

/// The tree a COMBINED-role [`FileView`]'s NEW side reads from (see [`FileView::load`]'s role
/// table): the changeset's `head` commit for a committed changeset, or `None` for the uncommitted
/// layer — where `None` means "read the worktree", the only new-side source M2–M4 ever had. A
/// committed changeset's combined role is `base..head`, so its new side must read `head`'s blob,
/// not the current worktree (which for an OLDER committed changeset differs from `head` and would
/// break the align invariant against the `base..head` hunks). The mirror of [`old_side_tree_for`].
///
/// A free function for the same borrow-checker reason as [`old_side_tree_for`]: the returned
/// [`git2::Tree`] borrows only `repo`, leaving `&mut self.highlighter` free at the call site.
fn new_side_tree_for(repo: &Repository, span: ChangesetSpan) -> Option<git2::Tree<'_>> {
    match span {
        ChangesetSpan::Committed { head, .. } | ChangesetSpan::CommittedRoot { head } => {
            repo.find_commit(head).and_then(|c| c.tree()).ok()
        }
        ChangesetSpan::Uncommitted => None,
    }
}

/// Build a [`Role::Combined`] [`FileView`] against `repo`/`ts` for a file whose combined
/// [`FileChange`] is `file` and whose changeset span is `span` — the shared core
/// [`App::ensure_role_loaded`]'s combined branch and ADR-037's [`build_file_views`] (the loader
/// job's pure body) both call, so a deferred-then-loader-completed open is byte-identical to an
/// eager one. `None` for a binary file or an unreadable tree; never panics.
fn build_combined_view(
    repo: &Repository,
    ts: &mut TsHighlighter,
    span: ChangesetSpan,
    file: &FileChange,
) -> Option<FileView> {
    if file.is_binary {
        return None;
    }
    // Re-peeled per call rather than cached: for the uncommitted layer `HEAD` can move between
    // file loads, and the tree is cheap to re-peel either way (see `old_side_tree_for`'s doc
    // comment).
    let head_tree = old_side_tree_for(repo, span)?;
    let new_tree = new_side_tree_for(repo, span);
    Some(FileView::load(
        repo,
        &head_tree,
        new_tree.as_ref(),
        file,
        Role::Combined,
        ts,
    ))
}

/// Build a non-Combined ([`Role::Unstaged`]/[`Role::Staged`]) [`FileView`] against `repo`/`ts` for
/// sub-role file `file` — the mirror of [`build_combined_view`], shared the same way. Non-Combined
/// roles are uncommitted-only (a committed changeset's staged/unstaged sub-models are always
/// empty — see [`DiffState::from_committed`]), so the new side always stays worktree/index (`None`
/// to [`FileView::load`]) and the old side is always live `HEAD`, never a changeset's `base`.
/// `None` for a binary file or an unreadable `HEAD`; never panics.
fn build_sub_role_view(
    repo: &Repository,
    ts: &mut TsHighlighter,
    role: Role,
    file: &FileChange,
) -> Option<FileView> {
    debug_assert_ne!(
        role,
        Role::Combined,
        "build_sub_role_view is non-Combined only"
    );
    if file.is_binary {
        return None;
    }
    let head_tree = repo.head().and_then(|h| h.peel_to_tree()).ok()?;
    Some(FileView::load(repo, &head_tree, None, file, role, ts))
}

fn read_head_blob(repo: &Repository, tree: &git2::Tree<'_>, path: &str) -> String {
    tree.get_path(Path::new(path))
        .and_then(|entry| entry.to_object(repo))
        .ok()
        .and_then(|obj| obj.into_blob().ok())
        .map(|blob| String::from_utf8_lossy(blob.content()).into_owned())
        .unwrap_or_default()
}

/// Read the INDEX (staging area) copy of `path` as text — the "old" side of an unstaged
/// (index ↔ worktree) view and the "new" side of a staged (`HEAD` ↔ index) view. Reads stage-0
/// (the ordinary, non-conflict entry); a path absent from the index (or with no stage-0 entry)
/// reads as empty, same graceful-default posture as [`read_head_blob`].
fn read_index_blob(repo: &Repository, path: &str) -> String {
    repo.index()
        .ok()
        .and_then(|index| index.get_path(Path::new(path), 0))
        .and_then(|entry| repo.find_blob(entry.id).ok())
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

/// Default outline pane width (locked design: "~35 cols") — the CS7
/// (`workon.review.outline.width`) fallback when the setting is unset, out of range, or the
/// config read fails. Was a `render.rs`-local const before CS7; now App-owned state since it's
/// configurable per session (see [`OutlineState::width`]).
pub const DEFAULT_OUTLINE_WIDTH: u16 = 35;
/// Sane clamp bounds for `workon.review.outline.width` (CS7). Below `MIN_OUTLINE_WIDTH` the
/// pane can't show a useful path fragment; above `MAX_OUTLINE_WIDTH` it would swallow the diff
/// pane on any reasonable terminal. Also addresses M5's deferred narrow-terminal papercut: a
/// user on a narrow terminal can now set a smaller width instead of losing the diff pane
/// entirely to a fixed 35-col outline.
pub const MIN_OUTLINE_WIDTH: u16 = 10;
pub const MAX_OUTLINE_WIDTH: u16 = 200;

/// Which layout the renderer draws the current file's rows in — runtime-toggled via `L`
/// (prototype analog: `<leader>rl`), and persists across file navigation (neither
/// [`App::next_file`]/[`App::prev_file`] nor [`App::open_current`] touch it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Sbs,
    Inline,
}

/// Which of the three per-file diff roles a [`FileView`] is built from. The **combined** role is
/// `HEAD` ↔ worktree (the whole change); **unstaged** is index ↔ worktree; **staged** is `HEAD` ↔
/// index. A file need not have a change in every role — an untracked file has only an unstaged
/// change; a freshly `git add`ed one only a staged change; a partially-staged file has all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Combined,
    Unstaged,
    Staged,
}

/// The zoom the user *requested* via `z` — persists across file navigation (like [`Layout`]). The
/// actual state rendered per file is [`EffectiveZoom`], resolved by [`effective_zoom`] from this
/// plus the file's available sub-diffs; a file lacking the requested role collapses to
/// [`Role::Combined`] rather than showing an empty pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    /// Unstaged pane stacked above staged pane, each independently navigable. The default —
    /// the gate downgrades it to a single pane for files that don't have both sub-diffs, so the
    /// common all-unstaged worktree still renders as one pane.
    #[default]
    Split,
    Combined,
    Unstaged,
    Staged,
}

/// The zoom actually rendered for a given file this frame — the gated resolution of a [`Zoom`]
/// against that file's available sub-diffs (see [`effective_zoom`]). Either a single pane over one
/// [`Role`], or the two-pane [`EffectiveZoom::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveZoom {
    Single(Role),
    Split,
}

/// Resolve the requested [`Zoom`] to the [`EffectiveZoom`] a file can actually show, given which of
/// its sub-diffs exist (`has_unstaged`/`has_staged` = the file's path appears in that role's
/// `DiffModel`) and whether it's stageable at all (`can_stage` = non-binary in M4).
///
/// Rules (a pure gate, unit-tested against the full truth table):
/// - not stageable → [`Role::Combined`] (binary files render the placeholder; no attribution);
/// - `Combined` → `Combined`;
/// - `Unstaged` → `Unstaged` if it has one, else `Combined`;
/// - `Staged` → `Staged` if it has one, else `Combined`;
/// - `Split` → `Split` only if it has BOTH sub-diffs; else downgrade to whichever single sub-diff
///   exists; else `Combined`.
pub fn effective_zoom(
    requested: Zoom,
    has_unstaged: bool,
    has_staged: bool,
    can_stage: bool,
) -> EffectiveZoom {
    if !can_stage {
        return EffectiveZoom::Single(Role::Combined);
    }
    match requested {
        Zoom::Combined => EffectiveZoom::Single(Role::Combined),
        Zoom::Unstaged => {
            if has_unstaged {
                EffectiveZoom::Single(Role::Unstaged)
            } else {
                EffectiveZoom::Single(Role::Combined)
            }
        }
        Zoom::Staged => {
            if has_staged {
                EffectiveZoom::Single(Role::Staged)
            } else {
                EffectiveZoom::Single(Role::Combined)
            }
        }
        Zoom::Split => {
            if has_unstaged && has_staged {
                EffectiveZoom::Split
            } else if has_unstaged {
                EffectiveZoom::Single(Role::Unstaged)
            } else if has_staged {
                EffectiveZoom::Single(Role::Staged)
            } else {
                EffectiveZoom::Single(Role::Combined)
            }
        }
    }
}

/// Parse `workon.review.outline.mode` (CS7) into an [`OutlineMode`]. Canonical strings mirror
/// the variant names, kebab-cased: `flat`, `stack`, `tree`, `stack-tree`. `None` on anything
/// else — [`App::apply_view_config`] falls back to [`OutlineMode::default`] and warns.
fn parse_outline_mode(raw: &str) -> Option<OutlineMode> {
    match raw {
        "flat" => Some(OutlineMode::Flat),
        "stack" => Some(OutlineMode::Stack),
        "tree" => Some(OutlineMode::Tree),
        "stack-tree" => Some(OutlineMode::StackTree),
        _ => None,
    }
}

/// Parse `workon.review.outline.order` (CS3) into an [`OutlineOrder`]. Canonical strings mirror
/// the variant names, kebab-cased: `head-first`, `base-first`. `None` on anything else —
/// [`App::apply_view_config`] falls back to [`OutlineOrder::default`] and warns.
fn parse_outline_order(raw: &str) -> Option<OutlineOrder> {
    match raw {
        "head-first" => Some(OutlineOrder::HeadFirst),
        "base-first" => Some(OutlineOrder::BaseFirst),
        _ => None,
    }
}

/// Parse `workon.review.icons` (CS5) into an [`IconMode`]. Canonical strings mirror
/// the variant names, kebab-cased: `nerd`, `none`. `None` on anything else —
/// [`App::apply_view_config`] falls back to [`IconMode::default`] (also `none` — CS5's
/// no-auto-detection default) and warns.
fn parse_icon_mode(raw: &str) -> Option<IconMode> {
    match raw {
        "nerd" => Some(IconMode::Nerd),
        "none" => Some(IconMode::None),
        _ => None,
    }
}

/// Parse `workon.review.diff.layout` (CS7) into a [`Layout`]. Canonical strings mirror the
/// variant names: `sbs`, `inline`. `None` on anything else — [`App::apply_view_config`] falls
/// back to [`Layout::default`] and warns.
fn parse_diff_layout(raw: &str) -> Option<Layout> {
    match raw {
        "sbs" => Some(Layout::Sbs),
        "inline" => Some(Layout::Inline),
        _ => None,
    }
}

/// Parse `workon.review.diff.zoom` (CS7) into a [`Zoom`]. Canonical strings mirror the variant
/// names: `split`, `combined`, `unstaged`, `staged`. `None` on anything else —
/// [`App::apply_view_config`] falls back to [`Zoom::default`] and warns.
fn parse_diff_zoom(raw: &str) -> Option<Zoom> {
    match raw {
        "split" => Some(Zoom::Split),
        "combined" => Some(Zoom::Combined),
        "unstaged" => Some(Zoom::Unstaged),
        "staged" => Some(Zoom::Staged),
        _ => None,
    }
}

/// CS4: which outline row a Header/Dir cursor selection resolves to — [`App::summary_target`]'s
/// return type, and the input [`App::summary_for`] consumes to build the renderable summary.
/// `render.rs`'s `render_summary` never matches on this directly — it only calls
/// `App::summary_for`/renders the [`Summary`] that comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryTarget {
    /// The cursor rests on an [`OutlineItem::Header`] row — `cs_idx` is that row's true index
    /// into [`App::changesets`].
    Changeset(usize),
    /// The cursor rests on an [`OutlineItem::Dir`] row — `path` is that row's full path, `cs_idx`
    /// its `cs_idx` (`Some` in [`OutlineMode::StackTree`], `None` in the cross-stack
    /// [`OutlineMode::Tree`] — see that field's doc comment on [`OutlineItem::Dir`]).
    Dir { cs_idx: Option<usize>, path: String },
}

/// CS4: the renderable summary [`App::summary_for`] builds for a [`SummaryTarget`] — a thin
/// wrapper so `render.rs` has one return type to match on regardless of which kind of row was
/// selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Summary {
    Changeset(summary::ChangesetSummary),
    Dir(summary::DirSummary),
}

/// CS7: a stable identity for an outline File/Dir row, captured BEFORE a staging/discard op's
/// `coordinated_refresh` rebuilds [`App::outline_items`]'s row list, so the row can be re-found
/// (or gracefully lost, e.g. a fully-discarded file) afterward — see
/// [`App::restore_outline_position`]. `cs_idx`/`path` mirror the row's own fields, EXCEPT a
/// [`OutlineItem::File`]'s `path` here is always the FULL path (from the underlying
/// [`FileChange`]), never the Tree/StackTree leaf-only segment the row itself may display — two
/// rows in different directories can share a leaf name, so the leaf alone isn't a stable key.
/// [`OutlineItem::Dir`]'s own `path` field is already full regardless of mode, so it's reused
/// as-is. No [`OutlineItem::Header`] variant: a header row is never a staging/discard target (see
/// [`App::outline_row_targets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineRowIdentity {
    File { cs_idx: usize, path: String },
    Dir { cs_idx: Option<usize>, path: String },
}

impl OutlineRowIdentity {
    /// Whether outline row `item` is the same row this identity was captured from. A
    /// [`OutlineItem::File`]'s displayed `path` may be leaf-only (Tree/StackTree) — that's
    /// resolved through [`App::outline_row_targets`]'s `(cs_idx, file_idx)` lookup instead of
    /// comparing against the row's own `path` field.
    fn matches_file(&self, item_cs_idx: usize, full_path: &str) -> bool {
        matches!(
            self,
            OutlineRowIdentity::File { cs_idx, path }
                if *cs_idx == item_cs_idx && path == full_path
        )
    }

    /// Whether outline row `item` is the same row this identity was captured from.
    fn matches_dir(&self, item: &OutlineItem) -> bool {
        match (self, item) {
            (
                OutlineRowIdentity::Dir { cs_idx, path },
                OutlineItem::Dir {
                    cs_idx: item_cs_idx,
                    path: item_path,
                    ..
                },
            ) => cs_idx == item_cs_idx && path == item_path,
            _ => false,
        }
    }
}

/// The outline side pane's own state (locked fork 3): whether it's showing, whether IT (rather
/// than the diff) currently has keyboard focus, its own cursor (an index into
/// [`App::outline_items`]'s row list — a wholly separate coordinate space from [`App::cursor`]),
/// and which [`OutlineMode`] it's rendering. Lives directly on [`App`] (unlike the per-changeset
/// diff state) since it's small and there's only ever one outline for the whole review session.
#[derive(Debug, Clone)]
pub struct OutlineState {
    pub open: bool,
    pub focused: bool,
    pub cursor: usize,
    pub mode: OutlineMode,
    /// The outline pane's column width — `workon.review.outline.width` (CS7), defaulting to
    /// [`DEFAULT_OUTLINE_WIDTH`]. Read by `render.rs` in place of the old fixed const.
    pub width: u16,
    /// Top-of-viewport row index into [`App::outline_items`]'s row list, derived from `cursor`
    /// via the same scrolloff discipline as [`App::scroll`] (see [`App::derive_outline_scroll`]) —
    /// never written directly.
    pub scroll: usize,
    /// Which end of the stack the stack-shaped modes display first — `workon.review.outline.order`
    /// (CS3), defaulting to [`OutlineOrder::HeadFirst`]. Read by [`App::outline_items`].
    pub order: OutlineOrder,
}

/// Which of a split's two panes has focus — the top pane renders the unstaged role, the bottom the
/// staged role. Focus decides which pane owns [`App::cursor`]/[`App::scroll`] and where the cursor
/// highlight draws; `w` toggles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitPane {
    Unstaged,
    Staged,
}

/// Cursor + derived scroll for a split's *unfocused* pane. The focused pane's equivalent state
/// lives directly on [`App`] (`cursor`/`scroll`) so every existing cursor-moving method keeps
/// operating on the focused pane unchanged; `w` swaps this in and out (see
/// [`App::toggle_split_focus`]).
#[derive(Debug, Clone, Copy, Default)]
struct PaneState {
    cursor: usize,
    scroll: usize,
}

/// CS6: a staging op's pre-op position, captured by [`App::capture_position`] before
/// `coordinated_refresh` and restored by [`App::restore_position`] after — so a staging op keeps
/// the reviewer's place instead of `reset_panes`' first-hunk reseat (that reseat still runs for
/// every MANUAL nav: file/changeset switches, zoom cycles). `path` + `role` say WHERE (the same
/// file, the pane the reviewer was in); `old_lineno`/`new_lineno` say WHAT (the acted-on row's
/// position in `role`'s own coordinate frame — the two sides a role's rows are diffed against,
/// per [`FileView::load`]'s table). Deliberately NO pre-op zoom snapshot: [`App::restore_position`]
/// re-derives the POST-op [`EffectiveZoom`] from live state, since the op itself is exactly what
/// invalidates a pre-op snapshot.
struct PositionMemento {
    path: String,
    role: Role,
    old_lineno: Option<usize>,
    new_lineno: Option<usize>,
}

/// The target role's display row (active layout) whose lineno IN `new_frame`'s coordinate frame
/// (`true` = new side, `false` = old side — the frame the memento's target lineno was captured
/// in) is the first `>= target`. Rows with no lineno on that side — gaps, and the unpaired
/// Del/Add rows whose only lineno lives on the OTHER side — are skipped rather than compared:
/// old-side and new-side numbering diverge as soon as a file has any insertion or deletion above
/// the row, so mixing frames in one monotonic scan would let e.g. a deletion hunk's old-side
/// numbers (which run ahead of the surrounding new-side numbers) capture the cursor first.
///
/// Falls back to the LAST row carrying a lineno in that frame when `target` is past the view's
/// end (staging the acted-on hunk can shrink the file out from under the old lineno). `None`
/// only when NO row carries a lineno in that frame (e.g. anchoring old-frame in an added-only
/// file) — the caller keeps `reset_panes`' first-hunk position then.
fn find_nearest_row(
    view: &FileView,
    layout: Layout,
    target: usize,
    new_frame: bool,
) -> Option<usize> {
    let in_frame = |old: Option<usize>, new: Option<usize>| if new_frame { new } else { old };
    let linenos: Vec<(usize, usize)> = match layout {
        Layout::Sbs => view
            .display
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let (old, new) = display_row_linenos(row);
                in_frame(old, new).map(|n| (i, n))
            })
            .collect(),
        Layout::Inline => view
            .inline
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let (old, new) = inline_row_linenos(row);
                in_frame(old, new).map(|n| (i, n))
            })
            .collect(),
    };
    linenos
        .iter()
        .find(|(_, n)| *n >= target)
        .or_else(|| linenos.last())
        .map(|(i, _)| *i)
}

/// Slide `prev_scroll` the minimum amount to keep `cursor` within `[SCROLLOFF, pane_height - 1 -
/// SCROLLOFF]` of the viewport, then clamp to `[0, rows - pane_height]` (edge wins over margin).
/// The pure core of [`App::derive_scroll`], factored out so a split's unfocused pane can derive its
/// own scroll against ITS height without going through `App`'s focused-pane fields.
fn derive_scroll_value(
    cursor: usize,
    prev_scroll: usize,
    rows: usize,
    pane_height: usize,
) -> usize {
    if rows == 0 {
        return 0;
    }
    let pane_height = pane_height.max(1);
    let cursor = cursor.min(rows - 1);
    let bottom_margin = pane_height.saturating_sub(1).saturating_sub(SCROLLOFF);

    let mut scroll = prev_scroll;
    if cursor < scroll + SCROLLOFF {
        scroll = cursor.saturating_sub(SCROLLOFF);
    } else if cursor > scroll + bottom_margin {
        scroll = cursor.saturating_sub(bottom_margin);
    }
    let max_scroll = rows.saturating_sub(pane_height);
    scroll.min(max_scroll)
}

/// One changeset's diff state: its [`workon::Changeset`] descriptor (name, source, restack
/// status), the [`DiffState`] acquired for it, and its own per-file, per-role lazily built
/// [`FileView`] caches — the same three `views_*` vectors [`App`] held directly through M4,
/// now scoped per changeset since M5 reviews more than one at a time.
///
/// A committed changeset's [`Self::diff`] has empty staged/unstaged sub-models (see
/// [`DiffState::from_committed`]), which is enough on its own to render it read-only: the
/// existing [`effective_zoom`] gate collapses `Split`/`Unstaged`/`Staged` to
/// [`EffectiveZoom::Single(Role::Combined)`] whenever both sub-diffs are absent — no
/// committed-specific rendering code needed for M5's spine (the mode-aware staging refusal and
/// zoom lock riding this natural collapse are [`App::is_committed`]'s targeted guards).
pub struct ChangesetView {
    pub cs: Changeset,
    diff: DiffState,
    /// Per-file, per-role lazily built views (parallel to [`DiffState::files`]). A slot stays
    /// `None` until first access; a role slot ALSO stays `None` forever when that file has no
    /// change in that role (see [`App::ensure_role_loaded`]).
    views_combined: Vec<Option<FileView>>,
    views_unstaged: Vec<Option<FileView>>,
    views_staged: Vec<Option<FileView>>,
    /// ADR-037's per-changeset acquisition state. `Ready` for every changeset this changeset
    /// (this revision of the codebase) actually diffs through; `Pending`/`Failed` slots are
    /// constructible today (state model + rendering) but nothing in the synchronous startup/
    /// refresh paths produces them yet — that lands with the streamed-acquisition changesets.
    slot: ChangesetSlot,
}

/// A [`ChangesetView`]'s acquisition state (ADR-037's "Slots" decision). `diff`/the `views_*`
/// caches stay meaningful only for `Ready` — a `Pending`/`Failed` view's [`DiffState`] is always
/// [`DiffState::empty`], so every existing `.diff.`-reading call site (file counts, outline
/// rows, nav guards) already treats it as "nothing to show" with no per-site branch needed; only
/// the render/outline paths that must actively DISTINGUISH the three states (vs. a genuinely
/// empty `Ready` changeset) read this directly.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChangesetSlot {
    /// Acquisition hasn't run (or hasn't completed) for this changeset yet.
    Pending,
    /// The diff (and its view caches) are real.
    Ready,
    /// The acquisition attempt errored; the message is shown in place of a diff body.
    Failed(String),
}

impl ChangesetView {
    fn new(cs: Changeset, diff: DiffState) -> Self {
        let n = diff.files.len();
        Self {
            cs,
            diff,
            views_combined: (0..n).map(|_| None).collect(),
            views_unstaged: (0..n).map(|_| None).collect(),
            views_staged: (0..n).map(|_| None).collect(),
            slot: ChangesetSlot::Ready,
        }
    }

    /// Construct a `Pending` slot for `cs` (ADR-037): no diff acquired yet. The outline shows
    /// its header with a loading indication; navigating onto it renders a changeset-level
    /// placeholder instead of "(no changes)"/per-file content.
    pub fn pending(cs: Changeset) -> Self {
        let mut view = Self::new(cs, DiffState::empty());
        view.slot = ChangesetSlot::Pending;
        view
    }

    /// Construct a `Failed` slot for `cs` carrying `message` (ADR-037): the acquisition attempt
    /// for this changeset errored. The outline marks it; navigating onto it renders `message`
    /// instead of a diff body.
    pub fn failed(cs: Changeset, message: impl Into<String>) -> Self {
        let mut view = Self::new(cs, DiffState::empty());
        view.slot = ChangesetSlot::Failed(message.into());
        view
    }

    /// Whether this changeset's diff hasn't been acquired yet (ADR-037).
    pub fn is_pending(&self) -> bool {
        matches!(self.slot, ChangesetSlot::Pending)
    }

    /// Whether this changeset's acquisition attempt errored (ADR-037).
    pub fn is_failed(&self) -> bool {
        matches!(self.slot, ChangesetSlot::Failed(_))
    }

    /// This changeset's failure message, if [`Self::is_failed`] — `None` for `Pending`/`Ready`.
    pub fn failure_message(&self) -> Option<&str> {
        match &self.slot {
            ChangesetSlot::Failed(msg) => Some(msg.as_str()),
            ChangesetSlot::Pending | ChangesetSlot::Ready => None,
        }
    }

    /// Whether this changeset's diff is real and ready (ADR-037's third slot state, named from
    /// the other side) — [`App::refresh`]'s span-keyed reuse reads this to decide which existing
    /// slots may be carried over wholesale. Deliberately excludes `Failed` (reuse only carries
    /// `Ready` slots — `r` naturally retries a failed one instead, see the ADR's "Failures") and
    /// `Pending` (nothing yet to reuse).
    fn is_ready(&self) -> bool {
        matches!(self.slot, ChangesetSlot::Ready)
    }

    /// Build the [`ChangesetView`] for `cs` from its acquired [`ChangesetDiff`] (see
    /// [`crate::acquire::diff_changeset`]) — the router from "how was this changeset diffed" to
    /// the uniform [`DiffState`] shape every [`ChangesetView`] carries.
    pub fn from_changeset_diff(cs: Changeset, diff: ChangesetDiff) -> Self {
        let diff = match diff {
            ChangesetDiff::Committed(model) => DiffState::from_committed(model),
            ChangesetDiff::Uncommitted(diffs) => DiffState::from(diffs),
        };
        Self::new(cs, diff)
    }

    /// Number of files this changeset's diff touches — `App::new_uncommitted`'s "nothing to
    /// review" check (and its `main.rs` stack analog) read this rather than reaching into
    /// [`Self::diff`] directly, which stays private to this module.
    pub fn file_count(&self) -> usize {
        self.diff.files.len()
    }

    /// This changeset's combined file list — `App::outline_items` reads this to build the
    /// outline's rows without reaching into [`Self::diff`] directly (private to this module).
    pub fn files(&self) -> &[FileChange] {
        &self.diff.files
    }

    /// This changeset's file `idx`'s [`crate::outline::StagedStatus`] for the outline's status
    /// column, derived from the same unstaged/staged membership maps [`effective_zoom`] gates
    /// on. A committed changeset's maps are always all-`None` (see
    /// [`DiffState::from_committed`]), so this naturally resolves every one of its files to
    /// [`crate::outline::StagedStatus::None`] with no committed-specific branch — the outline's
    /// "status column only for the uncommitted changeset" requirement falls out of that, rather
    /// than being checked explicitly here.
    pub fn staged_status(&self, idx: usize) -> crate::outline::StagedStatus {
        let has_unstaged = self.diff.unstaged_idx.get(idx).copied().flatten().is_some();
        let has_staged = self.diff.staged_idx.get(idx).copied().flatten().is_some();
        crate::outline::StagedStatus::from_flags(has_unstaged, has_staged)
    }
}

/// One content region the renderer painted this frame, in terminal cell coordinates (CS10). A
/// deliberately tiny local shape rather than `ratatui::layout::Rect`: `app.rs` has no ratatui
/// dependency today, and this keeps it that way — `render.rs` (which already depends on
/// ratatui) converts a `Rect`'s content area into this when it writes [`App::hit_regions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Region {
    fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x && col < self.x + self.w && row >= self.y && row < self.y + self.h
    }
}

/// The content regions the last frame painted (CS10), written by `render::render` (which clears
/// this to `Default` at the top of every frame first) and read by [`App::handle_click`]/
/// [`App::handle_wheel`] to hit-test a mouse event's `(col, row)` against the region under the
/// pointer. A `None` field simply wasn't painted this frame — the outline is closed, or the
/// current file isn't in [`EffectiveZoom::Split`], etc. — never a stale rect from an earlier
/// frame's layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HitRegions {
    pub outline: Option<Region>,
    pub single: Option<Region>,
    pub unstaged: Option<Region>,
    pub staged: Option<Region>,
}

/// Which content region a mouse event hit-tested into (CS10's `App::hit_test`) — the outline,
/// the single-zoom diff pane, or one half of a split, tagged with which [`SplitPane`] so the
/// click/wheel handlers know whether to `toggle_split_focus` first.
enum HitPane {
    Outline,
    Single,
    Split(SplitPane),
}

/// Review session state: the active changeset's file list, per-file lazily loaded views, and
/// navigation/scroll state. One long-lived [`TsHighlighter`] lives here (not per file) — its
/// language-config cache is keyed per-instance, so a fresh highlighter per file would rebuild
/// every grammar config on every navigation.
pub struct App {
    repo: Repository,
    /// One [`ChangesetView`] per reviewable changeset — the Graphite stack, or a single
    /// synthetic uncommitted changeset when no stack is active (see
    /// [`crate::acquire::resolve_changesets`]) — in base → head order. Everything that used to
    /// be App-level diff state (`files`, the staged/unstaged sub-models + index maps, and the
    /// three `views_*` lazy caches) now lives on the ACTIVE entry's [`ChangesetView`]; read it
    /// through [`Self::cur`]/[`Self::cur_mut`] rather than indexing this directly.
    changesets: Vec<ChangesetView>,
    /// Index into [`Self::changesets`] of the active changeset. Moved by continuous file nav
    /// crossing a changeset boundary ([`Self::next_file`]/[`Self::prev_file`]) and by explicit
    /// changeset nav ([`Self::next_changeset`]/[`Self::prev_changeset`]/[`Self::goto_changeset`]),
    /// besides construction/refresh.
    current_cs: usize,
    pub current: usize,
    /// Row index, in the ACTIVE layout's coordinate space, of the highlighted navigation
    /// anchor — THE nav state (locked decision #2 in the M4 plan). In a split this is the
    /// FOCUSED pane's cursor; the unfocused pane's lives in [`Self::alt`]. `scroll` is derived
    /// from this every time it moves, via [`Self::derive_scroll`].
    pub cursor: usize,
    /// Top-of-viewport row index for the focused pane, in the active layout's space. Read
    /// directly by the renderer, but never written except by [`Self::derive_scroll`] — every
    /// cursor-moving method ends by calling it, so `scroll` always reflects the CURRENT `cursor`.
    pub scroll: usize,
    /// Content height of the focused pane, written by the renderer each frame. In a single-pane
    /// zoom this is the whole body; in a split it's the focused half (see [`Self::alt_height`]).
    pub pane_height: usize,
    /// The unfocused split pane's cursor+scroll, swapped with the focused pane's on `w` (see
    /// [`Self::toggle_split_focus`]). Meaningless outside [`EffectiveZoom::Split`].
    alt: PaneState,
    /// Content height of the unfocused split pane, written by the renderer alongside
    /// [`Self::pane_height`] — [`Self::derive_alt_scroll`] derives the unfocused pane's scroll
    /// against THIS, not the focused pane's height.
    pub(crate) alt_height: usize,
    /// Content height of the outline pane, written by the renderer each frame — same discipline
    /// as [`Self::pane_height`]. Read by [`Self::derive_outline_scroll`].
    pub outline_height: usize,
    /// The content regions the last frame painted (CS10 mouse support) — see [`HitRegions`]'s
    /// doc comment. Cleared and re-written by `render::render` every frame; read by
    /// [`Self::handle_click`]/[`Self::handle_wheel`].
    pub hit_regions: HitRegions,
    /// Label for the old side of the diff, shown next to a rename's `old_path` in the header.
    /// M4 only reviews the uncommitted (`HEAD` ↔ worktree) diffs, so this is always `"HEAD"`
    /// today; M5's committed-changeset zoom will want the changeset's actual base rev.
    pub base_label: String,
    highlighter: TsHighlighter,
    /// Current render layout; see [`Layout`]'s doc comment for the persistence contract.
    pub layout: Layout,
    /// The requested zoom (cycled by `z`); the effective per-file zoom is resolved each frame via
    /// [`effective_zoom`]. Persists across file navigation, like [`Self::layout`].
    pub zoom: Zoom,
    /// Which split pane has focus. Only meaningful under [`EffectiveZoom::Split`]; reset to
    /// `Unstaged` (the top pane) whenever a file opens or the zoom changes.
    split_focus: SplitPane,
    /// A transient, footer-rendered message — set by [`Self::notify`], cleared by
    /// [`Self::clear_notice`] (the latter called by the event loop on the next keypress, so a
    /// notice stays visible until the user acts). `None` renders the footer's normal hint string
    /// instead (see `render::render_footer`).
    pub notice: Option<Notice>,
    /// FIFO queue every staging verb enqueues through, then drains on the same beat (locked
    /// decision #5). Going through the queue (rather than calling `ops::apply_*` directly) buys
    /// the queue's lock-retry and panic isolation for free; because the drain is synchronous and
    /// a refresh follows before the next keystroke, only ever one op is in flight.
    queue: StagingQueue,
    /// The default write path (M2 verdict): libgit2's `Repository::apply`. Held as the concrete
    /// type — [`crate::apply::Applier`] stays a trait for the CLI escape hatch, but the field is
    /// the default.
    applier: Git2Applier,
    /// A destructive op awaiting the user's `y`/`n`/`Esc`. Set by [`Self::request_confirm`] (the
    /// discard verbs), resolved by [`Self::resolve_confirm`]. While `Some`, the event loop routes
    /// `y`/`n`/`Esc` to it and IGNORES every other key (a modal capture — see `tui::update`); the
    /// footer shows its prompt in place of the notice/hints.
    pub pending_confirm: Option<Confirm>,
    /// The anchor row of an active line selection (`v` sets it to the current [`Self::cursor`]),
    /// or `None` when no selection is active. Lives in the FOCUSED pane's active-layout coordinate
    /// space, exactly like [`Self::cursor`]: the selected range is
    /// `[min(anchor, cursor), max(anchor, cursor)]`, so `j`/`k` extend it for free as the cursor
    /// moves. Cancelled (not translated) whenever the coordinate space reshapes — layout toggle,
    /// zoom change, file switch, split-focus swap — since a raw row index carries no meaning across
    /// a reshape.
    pub selection_anchor: Option<usize>,
    /// Trap-4/5 livelock/interlock state for the M4 index watcher (locked decision #4: a
    /// synchronous poll-on-`Tick`, no threads). See [`Self::on_tick`] and
    /// [`Self::coordinated_refresh`].
    refresh_coordinator: RefreshCoordinator,
    /// The outline side pane's state — see [`OutlineState`]'s doc comment. Initialized by
    /// [`Self::from_changesets`] to open-when-`len() > 1`/unfocused/[`OutlineMode::default`]
    /// (the "decided without interview" default in the M5 plan), and repositioned (never
    /// rebuilt-from-scratch — `open`/`focused`/`mode` persist, like [`Self::layout`]/
    /// [`Self::zoom`]) by every diff-initiated nav and by [`Self::refresh`].
    outline: OutlineState,
    /// Opt-in nerd-font iconography — `workon.review.icons`, defaulting to [`IconMode::None`]
    /// (no auto-detection story exists — a terminal can't report the user's font). A TUI-wide
    /// appearance mode like the theme, not an outline view setting: it gates the outline's
    /// file/dir icons AND the summary panel's and winbar's glyphs (see `render.rs`).
    icon_mode: IconMode,
    /// Whether the `?` help overlay is showing (CS3). While `true`, `tui::update` intercepts
    /// every key as a modal (mirroring [`Self::pending_confirm`]'s capture) — see its doc comment
    /// for the precedence between the two modals.
    pub help_visible: bool,
    /// The `git workon review [<source>]` argument the session was launched with, set via
    /// [`Self::set_review_source`] (M7 CS2 fix). `None` means the session was launched via
    /// no-argument auto-detect (`crate::acquire::resolve_changesets`); `Some(source)` means an
    /// explicit ask (`stack`, `uncommitted`, and later CS3/CS4's ref/range/PR variants) that
    /// [`Self::refresh`] must re-resolve on every refresh, NEVER downgrade to auto-detect — a
    /// setter (rather than a constructor parameter) so `App::from_changesets`'s signature, and
    /// every existing test building through it, stays untouched.
    review_source: Option<Source>,
    /// CS4's idle-deferred load switch. `false` (the default) keeps every pre-CS4
    /// `open_current`/render-path behavior byte-identical, so the ~80 existing tests asserting
    /// eager loads keep passing unchanged. `main.rs` turns this on via [`Self::set_defer_loads`]
    /// right after construction; the event loop is what actually defers (see `tui.rs`'s
    /// `OPEN_DEBOUNCE`).
    defer_loads: bool,
    /// Set when [`Self::open_current`] deferred its load (only possible while
    /// [`Self::defer_loads`] is on) — the render path shows a placeholder instead of loading
    /// while this is `true`, and the event loop calls [`Self::complete_pending_open`] once input
    /// has been quiet for `OPEN_DEBOUNCE`. Read via [`Self::open_pending`].
    open_pending: bool,
    /// Whether a [`crate::app::FileLoadSpec`] has already been dispatched to the ADR-037 loader
    /// thread for the CURRENT pending open — set by [`Self::take_pending_load_spec`], cleared
    /// whenever a fresh open is marked pending. Without this, every idle `Tick` while
    /// `open_pending` stays true (the loader hasn't answered yet) would re-dispatch the same
    /// request; this makes dispatch idempotent across the pending open's whole lifetime.
    open_pending_dispatched: bool,
    /// ADR-037's global generation counter. Invariant: bumps ⟺ every view cache was invalidated
    /// — launch seeds it at `1` ([`Self::from_changesets`]); [`Self::refresh`] bumps it on every
    /// successful rebuild (still synchronous in this slice). A loader result whose `gen` doesn't
    /// match this is for a world that no longer exists and is dropped at the inbox chokepoint
    /// ([`Self::apply_file_ready`]) — the ONLY drop rule; within a generation, results are cached
    /// even if the user navigated away (warmth, not staleness — see the ADR's "Generations").
    generation: u64,
    /// Whether the CURRENT wave (startup's, or the most recent refresh's) has already raised its
    /// one footer notice for a `ChangesetReady { result: Err }`. Set by
    /// [`Self::apply_changeset_ready`]; reset to `false` by every [`Self::refresh`] right
    /// alongside the generation bump, since a refresh dispatching a NEW wave (ADR-037's "Refresh"
    /// changeset) starts that wave's own "first failure" count over — otherwise a stack whose
    /// first-ever wave had one bad changeset would never notify again for a LATER, unrelated
    /// failure. "the wave's first failure raises a footer notice" — this is what makes it FIRST
    /// per wave, not every one of a bad stack's failures within it.
    wave_failure_notified: bool,
    /// The ADR-037 refresh wave [`Self::refresh`] most recently queued (span-keyed reuse's
    /// changed/new committed spans, stamped with the generation they belong to), if any — taken
    /// (and cleared) by [`Self::take_pending_wave`]. Mirrors [`Self::open_pending`]/
    /// [`Self::take_pending_load_spec`]'s shape: `App` computes WHAT needs diffing but never
    /// touches a thread or a `Repository`-carrying `Sender` itself, so it stays constructible (and
    /// `refresh` stays synchronously testable) with nothing wired up to actually dispatch this.
    pending_wave: Option<(u64, Vec<(usize, Changeset)>)>,
}

/// A destructive staging op deferred behind a [`Confirm`], identified by index into [`App::files`]
/// (plus a hunk index for the hunk variant) rather than by a captured closure — an enum stores
/// cleanly on [`App`] and is resolved against the live diff at `y`-time. Every variant is a
/// discard.
///
/// Not `Copy`: [`Self::DiscardLines`] owns a `Vec` of per-hunk [`LineSelection`]s (the selection
/// snapshot, baked in at `d`-time so the confirm survives the intervening keystrokes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    /// Discard one hunk of the file at `file_idx` from the worktree. `hunk_idx` indexes the
    /// UNSTAGED role's hunks (discard only acts in the unstaged pane), matching where the cursor
    /// resolved it.
    DiscardHunk { file_idx: usize, hunk_idx: usize },
    /// Discard all of the file at `file_idx`'s worktree changes.
    DiscardFile { file_idx: usize },
    /// Discard a line selection of the file at `file_idx` from the worktree. `selections` is one
    /// `(hunk_idx, LineSelection)` per overlapped hunk, in the UNSTAGED role's coordinate space
    /// (discard only acts in the unstaged pane) — the frozen keep-predicate mapping from the
    /// selection active when the user pressed `d`.
    DiscardLines {
        file_idx: usize,
        selections: Vec<(usize, LineSelection)>,
    },
    /// CS7: discard every file in `files` — `(changeset name, file path)` pairs — from the
    /// worktree: an outline File row's single target, or a Dir row's every file under its path.
    /// Stored by NAME + PATH rather than raw `(cs_idx, file_idx)` indices because the confirm
    /// modal doesn't stop the tick beat: an external index change (e.g. `git add` from another
    /// terminal) can run a full refresh between `d` and `y`, rebuilding the per-changeset file
    /// lists and shifting positions — [`App::resolve_confirm`] re-resolves each pair against the
    /// LIVE changesets at answer time (silently skipping any that vanished) so a stale index can
    /// never discard the wrong file. `identity` is the acted-on outline row's
    /// [`OutlineRowIdentity`], captured at request-time for the post-op outline cursor restore.
    DiscardOutlineFiles {
        files: Vec<(String, String)>,
        identity: OutlineRowIdentity,
    },
}

/// A pending destructive op plus the scope-stating prompt shown on the footer until answered.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub prompt: String,
    pub op: PendingOp,
}

/// How severely a [`Notice`] should read in the footer — decides its color (see
/// `render::FG_ERROR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Info,
}

/// A transient footer message: the text to show and how severely to color it. Set via
/// [`App::notify`]; producers are the confirm/discard flows landing in m4-staging (refusals,
/// errors) — this crate has no in-crate caller yet, which is expected for a `pub` API this early.
#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub severity: Severity,
}

impl App {
    /// Build an [`App`] reviewing a single uncommitted changeset — the M2–M4 shape, and still
    /// what a non-Graphite (or clean-Graphite-tip) repo degrades to under M5's auto-detect
    /// (locked decision #7): a one-element [`Self::changesets`], `current_cs = 0`,
    /// `base_label = "HEAD"`. `test_support::app_from_fixture` and every existing M2–M4 test
    /// build through this constructor unchanged.
    pub fn new(repo: Repository, diffs: WorktreeDiffs) -> Self {
        let name = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(str::to_string))
            .unwrap_or_default();
        let cs = Changeset {
            name,
            span: ChangesetSpan::Uncommitted,
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::new(cs, DiffState::from(diffs));
        Self::from_changesets(repo, vec![view])
    }

    /// Build an [`App`] over an already-diffed changeset stack — `main.rs`'s entry point for
    /// both the Graphite-stack and single-uncommitted-changeset cases (the latter goes through
    /// [`Self::new`] instead, which is the same thing for a one-element stack). Opens on
    /// whichever changeset the lib marked `current` (locked decision #6: "honor lib `current`,
    /// first file"), falling back to index `0` if none is marked. An empty `changesets` panics —
    /// `main.rs` and [`Self::new`] never call this with one.
    pub fn from_changesets(repo: Repository, changesets: Vec<ChangesetView>) -> Self {
        assert!(
            !changesets.is_empty(),
            "App::from_changesets requires at least one changeset"
        );
        let current_cs = current_cs_index(&changesets);
        let base_label = base_label_for(&changesets[current_cs].cs);
        // Default-open when the stack has more than one changeset (the M5 plan's
        // "decided without interview" default — preserves the M4 full-width look for a lone
        // uncommitted changeset), unfocused (the diff keeps initial keyboard focus so the user
        // can start reading immediately), Stack mode (shows the structure M5 exists to surface).
        // Under the pure open/closed toggle (`o`) this is now a consistent split: `o` controls
        // visibility, `h`/[`App::focus_outline`] controls focus — so seeding open+unfocused here
        // doesn't fight the toggle the way it did under the old three-state cycle.
        let outline = OutlineState {
            open: changesets.len() > 1,
            focused: false,
            cursor: 0,
            mode: OutlineMode::default(),
            width: DEFAULT_OUTLINE_WIDTH,
            scroll: 0,
            order: OutlineOrder::default(),
        };
        let mut refresh_coordinator = RefreshCoordinator::new();
        // Seed the coordinator with the index signature as it stands right after this initial
        // diff, so the FIRST `Tick` doesn't see an "unseen" signature and spuriously re-diff an
        // index that hasn't actually changed since construction. `begin`/`complete` with no
        // refresh in between is just a way to prime `last_signature` through the same API a real
        // refresh uses — there's nothing to commit/supersede here, only one coordinator exists. If
        // the initial read fails (e.g. a repo with no index file yet), leave it unseeded: the
        // first tick will see a "new" signature and refresh once, which is harmless — cheaper than
        // threading an extra error path through construction.
        if let Ok(sig) = IndexSignature::read(repo.path()) {
            let ticket = refresh_coordinator.begin();
            refresh_coordinator.complete(ticket, sig);
        }
        let mut app = Self {
            repo,
            changesets,
            current_cs,
            current: 0,
            cursor: 0,
            scroll: 0,
            pane_height: 20,
            alt: PaneState::default(),
            alt_height: 20,
            outline_height: 20,
            hit_regions: HitRegions::default(),
            base_label,
            highlighter: TsHighlighter::new(),
            layout: Layout::default(),
            zoom: Zoom::default(),
            split_focus: SplitPane::Unstaged,
            notice: None,
            queue: StagingQueue::new(),
            applier: Git2Applier,
            pending_confirm: None,
            selection_anchor: None,
            refresh_coordinator,
            outline,
            icon_mode: IconMode::default(),
            help_visible: false,
            review_source: None,
            defer_loads: false,
            open_pending: false,
            open_pending_dispatched: false,
            generation: 1,
            wave_failure_notified: false,
            pending_wave: None,
        };
        // Position the outline cursor on the changeset/file the lib marked `current` (the same
        // row `sync_outline_to_current` would reposition to after any diff-initiated nav) rather
        // than leaving it at its default `0` — in Stack mode row `0` is a HEADER, not the current
        // file, whenever the current changeset isn't the first in the stack.
        app.sync_outline_to_current();
        app
    }

    /// Record the `[SOURCE]` argument the review session was launched with, so
    /// [`Self::refresh`] re-resolves that same ask instead of silently falling back to
    /// no-argument auto-detect (M7 CS2 fix). `main.rs` calls this right after
    /// [`Self::from_changesets`] whenever a `[SOURCE]` argument was given; a no-argument launch
    /// never calls it, leaving [`Self::review_source`] at its `None` default.
    pub fn set_review_source(&mut self, source: Source) {
        self.review_source = Some(source);
    }

    /// The current `.git/index`'s cheap fingerprint (mtime + size), or `None` if the read fails —
    /// tolerated rather than propagated, since a transient read error (e.g. a concurrent git
    /// process mid-write) must not crash the TUI or wedge the tick loop; the next tick just tries
    /// again. `repo.path()` is the `.git` directory itself, which [`IndexSignature::read`] expects.
    fn index_signature(&self) -> Option<IndexSignature> {
        IndexSignature::read(self.repo.path()).ok()
    }

    /// Refresh wrapped with [`RefreshCoordinator`] bookkeeping — the entry point every refresh
    /// trigger (manual `r`, and the post-staging-op drain) must go through instead of calling
    /// [`Self::refresh`] directly, so `last_signature` stays current and a `Tick` right after
    /// doesn't mistake our own write for an external one (trap 5's echo-suppression).
    ///
    /// The signature is read AFTER `self.refresh()` runs, not before: `refresh`'s own diffing can
    /// itself touch the index's stat cache (see [`RefreshCoordinator::complete`]'s doc comment for
    /// why the post-completion signature, not the pre-refresh one, is the one that must be
    /// recorded). If the post-refresh read fails, `last_signature` simply isn't updated this
    /// round — the next tick will see a "new" signature and refresh again, which is a harmless
    /// extra re-diff, not a crash.
    pub fn coordinated_refresh(&mut self) {
        let ticket = self.refresh_coordinator.begin();
        self.refresh();
        if let Some(sig) = self.index_signature() {
            self.refresh_coordinator.complete(ticket, sig);
        }
    }

    /// The periodic `Tick` hook (locked decision #4: sync poll, no threads/channels). Reads the
    /// current index signature and, if [`RefreshCoordinator::note_index_event`] says it's a
    /// genuinely new, unseen state with no staging op in flight, runs a [`Self::coordinated_refresh`].
    /// A failed signature read is a silent no-op (tolerated, see [`Self::index_signature`]) — the
    /// next tick just tries again.
    pub fn on_tick(&mut self) {
        let Some(sig) = self.index_signature() else {
            return;
        };
        if self
            .refresh_coordinator
            .note_index_event(sig, self.queue.len())
        {
            self.coordinated_refresh();
        }
    }

    /// The active changeset's view — every read site that used to reach an App-level diff field
    /// directly now goes through this (or [`Self::cur_mut`]).
    fn cur(&self) -> &ChangesetView {
        &self.changesets[self.current_cs]
    }

    /// Mutable analog of [`Self::cur`].
    fn cur_mut(&mut self) -> &mut ChangesetView {
        &mut self.changesets[self.current_cs]
    }

    /// The active changeset's combined file list — `render.rs` and tests read this instead of
    /// the old `pub files` field, which moved onto [`ChangesetView`] (see [`Self::cur`]).
    pub fn files(&self) -> &[FileChange] {
        &self.cur().diff.files
    }

    /// ADR-037's global generation — see the field's doc comment for the invariant. The loader
    /// thread stamps every [`FileLoadSpec`] request it's handed with this value at send time;
    /// [`Self::apply_file_ready`] drops a result whose stamp no longer matches.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Index into the reviewed stack of the active changeset — read by tests asserting the
    /// [`Self::from_changesets`]/[`Self::refresh`] "honor lib `current`" rule (locked decision
    /// #6), and by changeset-nav's own tests ([`Self::next_changeset`]/[`Self::prev_changeset`]/
    /// [`Self::goto_changeset`]).
    pub fn current_cs(&self) -> usize {
        self.current_cs
    }

    /// Whether the ACTIVE changeset's slot is `Pending` (ADR-037) — `render.rs`'s body path
    /// shows a changeset-level loading placeholder instead of "(no changes)"/per-file content
    /// while this holds.
    pub fn is_current_pending(&self) -> bool {
        self.cur().is_pending()
    }

    /// The ACTIVE changeset's failure message, if its slot is `Failed` (ADR-037) — `render.rs`'s
    /// body path shows this instead of a diff body.
    pub fn current_failure(&self) -> Option<&str> {
        self.cur().failure_message()
    }

    /// The active changeset's descriptor (name, source, restack status) — read by tests
    /// asserting which changeset [`Self::current_cs`] landed on.
    pub fn current_changeset(&self) -> &Changeset {
        &self.cur().cs
    }

    /// Number of changesets in the reviewed stack — `1` for a non-Graphite (or
    /// clean-Graphite-tip) repo, per locked decision #7.
    pub fn changeset_count(&self) -> usize {
        self.changesets.len()
    }

    /// Whether the ACTIVE changeset is a committed range (`base..head`) rather than the
    /// uncommitted worktree layer — derived from [`workon::ChangesetSpan`] on every call rather
    /// than cached (locked decision #2's "derive, don't store" mode gate). Drives every
    /// committed-mode guard: the mode-aware staging refusal, skipping combined attribution (no
    /// staged/unstaged sets exist to color by), and locking zoom to combined.
    pub fn is_committed(&self) -> bool {
        matches!(
            self.cur().cs.span,
            ChangesetSpan::Committed { .. } | ChangesetSpan::CommittedRoot { .. }
        )
    }

    /// Re-run [`crate::acquire::resolve_changesets`] against the CURRENT `HEAD` branch and
    /// rebuild [`Self::changesets`] — the operation both a manual refresh (`r`) and the
    /// post-staging-op/external-write refresh need. Re-assembling (not just re-diffing the
    /// active changeset) matters because a restack can change the stack's topology, not just its
    /// diffs.
    ///
    /// ADR-037 "Refresh" — span-keyed reuse, uncommitted always sync:
    ///
    /// - Resolve (this method's first half) stays fully synchronous on the main thread — it's
    ///   offline and cheap, and re-running it on every refresh is what keeps [`Self::review_source`]
    ///   honored (see below).
    /// - The rebuilt view list carries over any existing `Ready` slot whose `(name, span)` is
    ///   unchanged — a committed diff is a pure function of its span
    ///   ([`ChangesetSpan::Committed`] compares `base`/`head`; [`ChangesetSpan::CommittedRoot`]
    ///   compares `head`) — so an ordinary post-staging refresh re-diffs *nothing but the
    ///   uncommitted layer*. A carried slot keeps its `DiffState` AND warm view caches verbatim:
    ///   never blanked, never re-diffed. Reuse only ever carries a `Ready` slot — a `Failed` one
    ///   goes back through the `Pending`+wave path below, which is how `r` naturally retries it
    ///   with no separate retry machinery.
    /// - The [`ChangesetSpan::Uncommitted`] layer is never "unchanged": it re-diffs
    ///   SYNCHRONOUSLY, right here, on every refresh — ms-scale, and this is what preserves
    ///   staging's guarantee that the next keystroke sees the post-op world (an async refresh
    ///   would let a second `s` compute its patch against a stale diff). A failed sync re-diff
    ///   becomes a `Failed` slot plus a footer notice (an explicit error beats stale wrong
    ///   content) rather than aborting the whole refresh.
    /// - Every other changed-or-new committed span becomes a `Pending` slot; the caller (the
    ///   event loop, via [`Self::take_pending_wave`]) dispatches those as an async wave, current-
    ///   first if the active changeset is among them, same as the streamed-launch wave. Their
    ///   results land through [`Self::apply_changeset_ready`] tagged with the NEW generation.
    /// - Every refresh bumps the generation exactly once, right where the view caches it protects
    ///   are actually replaced — reused (carried) slots' in-flight loader results now carry a
    ///   stale `gen` and die at [`Self::apply_file_ready`]'s chokepoint; accepted waste for one
    ///   global rule (see the ADR's "Generations"). [`Self::wave_failure_notified`] resets
    ///   alongside it, so the freshly-dispatched wave gets its own first-failure notice.
    ///
    /// Position rules, adapted to the streamed world:
    /// - Preserves the active changeset by NAME: if a changeset with that name still exists in
    ///   the rebuilt stack, `current_cs` follows it; otherwise it falls back to whichever
    ///   changeset the lib now reports as `current`, or index `0`.
    /// - A carried (still-`Ready`) active changeset — or the always-sync uncommitted layer —
    ///   preserves file position by PATH exactly like before streaming: `current` follows the
    ///   path if it still exists, else clamps into the new list (or `0` if empty), then
    ///   [`Self::open_current`] re-seats it at the first hunk (this does NOT try to preserve the
    ///   exact cursor row — jumping to the first hunk is the only always-valid choice once the
    ///   diff is rebuilt, consistent with zoom/layout switches). An active changeset that went
    ///   `Pending` instead has no diff yet to preserve a path INTO — `current` resets to `0` and
    ///   [`Self::apply_changeset_ready`] re-seats it (to its first file) exactly as it already
    ///   does for a freshly-`Pending` changeset, once that `ChangesetReady` lands.
    /// - The rebuilt changeset list can resize/reorder the outline's row list out from under its
    ///   cursor — reposition it, same as every other diff-initiated nav (does NOT touch
    ///   `outline.open`/`focused`/`mode`, which persist across a refresh like `layout`/`zoom`).
    ///
    /// On a resolve/assembly error (or the uncommitted layer's own sync re-diff failing — see
    /// above), leaves the rest of `Self::changesets` untouched and sets an error [`Notice`]
    /// instead (via [`Self::notify`]) — a failed refresh must never blank the review.
    ///
    /// Dispatches on [`Self::review_source`] (M7 CS2 fix): a no-argument launch (`None`) re-runs
    /// today's auto-detect ([`crate::acquire::resolve_changesets`]); an explicit-source launch
    /// (`Some`) re-runs [`crate::source::resolve_source`] against THAT source, never auto-detect
    /// — every ref-shaped source variant (`Stack`, `Uncommitted`, `Ref`, `Range`) is offline,
    /// so re-resolving on every refresh (manual `r` and the tick-driven index watcher alike) is
    /// cheap and safe. Without this, both refresh triggers would silently swap an explicit
    /// review (e.g. `uncommitted`) for the current `HEAD`'s auto-detected state.
    /// [`Source::Pr`] is the one exception: it resolves over the network (gh metadata + fetch),
    /// so refresh is a no-op for it — see the match arm below.
    pub fn refresh(&mut self) {
        let Some(head_branch) = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(str::to_string))
        else {
            self.notify("refresh failed: no current branch", Severity::Error);
            return;
        };

        let changesets = match &self.review_source {
            None => crate::acquire::resolve_changesets(&self.repo, &head_branch)
                .map_err(|err| err.to_string()),
            // A PR review is committed-only: nothing it renders depends on the index/worktree
            // state that refresh exists to pick up, and re-resolving would hit the network
            // (gh metadata + fetch) on every tick-driven refresh. Remote freshness is a
            // re-launch, not a refresh.
            Some(Source::Pr(_)) => return,
            Some(source) => resolve_source(&self.repo, &head_branch, source.clone())
                .map_err(|err| err.to_string()),
        };
        let changesets = match changesets {
            Ok(cs) => cs,
            Err(err) => {
                self.notify(format!("refresh failed: {err}"), Severity::Error);
                return;
            }
        };
        // `resolve_changesets` always returns at least one changeset (a lone Uncommitted entry
        // when no stack is active), but stay defensive rather than index an empty `Vec` below.
        if changesets.is_empty() {
            self.notify("refresh failed: no changesets to review", Severity::Error);
            return;
        }

        let prev_cs_name = self.cur().cs.name.clone();
        let current_path = self
            .cur()
            .diff
            .files
            .get(self.current)
            .map(|f| f.path.clone());

        // Span-keyed reuse: pull the OLD view list out so a `Ready` slot whose `(name, span)`
        // survives can be moved (not cloned) into the rebuilt list, keeping its warm view caches.
        // `Vec::remove`'s O(n) shift is immaterial at stack sizes (a handful of changesets).
        let mut old_views = std::mem::take(&mut self.changesets);

        let mut new_views: Vec<ChangesetView> = Vec::with_capacity(changesets.len());
        let mut to_diff: Vec<(usize, Changeset)> = Vec::new();
        let mut uncommitted_diff_failed: Option<String> = None;

        for cs in changesets {
            if cs.span == ChangesetSpan::Uncommitted {
                match crate::acquire::diff_changeset(&self.repo, &cs) {
                    Ok(diff) => new_views.push(ChangesetView::from_changeset_diff(cs, diff)),
                    Err(err) => {
                        let message = err.to_string();
                        uncommitted_diff_failed = Some(message.clone());
                        new_views.push(ChangesetView::failed(cs, message));
                    }
                }
                continue;
            }
            if let Some(pos) = old_views
                .iter()
                .position(|v| v.is_ready() && v.cs.name == cs.name && v.cs.span == cs.span)
            {
                // Carry the slot's diff/view caches verbatim, but adopt the FRESH descriptor —
                // metadata like `needs_restack` can change even when the span itself didn't.
                let mut reused = old_views.remove(pos);
                reused.cs = cs;
                new_views.push(reused);
            } else {
                let idx = new_views.len();
                to_diff.push((idx, cs.clone()));
                new_views.push(ChangesetView::pending(cs));
            }
        }

        self.current_cs = new_views
            .iter()
            .position(|v| v.cs.name == prev_cs_name)
            .unwrap_or_else(|| current_cs_index(&new_views));
        self.base_label = base_label_for(&new_views[self.current_cs].cs);
        self.changesets = new_views;
        // ADR-037: every refresh bumps the generation, right where the view caches it protects
        // are actually replaced — an early `return` above (a failed resolve) leaves the old
        // world's caches intact, so it must NOT bump. Any loader result still in flight for the
        // pre-refresh world now carries a stale `gen` and dies at `apply_file_ready`'s chokepoint;
        // same for a wave result still in flight for a superseded generation.
        self.generation += 1;
        self.wave_failure_notified = false;

        if self.cur().is_pending() {
            self.current = 0;
        } else {
            let n = self.cur().diff.files.len();
            self.current = current_path
                .and_then(|path| self.cur().diff.files.iter().position(|f| f.path == path))
                .unwrap_or(if n == 0 { 0 } else { self.current.min(n - 1) });
        }
        self.open_current();
        self.sync_outline_to_current();

        if let Some(err) = uncommitted_diff_failed {
            self.notify(
                format!("refresh failed: uncommitted diff failed: {err}"),
                Severity::Error,
            );
        }

        self.pending_wave = if to_diff.is_empty() {
            None
        } else {
            Some((self.generation, to_diff))
        };
    }

    /// Resolve the [`EffectiveZoom`] for file `idx` this frame: the requested [`Self::zoom`] gated
    /// against that file's available sub-diffs and stageability. Cheap (three lookups + the pure
    /// [`effective_zoom`]) — re-evaluated per file per frame, no caching (locked decision #3).
    pub(crate) fn effective_zoom_for(&self, idx: usize) -> EffectiveZoom {
        let can_stage = self
            .cur()
            .diff
            .files
            .get(idx)
            .map(|f| !f.is_binary)
            .unwrap_or(false);
        let has_unstaged = self
            .cur()
            .diff
            .unstaged_idx
            .get(idx)
            .copied()
            .flatten()
            .is_some();
        let has_staged = self
            .cur()
            .diff
            .staged_idx
            .get(idx)
            .copied()
            .flatten()
            .is_some();
        effective_zoom(self.zoom, has_unstaged, has_staged, can_stage)
    }

    /// The role whose view [`Self::cursor`]/[`Self::scroll`] currently drive for file `idx`: the
    /// single effective role, or the focused split pane's role.
    fn focused_role_for(&self, idx: usize) -> Role {
        match self.effective_zoom_for(idx) {
            EffectiveZoom::Single(role) => role,
            EffectiveZoom::Split => self.split_focus_role(),
        }
    }

    /// The sub-[`FileChange`] backing file `idx`'s `role` view: `self.files[idx]` itself for
    /// [`Role::Combined`], or the matching entry in the unstaged/staged model (`None` if that
    /// role has no change for this file). Used by the renderer to build a fresh
    /// [`crate::attribute::Attribution`] for the combined role each frame — see that module's
    /// docs for why the two sub-roles' hunks (not the combined ones) are the attribution source.
    pub(crate) fn role_change(&self, idx: usize, role: Role) -> Option<&FileChange> {
        let diff = &self.cur().diff;
        match role {
            Role::Combined => diff.files.get(idx),
            Role::Unstaged => diff
                .unstaged_idx
                .get(idx)
                .copied()
                .flatten()
                .map(|mi| &diff.unstaged_model.files[mi]),
            Role::Staged => diff
                .staged_idx
                .get(idx)
                .copied()
                .flatten()
                .map(|mi| &diff.staged_model.files[mi]),
        }
    }

    /// The role of the currently focused split pane (or the pane that WOULD be focused). See
    /// [`Self::split_focus`].
    pub(crate) fn split_focus_role(&self) -> Role {
        match self.split_focus {
            SplitPane::Unstaged => Role::Unstaged,
            SplitPane::Staged => Role::Staged,
        }
    }

    fn unfocused_split_role(&self) -> Role {
        match self.split_focus {
            SplitPane::Unstaged => Role::Staged,
            SplitPane::Staged => Role::Unstaged,
        }
    }

    fn views_for(&self, role: Role) -> &[Option<FileView>] {
        let cur = self.cur();
        match role {
            Role::Combined => &cur.views_combined,
            Role::Unstaged => &cur.views_unstaged,
            Role::Staged => &cur.views_staged,
        }
    }

    fn views_for_mut(&mut self, role: Role) -> &mut [Option<FileView>] {
        let cur = self.cur_mut();
        match role {
            Role::Combined => &mut cur.views_combined,
            Role::Unstaged => &mut cur.views_unstaged,
            Role::Staged => &mut cur.views_staged,
        }
    }

    /// Read-only access to file `idx`'s already-loaded [`FileView`] for `role` (`None` if the role
    /// has no change for the file, or it isn't loaded yet). `pub` (not `pub(crate)`) so the
    /// separate `git-workon-review` bin crate's `tui.rs` tests can assert a file was — or, more
    /// importantly, was NOT — loaded without visiting it (CS2's event-coalescing regression
    /// test); read-only and does not touch `open_current`/`ensure_loaded`/`outline_move_by`'s
    /// eager-load semantics.
    pub fn role_view_ref(&self, idx: usize, role: Role) -> Option<&FileView> {
        self.views_for(role).get(idx).and_then(|v| v.as_ref())
    }

    /// Mutable access to file `idx`'s already-loaded [`FileView`] for `role` — does NOT trigger a
    /// load (call [`Self::ensure_role_loaded`] first). Used by the renderer's word-span
    /// cache-populating pass.
    pub(crate) fn role_view_mut(&mut self, idx: usize, role: Role) -> Option<&mut FileView> {
        self.views_for_mut(role)
            .get_mut(idx)
            .and_then(|v| v.as_mut())
    }

    /// Load (and cache) every [`FileView`] needed to render file `idx` under its effective zoom:
    /// the single effective role, or BOTH split panes' roles. Binary files load nothing (the
    /// renderer shows a placeholder).
    pub fn ensure_loaded(&mut self, idx: usize) {
        match self.effective_zoom_for(idx) {
            EffectiveZoom::Single(role) => self.ensure_role_loaded(idx, role),
            EffectiveZoom::Split => {
                self.ensure_role_loaded(idx, Role::Unstaged);
                self.ensure_role_loaded(idx, Role::Staged);
            }
        }
    }

    /// Load file `idx`'s [`FileView`] for one `role`, unless already loaded, binary, or the role
    /// has no change for the file. The combined role builds from [`Self::files`]; the sub-roles
    /// build from the matching [`FileChange`] in the unstaged/staged model. Each role's text is
    /// sourced from the two revisions its hunks were diffed against (see [`FileView::load`]) so
    /// context lines match on both sides.
    fn ensure_role_loaded(&mut self, idx: usize, role: Role) {
        let model_idx = match role {
            Role::Combined => {
                let Some(file) = self.cur().diff.files.get(idx) else {
                    return;
                };
                if file.is_binary {
                    return;
                }
                if self.cur().views_combined.get(idx).map(Option::is_some) != Some(false) {
                    return;
                }
                None
            }
            Role::Unstaged => self.cur().diff.unstaged_idx.get(idx).copied().flatten(),
            Role::Staged => self.cur().diff.staged_idx.get(idx).copied().flatten(),
        };

        if role != Role::Combined {
            let Some(mi) = model_idx else {
                return; // no change in this role for this file
            };
            if self.views_for(role).get(idx).map(Option::is_some) != Some(false) {
                return; // already loaded (or slot absent)
            }
            let file = match role {
                Role::Unstaged => self.cur().diff.unstaged_model.files[mi].clone(),
                Role::Staged => self.cur().diff.staged_model.files[mi].clone(),
                Role::Combined => unreachable!(),
            };
            // `file` is cloned out of `self.cur()` (rather than a borrow) because
            // `build_sub_role_view` needs `&self.repo` and `&mut self.highlighter` at once, which
            // a borrow still anchored in `self.cur()` would conflict with — same rationale as the
            // combined path below.
            let Some(view) = build_sub_role_view(&self.repo, &mut self.highlighter, role, &file)
            else {
                return;
            };
            self.views_for_mut(role)[idx] = Some(view);
            return;
        }

        // Combined role. `self.cur().cs.span`/`self.cur().diff.files[idx].clone()` are read out
        // (rather than borrowed) for the same reason as the sub-role branch above —
        // `build_combined_view` needs `&self.repo` and `&mut self.highlighter` together.
        let span = self.cur().cs.span;
        let file = self.cur().diff.files[idx].clone();
        let Some(view) = build_combined_view(&self.repo, &mut self.highlighter, span, &file) else {
            return;
        };
        self.cur_mut().views_combined[idx] = Some(view);
    }

    pub fn current_view(&mut self) -> Option<&mut FileView> {
        let role = self.focused_role_for(self.current);
        self.ensure_role_loaded(self.current, role);
        self.role_view_mut(self.current, role)
    }

    /// The focused pane's [`FileView`] for the current file — the effective single role, or the
    /// focused split pane's role. `None` if unloaded (binary, or a role with no change).
    pub fn current_view_ref(&self) -> Option<&FileView> {
        self.role_view_ref(self.current, self.focused_role_for(self.current))
    }

    /// First-hunk row (in the active layout's space) of file `idx`'s `role` view, or 0 when the
    /// view is absent/unloaded. Reads whichever of [`FileView`]'s two first-hunk fields matches
    /// [`Self::layout`].
    fn role_first_hunk(&self, idx: usize, role: Role) -> usize {
        self.role_view_ref(idx, role)
            .map(|v| match self.layout {
                Layout::Sbs => v.first_hunk_row,
                Layout::Inline => v.first_inline_hunk_row,
            })
            .unwrap_or(0)
    }

    /// Reset BOTH panes to their role views' first hunks and refocus the top (unstaged) pane —
    /// run on file open and zoom change. The two role coordinate spaces disagree, so carrying a
    /// raw cursor index across a role/zoom switch would be meaningless; jumping to the role's own
    /// first hunk (the same position a fresh file open lands on) is always valid and predictable.
    ///
    /// This is also what `coordinated_refresh` leaves behind after a staging op (via
    /// `open_current`), since a refresh is itself a file "open" of the post-op state — CS6's
    /// `App::restore_position` runs immediately after, overwriting this first-hunk reseat with
    /// the reviewer's pre-op position when it can. Every OTHER caller (manual file/changeset
    /// nav, zoom cycles) has no such follow-up, so first-hunk-on-open is still what they see.
    fn reset_panes(&mut self) {
        // Any file open / zoom change reshapes the coordinate space an active selection is keyed
        // in, so drop it (see [`Self::selection_anchor`]).
        self.selection_anchor = None;
        self.split_focus = SplitPane::Unstaged;
        self.alt = PaneState::default();
        match self.effective_zoom_for(self.current) {
            EffectiveZoom::Single(role) => {
                self.cursor = self.role_first_hunk(self.current, role);
            }
            EffectiveZoom::Split => {
                self.cursor = self.role_first_hunk(self.current, Role::Unstaged);
                self.alt.cursor = self.role_first_hunk(self.current, Role::Staged);
            }
        }
        self.derive_scroll();
        // The unfocused pane's scroll is derived at render time, once its height is known.
    }

    /// Jump the focused pane's cursor to its role view's first hunk, then re-derive `scroll`.
    pub fn jump_to_first_hunk(&mut self) {
        let role = self.focused_role_for(self.current);
        self.cursor = self.role_first_hunk(self.current, role);
        self.derive_scroll();
    }

    /// Turn CS4's idle-deferred load mode on/off. `main.rs` calls this with `true` right after
    /// [`Self::from_changesets`], before the first [`Self::open_current`] — see the field's doc
    /// comment. Exposed as a setter (rather than folded into construction) so every existing test
    /// building through `from_changesets`/`App::new` keeps today's eager behavior untouched.
    pub fn set_defer_loads(&mut self, on: bool) {
        self.defer_loads = on;
    }

    /// Whether CS4's idle-deferred load mode is on — see [`Self::set_defer_loads`].
    pub fn defer_loads(&self) -> bool {
        self.defer_loads
    }

    /// Whether [`Self::open_current`] deferred its load and it hasn't been completed yet — the
    /// render path (in defer mode) and the event loop both read this: render to decide whether to
    /// show the placeholder, the event loop to decide whether to shorten its poll timeout and to
    /// call [`Self::complete_pending_open`] on the next idle tick.
    pub fn open_pending(&self) -> bool {
        self.open_pending
    }

    /// Load the current file's needed views and reset both panes to their first hunks.
    ///
    /// In [`Self::defer_loads`] mode a file whose views are NOT yet cached does not load here:
    /// the open is marked pending and the panes reset anyway (the cursor falls back to row 0
    /// for the still-unloaded view, via [`Self::role_first_hunk`]'s `unwrap_or(0)` — harmless,
    /// since the body renders a placeholder until [`Self::complete_pending_open`] runs). A file
    /// whose views ARE cached takes the eager path even in defer mode: `ensure_loaded` is a
    /// pure cache hit there, and deferring would only trade an instantly-renderable diff for a
    /// placeholder flash lasting the debounce window — revisiting a file is the most common
    /// navigation of all, and it must render immediately. Outside defer mode this is exactly
    /// the pre-defer eager behavior.
    pub fn open_current(&mut self) {
        // An empty file list (a `Pending`/`Failed` slot, ADR-037, or a genuinely empty committed
        // changeset) has nothing to defer: `current_load_spec` indexes `diff.files[self.current]`
        // unconditionally, which would panic on the loader-dispatch path if `open_pending` were
        // set here for a file that doesn't exist. There's nothing to load either way — this is
        // the same "no-op on an empty file list" contract `main.rs`'s streamed-launch comment
        // documents for a fresh `Pending` changeset, made actually true rather than incidental.
        if self.defer_loads && !self.files().is_empty() && !self.current_views_cached() {
            self.open_pending = true;
            // A fresh pending open has nothing dispatched to the loader yet — see
            // [`Self::take_pending_load_spec`].
            self.open_pending_dispatched = false;
            self.reset_panes();
            return;
        }
        self.ensure_loaded(self.current);
        self.reset_panes();
        // F1: an eager load or the empty-file no-op above supersedes any STALE deferred open —
        // e.g. a pending open set before `r`, followed by a refresh that turns the active
        // changeset `Pending` (empty files, skipping the defer branch above since there's
        // nothing to load). Without this, the stale flags survive the refresh and wedge the
        // idle-Tick fast-poll loop (`take_pending_load_spec` keeps returning `None` for a file
        // that no longer exists) while the placeholder stays stuck. Mirrors
        // [`Self::complete_pending_open`]'s tail.
        self.open_pending = false;
        self.open_pending_dispatched = false;
    }

    /// Whether the view(s) the current file's effective zoom needs are already cached, making a
    /// deferred open pointless (`ensure_loaded` would be a cache hit). Split checks EITHER pane:
    /// a role with no change for the file stays legitimately `None` forever (see
    /// [`Self::ensure_role_loaded`]), so requiring both would defer a one-role file every time.
    /// A partially-cached split (one loadable pane in, one missing) takes the eager path and
    /// loads the single missing pane synchronously — one file, cheap, and consistent with the
    /// both-`None` gate the render placeholder uses.
    fn current_views_cached(&self) -> bool {
        match self.effective_zoom_for(self.current) {
            EffectiveZoom::Single(role) => self.role_view_ref(self.current, role).is_some(),
            EffectiveZoom::Split => {
                self.role_view_ref(self.current, Role::Unstaged).is_some()
                    || self.role_view_ref(self.current, Role::Staged).is_some()
            }
        }
    }

    /// Complete a deferred open, if one is pending: load the current file's needed views, then
    /// reset both panes again so the cursor now derives from the REAL first-hunk row (rather than
    /// the `0` fallback [`Self::open_current`] left it at). A no-op when nothing is pending —
    /// idempotent, so the event loop can call this liberally (e.g. on every idle tick while
    /// pending) without worrying about double-loading.
    ///
    /// Invariant this pins (the equivalence the tests assert): after this returns, `App` state is
    /// byte-identical to what an eager [`Self::open_current`] would have produced for the same
    /// current file.
    pub fn complete_pending_open(&mut self) {
        if !self.open_pending {
            return;
        }
        self.ensure_loaded(self.current);
        self.reset_panes();
        self.open_pending = false;
        self.open_pending_dispatched = false;
    }

    // ── ADR-037: the loader thread's request/result seam ────────────────────────

    /// Snapshot everything the ADR-037 loader needs to load the CURRENT file, mirroring exactly
    /// what [`Self::ensure_loaded`] would read from live state — see [`FileLoadSpec`]'s doc
    /// comment. Every field is owned (cloned out), so the spec outlives the borrow and can cross
    /// to the loader thread.
    ///
    /// `None` when the current changeset's file list doesn't have an entry at `self.current` —
    /// a clean uncommitted layer (zero files) is the common case, and a Pending/Failed slot
    /// (also zero files) will join this once ADR-037's later changesets land. Total by
    /// construction rather than relying on callers to guard first.
    fn current_load_spec(&self) -> Option<FileLoadSpec> {
        let idx = self.current;
        let zoom = self.effective_zoom_for(idx);
        let diff = &self.cur().diff;
        let combined_file = diff.files.get(idx)?.clone();
        Some(FileLoadSpec {
            span: self.cur().cs.span,
            combined_file,
            zoom,
            unstaged_file: diff
                .unstaged_idx
                .get(idx)
                .copied()
                .flatten()
                .map(|mi| diff.unstaged_model.files[mi].clone()),
            staged_file: diff
                .staged_idx
                .get(idx)
                .copied()
                .flatten()
                .map(|mi| diff.staged_model.files[mi].clone()),
        })
    }

    /// Take the [`FileLoadSpec`] for the current pending open, tagged with the generation/
    /// changeset/file it was built against — but ONLY if a request hasn't already been dispatched
    /// for this same pending open (see [`Self::open_pending_dispatched`]'s doc comment). The event
    /// loop calls this on every idle `Tick` while an open is pending; without the dispatched guard
    /// it would re-send the same request on every one of those ticks until the loader answers.
    /// Returns `None` when nothing is pending, a request already went out for it, or the
    /// current file has no spec to build (see [`Self::current_load_spec`]) — the pending flags
    /// are left alone in that last case, matching upstack's eventual empty-file guard in
    /// [`Self::open_current`]: this is a total fallback for a defer that outraced it, not a
    /// second copy of that guard.
    pub fn take_pending_load_spec(&mut self) -> Option<(u64, usize, usize, FileLoadSpec)> {
        if !self.open_pending || self.open_pending_dispatched {
            return None;
        }
        let spec = self.current_load_spec()?;
        self.open_pending_dispatched = true;
        Some((self.generation, self.current_cs, self.current, spec))
    }

    /// Take the ADR-037 refresh wave [`Self::refresh`] most recently queued (span-keyed reuse's
    /// changed/new committed spans), if any — `None` when the last refresh had nothing left to
    /// diff asynchronously (every span was reused or is the always-sync uncommitted layer; this
    /// is what keeps a single-uncommitted-changeset session's refresh effectively synchronous,
    /// see the ADR's "Refresh"). Mirrors [`Self::take_pending_load_spec`]'s take-once shape: the
    /// caller (the event loop — the only place with thread-spawning ability) is responsible for
    /// actually dispatching it; `App` never touches a `Sender`/`Repository`-carrying handle
    /// itself, so it stays constructible — and `refresh` stays synchronously testable — with
    /// nothing wired up to consume this at all.
    pub fn take_pending_wave(&mut self) -> Option<(u64, Vec<(usize, Changeset)>)> {
        self.pending_wave.take()
    }

    /// Apply one loader result (ADR-037's chokepoint, the `FileReady` inbox arm routes here):
    /// dropped outright on a generation mismatch (`gen != self.generation` — the world it was
    /// computed against no longer exists, see [`Self::generation`]'s doc comment). Otherwise:
    ///
    /// - `Ok(views)` caches every view the result carries, UNLESS that slot is already `Some` — a
    ///   result for an already-cached file is discarded (the loader is a pure cache-warmer, never
    ///   an overwriter; the synchronous force-completion fallback may have already filled it).
    /// - `Err(message)` is a job that panicked or otherwise failed: surfaced as a visible footer
    ///   notice (never silently stranding the file — see this changeset's report for why a footer
    ///   notice, not a new per-file `Failed` state, is the shape chosen here).
    ///
    /// Either way, when the readied file IS the current pending open, it's seated like
    /// [`Self::complete_pending_open`]'s tail — with one refinement over a plain "always clear"
    /// rule: an `Ok` result only clears the pending open when its SHAPE satisfies the current
    /// effective zoom (see [`loaded_views_satisfy`]). Without this, a zoom cycled mid-load
    /// (`z` is exempt from force-completion — [`Self::open_current`] re-defers with
    /// `open_pending_dispatched = false`) lets the stale-shaped in-flight result seat only the
    /// old view, clear the pending flags, and strand the new zoom's view forever un-dispatched.
    /// When unsatisfied, `open_pending` stays set and `open_pending_dispatched` resets to
    /// `false` so the next idle Tick re-dispatches against the NOW-current zoom — mirroring a
    /// fresh [`Self::open_current`] defer. An `Err` result keeps clearing unconditionally: a
    /// failed load must not leave the placeholder stuck forever, and correctness never depends
    /// on this path — the sync fallback owns correctness (see this method's summary above).
    pub fn apply_file_ready(
        &mut self,
        gen: u64,
        cs_idx: usize,
        file_idx: usize,
        result: Result<LoadedViews, String>,
    ) {
        if gen != self.generation {
            return;
        }
        let is_current_pending =
            cs_idx == self.current_cs && file_idx == self.current && self.open_pending;
        match result {
            Ok(views) => {
                let satisfies_current_zoom = is_current_pending
                    && loaded_views_satisfy(&views, self.effective_zoom_for(file_idx));
                if let Some(cs) = self.changesets.get_mut(cs_idx) {
                    match views {
                        LoadedViews::Single(role, view) => set_if_absent(cs, role, file_idx, view),
                        LoadedViews::Split { unstaged, staged } => {
                            set_if_absent(cs, Role::Unstaged, file_idx, unstaged);
                            set_if_absent(cs, Role::Staged, file_idx, staged);
                        }
                    }
                }
                if is_current_pending {
                    if satisfies_current_zoom {
                        self.reset_panes();
                        self.open_pending = false;
                        self.open_pending_dispatched = false;
                    } else {
                        self.open_pending_dispatched = false;
                    }
                }
            }
            Err(message) => {
                self.notify(format!("failed to load file: {message}"), Severity::Error);
                if is_current_pending {
                    self.reset_panes();
                    self.open_pending = false;
                    self.open_pending_dispatched = false;
                }
            }
        }
    }

    /// Apply one streamed-diff wave result (ADR-037's `ChangesetReady` chokepoint, the streamed-
    /// launch counterpart to [`Self::apply_file_ready`]): dropped outright on a generation
    /// mismatch, same rule and same reason (the world the wave was diffing no longer exists —
    /// e.g. a refresh ran mid-wave). Otherwise replaces changeset `idx`'s slot in place:
    ///
    /// - `Ok(diff)` builds its `Ready` [`ChangesetView`] via [`ChangesetView::from_changeset_diff`]
    ///   — the SAME router [`main.rs`'s lone-changeset sync path uses, so a streamed changeset's
    ///   `DiffState`/view caches are byte-identical to what a synchronous diff would have built.
    /// - `Err(message)` builds a `Failed` slot carrying it ([`ChangesetView::failed`]); the wave's
    ///   FIRST failure (across the whole launch, not per-changeset) raises a footer notice — see
    ///   [`Self::wave_failure_notified`]'s doc comment — and the review continues (a stack with one
    ///   corrupt changeset still shows the other N-1).
    ///
    /// When `idx` IS the active changeset (the outline cursor already sits there — either it was
    /// the lib-marked `current` changeset at launch, or the user navigated onto its still-`Pending`
    /// placeholder), it's seated exactly as a fresh open would be: `current` resets to its first
    /// file and [`Self::open_current`] runs (deferred-open semantics — CS4's placeholder shows
    /// until the file itself loads), then the outline cursor resyncs. Nothing here requires the
    /// user to navigate away and back for a just-readied active changeset to become interactive.
    pub fn apply_changeset_ready(
        &mut self,
        gen: u64,
        idx: usize,
        result: Result<ChangesetDiff, String>,
    ) {
        if gen != self.generation {
            return;
        }
        let Some(existing) = self.changesets.get(idx) else {
            return;
        };
        let cs = existing.cs.clone();
        // F3: a landed NON-active changeset inserts file rows into the outline's row list,
        // silently shifting a plain row-index cursor. Capture the identity of the row under the
        // cursor now (before the slot swap rebuilds `outline_items()`) so it can be re-found
        // afterward — the active-changeset case doesn't need this, since `sync_outline_to_current`
        // below already repositions by diff identity, not row index.
        let cursor_identity = if idx != self.current_cs {
            self.outline_items()
                .get(self.outline.cursor)
                .and_then(outline_row_identity)
        } else {
            None
        };
        match result {
            Ok(diff) => {
                self.changesets[idx] = ChangesetView::from_changeset_diff(cs, diff);
            }
            Err(message) => {
                if !self.wave_failure_notified {
                    self.notify(
                        format!("failed to diff a changeset: {message}"),
                        Severity::Error,
                    );
                    self.wave_failure_notified = true;
                }
                self.changesets[idx] = ChangesetView::failed(cs, message);
            }
        }
        if idx == self.current_cs {
            self.current = 0;
            self.open_current();
            self.sync_outline_to_current();
        } else if let Some(identity) = cursor_identity {
            let items = self.outline_items();
            if let Some(new_idx) = items
                .iter()
                .position(|it| outline_row_identity(it) == Some(identity))
            {
                self.outline.cursor = new_idx;
            } else {
                // The identified row is gone (e.g. Flat mode deduped it out) — fall back to the
                // same clamp `sync_outline_to_current` uses.
                self.outline.cursor = self.outline.cursor.min(items.len().saturating_sub(1));
            }
        }
    }

    /// Cycle the requested zoom `Split → Combined → Unstaged → Staged → Split` (`z`). The new zoom
    /// persists across file navigation; both panes reset to their first hunks so `cursor`/`scroll`
    /// are always valid for the now-active view(s).
    pub fn cycle_zoom(&mut self) {
        // A committed changeset has no staged/unstaged split to zoom into — lock zoom to combined
        // (locked decision #2) rather than cycling into a state `effective_zoom` immediately
        // collapses back anyway.
        if self.is_committed() {
            self.notify(
                "changeset is committed — combined view only",
                Severity::Info,
            );
            return;
        }
        self.zoom = match self.zoom {
            Zoom::Split => Zoom::Combined,
            Zoom::Combined => Zoom::Unstaged,
            Zoom::Unstaged => Zoom::Staged,
            Zoom::Staged => Zoom::Split,
        };
        self.open_current();
    }

    /// Set the requested zoom directly — the config-startup (CS7) counterpart to
    /// [`Self::cycle_zoom`]. Skips `cycle_zoom`'s committed-changeset guard: that guard exists
    /// only to give interactive feedback when a cycle would be a no-op, not to enforce the
    /// invariant itself — [`Self::effective_zoom_for`] (driven from [`Self::open_current`]'s
    /// `reset_panes`, which [`Self::apply_view_config`]'s caller runs right after this) already
    /// collapses a non-stageable changeset to [`Role::Combined`] regardless of the requested
    /// zoom, so setting the raw value here can never bypass the gate. Does NOT call
    /// `open_current` itself — the caller applies every CS7 setting first, then opens once.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
    }

    /// Swap focus between the two split panes (`w`) — swaps `cursor`/`scroll`/`pane_height` with
    /// the stashed unfocused pane so the existing cursor methods keep driving the focused pane, and
    /// re-derives the newly focused pane's scroll against its own (just-swapped-in) height. A no-op
    /// outside a split.
    pub fn toggle_split_focus(&mut self) {
        if self.effective_zoom_for(self.current) != EffectiveZoom::Split {
            return;
        }
        // The anchor is keyed in the currently-focused pane's coordinate space; switching panes
        // makes it meaningless, so drop the selection rather than carry a stale index across.
        self.selection_anchor = None;
        std::mem::swap(&mut self.cursor, &mut self.alt.cursor);
        std::mem::swap(&mut self.scroll, &mut self.alt.scroll);
        std::mem::swap(&mut self.pane_height, &mut self.alt_height);
        self.split_focus = match self.split_focus {
            SplitPane::Unstaged => SplitPane::Staged,
            SplitPane::Staged => SplitPane::Unstaged,
        };
        self.derive_scroll();
    }

    /// Reshape onto changeset `target` (clamped into range), landing on file `file_idx` of ITS
    /// list (clamped into range, `0` if empty) — the shared core of every changeset switch
    /// (continuous file nav crossing a boundary, and `]c`/`[c`). A coordinate-space reshape
    /// exactly like a plain file switch (cursor/scroll/selection reset via `open_current`), plus
    /// re-deriving `base_label` for the newly active changeset (each changeset can have its own
    /// base rev — see [`base_label_for`]).
    fn switch_changeset(&mut self, target: usize, file_idx: usize) {
        self.current_cs = target.min(self.changesets.len().saturating_sub(1));
        self.base_label = base_label_for(&self.cur().cs);
        let n = self.cur().diff.files.len();
        self.current = if n == 0 { 0 } else { file_idx.min(n - 1) };
        self.open_current();
    }

    /// Advance to the next file (`]f`/Tab), continuously across the whole stack (locked decision
    /// #5): at the active changeset's last file, this advances `current_cs` and lands on the NEXT
    /// changeset's first file, rather than wrapping within the active changeset. Clamps (does NOT
    /// wrap) at the very last file of the very last changeset.
    ///
    /// A DIFF-initiated nav entry point (as opposed to `switch_changeset`/`goto_changeset`, which
    /// also serve the OUTLINE's own jumps) — repositions the outline cursor to follow via
    /// [`Self::sync_outline_to_current`] at the end. This is the sync-follow discipline's echo
    /// break (see that method's doc comment): only the diff-initiated entry points call it, so an
    /// outline-initiated jump (which sets [`OutlineState::cursor`] itself before calling
    /// `switch_changeset`/`goto_changeset` directly) never re-triggers it.
    pub fn next_file(&mut self) {
        if self.cur().diff.files.is_empty() {
            return;
        }
        if self.current + 1 < self.cur().diff.files.len() {
            self.current += 1;
            self.open_current();
        } else if self.current_cs + 1 < self.changesets.len() {
            self.switch_changeset(self.current_cs + 1, 0);
        }
        // Else: already at the stack's very last file — clamp, no-op.
        self.sync_outline_to_current();
    }

    /// Retreat to the previous file (`[f`/BackTab), continuously across the whole stack — the
    /// mirror of [`Self::next_file`]. At the active changeset's first file, drops into the
    /// PREVIOUS changeset's LAST file. Clamps (does NOT wrap) at the very first file of the very
    /// first changeset. See [`Self::next_file`]'s doc comment for why this calls
    /// [`Self::sync_outline_to_current`] at the end.
    pub fn prev_file(&mut self) {
        if self.cur().diff.files.is_empty() {
            return;
        }
        if self.current > 0 {
            self.current -= 1;
            self.open_current();
        } else if self.current_cs > 0 {
            let target = self.current_cs - 1;
            let last = self.changesets[target].diff.files.len().saturating_sub(1);
            self.switch_changeset(target, last);
        }
        // Else: already at the stack's very first file — clamp, no-op.
        self.sync_outline_to_current();
    }

    /// Jump to changeset `target`'s first file (`]c`/`[c`, and the outline's own header-row
    /// jump). Clamps into `[0, changeset_count() - 1]` — never wraps. Deliberately does NOT call
    /// [`Self::sync_outline_to_current`] itself (see [`Self::next_file`]'s doc comment) — the
    /// outline's own header-row jump ([`Self::outline_confirm`]) calls it explicitly afterward
    /// instead, since this method is shared with that outline-initiated path.
    pub fn goto_changeset(&mut self, target: usize) {
        self.switch_changeset(target, 0);
    }

    /// Jump to the next changeset's first file (`]c`). A no-op at the last changeset. A
    /// DIFF-initiated entry point — see [`Self::next_file`]'s doc comment on the sync-follow
    /// discipline.
    pub fn next_changeset(&mut self) {
        if self.current_cs + 1 < self.changesets.len() {
            self.goto_changeset(self.current_cs + 1);
        }
        self.sync_outline_to_current();
    }

    /// Jump to the previous changeset's first file (`[c`). A no-op at the first changeset. See
    /// [`Self::next_file`]'s doc comment on the sync-follow discipline.
    pub fn prev_changeset(&mut self) {
        if self.current_cs > 0 {
            self.goto_changeset(self.current_cs - 1);
        }
        self.sync_outline_to_current();
    }

    // ── Outline side pane (CS3) ─────────────────────────────────────────────────

    // ── Summary panel (CS4) ─────────────────────────────────────────────────────

    /// Snapshot every reviewed changeset into [`OutlineChangeset`]/[`OutlineFile`] — the input
    /// [`Self::outline_items`] feeds `outline::build_items`, and CS4's [`Self::summary_for`]
    /// feeds `outline::latest_by_path` for a [`OutlineMode::Tree`] directory's cross-stack
    /// aggregate. Rebuilt fresh on every call, same posture as [`Self::outline_items`] itself.
    fn outline_snapshot(&self) -> Vec<OutlineChangeset> {
        self.changesets
            .iter()
            .map(|v| OutlineChangeset {
                label: v.cs.title.clone().unwrap_or_else(|| v.cs.name.clone()),
                current: v.cs.current,
                needs_restack: v.cs.needs_restack,
                loading: v.is_pending(),
                failed: v.is_failed(),
                files: v
                    .files()
                    .iter()
                    .enumerate()
                    .map(|(idx, f)| OutlineFile {
                        path: f.path.clone(),
                        status: v.staged_status(idx),
                        change: f.status,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Build the current [`OutlineMode`]'s row list — the outline cursor's index space, and the
    /// source of truth `render.rs` draws from. Rebuilt fresh on every call (cheap: a small stack
    /// times a handful of files each, no caching, same posture as [`Self::effective_zoom_for`])
    /// rather than cached on `App`, so it's never stale across a mode toggle, a nav, or a
    /// refresh.
    pub fn outline_items(&self) -> Vec<OutlineItem> {
        let snapshot = self.outline_snapshot();
        outline::build_items(&snapshot, self.outline.mode, self.outline.order)
    }

    /// CS4: the outline row a Header/Dir cursor selection resolves to — `None` when the outline
    /// isn't in a state where the diff area shows a summary instead of a file's diff (closed,
    /// merely open-but-unfocused, or the cursor is on a File row). `render_body` branches on this
    /// before any of its usual diff-body gates (pending/failed/binary/deferred-load).
    pub fn summary_target(&self) -> Option<SummaryTarget> {
        if !self.outline.open || !self.outline.focused {
            return None;
        }
        let items = self.outline_items();
        match items.get(self.outline.cursor)? {
            OutlineItem::Header { cs_idx, .. } => Some(SummaryTarget::Changeset(*cs_idx)),
            OutlineItem::Dir { path, cs_idx, .. } => Some(SummaryTarget::Dir {
                cs_idx: *cs_idx,
                path: path.clone(),
            }),
            OutlineItem::File { .. } => None,
        }
    }

    /// Build the renderable summary for `target` (see [`Self::summary_target`]) —
    /// `render::render_summary`'s data source.
    pub fn summary_for(&self, target: SummaryTarget) -> Summary {
        match target {
            SummaryTarget::Changeset(cs_idx) => {
                let view = &self.changesets[cs_idx];
                let label = view
                    .cs
                    .title
                    .clone()
                    .unwrap_or_else(|| view.cs.name.clone());
                let failure_message = view.failure_message().map(|s| s.to_string());
                Summary::Changeset(summary::changeset_summary(
                    label,
                    view.cs.current,
                    view.cs.needs_restack,
                    view.is_pending(),
                    view.is_failed(),
                    failure_message,
                    view.files(),
                ))
            }
            SummaryTarget::Dir {
                cs_idx: Some(cs_idx),
                path,
            } => {
                // StackTree mode: the dir row's trie belongs to exactly one changeset, so scope
                // the aggregate to that changeset's own files (mirrors `build_stack_tree`'s "no
                // cross-changeset dedup" rule).
                let view = &self.changesets[cs_idx];
                Summary::Dir(summary::dir_summary(
                    path,
                    &view.files().iter().collect::<Vec<_>>(),
                ))
            }
            SummaryTarget::Dir { cs_idx: None, path } => {
                // Tree mode: the dir row's trie spans the whole stack with no single owning
                // changeset — aggregate over the same last-write-wins de-duped path set the Tree
                // outline itself displays, reusing `outline::latest_by_path` rather than
                // re-deriving the dedup rule here. `latest_by_path` returns a `HashMap`, whose
                // iteration order is unspecified — sort by path so the panel's file list reads in
                // the same alpha order the Tree outline itself paints (`emit`'s own sort).
                let snapshot = self.outline_snapshot();
                let latest = outline::latest_by_path(&snapshot);
                let mut entries: Vec<(&String, &outline::FileOccurrence)> = latest.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                let files: Vec<&FileChange> = entries
                    .into_iter()
                    .filter_map(|(_, occ)| self.changesets[occ.cs_idx].files().get(occ.file_idx))
                    .collect();
                Summary::Dir(summary::dir_summary(path, &files))
            }
        }
    }

    pub fn outline_open(&self) -> bool {
        self.outline.open
    }

    pub fn outline_focused(&self) -> bool {
        self.outline.focused
    }

    pub fn outline_cursor(&self) -> usize {
        self.outline.cursor
    }

    /// Top-of-viewport row index into [`Self::outline_items`]'s row list — see
    /// [`Self::derive_outline_scroll`].
    pub fn outline_scroll(&self) -> usize {
        self.outline.scroll
    }

    /// The outline pane's column width — `workon.review.outline.width` (CS7), or
    /// [`DEFAULT_OUTLINE_WIDTH`] if never set. Read by `render.rs` in place of the old fixed
    /// const.
    pub fn outline_width(&self) -> u16 {
        self.outline.width
    }

    pub fn outline_mode(&self) -> OutlineMode {
        self.outline.mode
    }

    /// Which end of the stack the outline displays first — `workon.review.outline.order` (CS3),
    /// or [`OutlineOrder::default`] if never set.
    pub fn outline_order(&self) -> OutlineOrder {
        self.outline.order
    }

    /// The nerd-font iconography mode — `workon.review.icons`, or [`IconMode::default`]
    /// (`None`) if never set. TUI-wide: read by the outline, summary panel, and winbar renderers.
    pub fn icon_mode(&self) -> IconMode {
        self.icon_mode
    }

    /// `o`: a pure show/hide toggle — closed -> open+focused (+[`Self::sync_outline_to_current`]),
    /// open (regardless of focus) -> closed+diff-focused. Focus itself is now a separate concern
    /// handled by [`Self::focus_outline`]/[`Self::focus_diff`] (`h`/`l`) — `o` only ever changes
    /// visibility. The opening arm IS `focus_outline`'s closed-case behavior, so it delegates
    /// there rather than restating it.
    pub fn toggle_outline(&mut self) {
        if !self.outline.open {
            self.focus_outline();
        } else {
            self.outline.open = false;
            self.outline.focused = false;
        }
    }

    /// `h`/Esc-cascade target: focus the outline, opening it first if it's closed. Syncing the
    /// cursor to the current diff position only happens on the closed -> open transition — if the
    /// outline is already open, re-focusing it (e.g. `h` after a manual `j`/`k` outline move
    /// followed by `l`) must not stomp a manually positioned cursor.
    pub fn focus_outline(&mut self) {
        if !self.outline.open {
            self.outline.open = true;
            self.sync_outline_to_current();
        }
        self.outline.focused = true;
    }

    /// `l`/Enter: return focus to the diff. The outline stays open — this only ever changes
    /// focus, never visibility (that's `o`/[`Self::toggle_outline`]'s job).
    pub fn focus_diff(&mut self) {
        self.outline.focused = false;
    }

    // ── Mouse (CS10) ─────────────────────────────────────────────────────────────

    /// Hit-test `(col, row)` against [`Self::hit_regions`] — outline first, then the single diff
    /// pane, then the split's two halves — returning the matched region tagged with which
    /// [`HitPane`] it was. `None` when the pointer is over a header/footer/divider/caption row
    /// (recorded regions cover content only).
    fn hit_test(&self, col: u16, row: u16) -> Option<(HitPane, Region)> {
        if let Some(region) = self.hit_regions.outline {
            if region.contains(col, row) {
                return Some((HitPane::Outline, region));
            }
        }
        if let Some(region) = self.hit_regions.single {
            if region.contains(col, row) {
                return Some((HitPane::Single, region));
            }
        }
        if let Some(region) = self.hit_regions.unstaged {
            if region.contains(col, row) {
                return Some((HitPane::Split(SplitPane::Unstaged), region));
            }
        }
        if let Some(region) = self.hit_regions.staged {
            if region.contains(col, row) {
                return Some((HitPane::Split(SplitPane::Staged), region));
            }
        }
        None
    }

    /// Focus the diff pane a click/wheel landed in, mirroring the keyboard focus rules: if the
    /// outline had focus, `focus_diff()` moves focus onto whichever split pane already has it; if
    /// the event landed in the OTHER split pane, `toggle_split_focus()` flips onto it next (never
    /// assigning `split_focus` directly — see that method's doc comment). `target` is `None` for
    /// the single-zoom pane, where there is no second half to flip to.
    fn focus_diff_pane(&mut self, target: Option<SplitPane>) {
        if self.outline_focused() {
            self.focus_diff();
        }
        if let Some(target) = target {
            if self.split_focus != target {
                self.toggle_split_focus();
            }
        }
    }

    /// Set the (now-focused) pane's cursor to the row under a click, offset from `region`'s top by
    /// `row` and clamped into the current row list, then re-derive `scroll`. A no-op on an empty
    /// file list, matching [`Self::move_cursor_by`]'s empty-list behavior.
    fn set_cursor_from_click(&mut self, region: Region, row: u16) {
        let rows = self.row_count();
        if rows == 0 {
            return;
        }
        let offset = (row - region.y) as usize;
        self.cursor = (self.scroll + offset).min(rows - 1);
        self.derive_scroll();
    }

    /// Left-click at terminal `(col, row)` (CS10): focus + select whatever content region the
    /// click landed in, matching the keyboard-driven equivalent for that region. Outline: focuses
    /// the outline and jumps the cursor to the clicked row via [`Self::outline_move_to`] — a File
    /// row jumps the diff there (same single-jump semantics `g`/`G` use), a Header/Dir row just
    /// selects (the summary panel follows via [`Self::summary_target`]). Diff pane (single or
    /// split): focuses that pane (flipping `split_focus` first if the click landed in the
    /// unfocused half) and moves its cursor to the clicked row. Outside every recorded region
    /// (header/footer/divider/captions): no-op.
    pub fn handle_click(&mut self, col: u16, row: u16) {
        let Some((pane, region)) = self.hit_test(col, row) else {
            return;
        };
        match pane {
            HitPane::Outline => {
                self.focus_outline();
                let idx = self.outline.scroll + (row - region.y) as usize;
                self.outline_move_to(idx);
            }
            HitPane::Single => {
                self.focus_diff_pane(None);
                self.set_cursor_from_click(region, row);
            }
            HitPane::Split(target) => {
                self.focus_diff_pane(Some(target));
                self.set_cursor_from_click(region, row);
            }
        }
    }

    /// Mouse wheel at terminal `(col, row)` with `delta` = ±3 rows (`tui::update` maps
    /// `ScrollDown`/`ScrollUp` to +3/-3). Focuses whichever region the pointer sits over first —
    /// same rule as [`Self::handle_click`] — then moves that pane's cursor by `delta`
    /// ([`Self::outline_move_by`] for the outline, [`Self::move_cursor_by`] otherwise); scroll
    /// simply follows via the normal derive discipline rather than a decoupled scroll state.
    /// Outside every recorded region: no-op.
    pub fn handle_wheel(&mut self, col: u16, row: u16, delta: i64) {
        let Some((pane, _region)) = self.hit_test(col, row) else {
            return;
        };
        match pane {
            HitPane::Outline => {
                self.focus_outline();
                self.outline_move_by(delta);
            }
            HitPane::Single => {
                self.focus_diff_pane(None);
                self.move_cursor_by(delta);
            }
            HitPane::Split(target) => {
                self.focus_diff_pane(Some(target));
                self.move_cursor_by(delta);
            }
        }
    }

    /// `?`: toggle the help overlay (CS3). A plain flip — the overlay always renders whatever
    /// view currently has keyboard focus (see `render::render_help_overlay`), so there is no
    /// extra state to reposition here, unlike [`Self::toggle_outline`].
    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    /// `i` while the outline has focus: cycle [`OutlineMode`], then reposition the cursor onto
    /// the row matching the current diff position in the NEW mode's row list (the row layout
    /// just changed shape, so the raw index would otherwise point at an unrelated row).
    pub fn outline_cycle_mode(&mut self) {
        self.outline.mode = self.outline.mode.cycle();
        self.sync_outline_to_current();
    }

    /// Set the outline pane width directly (CS7: `workon.review.outline.width`, applied by
    /// [`Self::apply_view_config`] at startup — there's no interactive key for this today). The
    /// caller is responsible for clamping into `[MIN_OUTLINE_WIDTH, MAX_OUTLINE_WIDTH]`
    /// (`apply_view_config` does); this setter trusts its input.
    pub fn set_outline_width(&mut self, width: u16) {
        self.outline.width = width;
    }

    /// Set the outline mode directly — the config-startup (CS7) counterpart to
    /// [`Self::outline_cycle_mode`]. Unlike the interactive cycle, this does NOT call
    /// [`Self::sync_outline_to_current`]: [`Self::apply_view_config`] runs before the first
    /// [`Self::open_current`], matching how [`Self::from_changesets`] seeds
    /// [`OutlineState::mode`] today (the outline cursor starts at `0` either way).
    pub fn set_outline_mode(&mut self, mode: OutlineMode) {
        self.outline.mode = mode;
    }

    /// Set the outline stack order directly — the config-startup (CS3) counterpart there is no
    /// interactive key for today. Same non-resync posture as [`Self::set_outline_mode`]: called
    /// before the first [`Self::open_current`], so no [`Self::sync_outline_to_current`] call is
    /// needed here either.
    pub fn set_outline_order(&mut self, order: OutlineOrder) {
        self.outline.order = order;
    }

    /// Set the icon mode directly — the config-startup counterpart; there is no interactive
    /// key for this (icons are a static config choice, not something to toggle mid-session).
    /// Same non-resync posture as [`Self::set_outline_mode`]/[`Self::set_outline_order`].
    pub fn set_icon_mode(&mut self, icons: IconMode) {
        self.icon_mode = icons;
    }

    /// Move the outline's own cursor by `delta` rows (`j`/`k` while the outline has focus),
    /// clamped into the current row list. Landing on a FILE row jumps the diff there
    /// immediately (outline -> diff, per the locked design); a HEADER/DIR row itself never
    /// causes a jump — only [`Self::outline_confirm`] (`Enter`) jumps from a header, since a
    /// header's "first file" isn't necessarily where a `j`/`k` scan through the stack should
    /// keep stopping the diff. This calls [`Self::switch_changeset`] directly (not
    /// `next_file`/`goto_changeset`), so it does NOT re-trigger
    /// [`Self::sync_outline_to_current`] — see that method's doc comment for why only the
    /// DIFF-initiated entry points do.
    ///
    /// A multi-row `delta` is a coalesced burst of unit presses (the event loop merges
    /// same-sign `j`/`k` runs — see `tui.rs`'s `update_batch`), so it must be
    /// indistinguishable from the unit presses it stands for: N unit moves jump the diff at
    /// every FILE row they cross, leaving it on the LAST one when the run stops on a
    /// header/dir row. So a non-File landing scans back toward (but excluding) the starting
    /// row for the last file crossed and jumps there. For a unit move that range is empty,
    /// preserving the single-press rule above: bare `j`/`k` onto a header neither jumps nor
    /// resets the diff.
    pub fn outline_move_by(&mut self, delta: i64) {
        let items = self.outline_items();
        if items.is_empty() {
            self.outline.cursor = 0;
            return;
        }
        let max = (items.len() - 1) as i64;
        let cur = self.outline.cursor as i64;
        let new_idx = (cur + delta).clamp(0, max) as usize;
        self.outline.cursor = new_idx;
        if let OutlineItem::File {
            cs_idx, file_idx, ..
        } = &items[new_idx]
        {
            self.switch_changeset(*cs_idx, *file_idx);
        } else if new_idx as i64 != cur {
            let step = if delta > 0 { -1 } else { 1 };
            let mut idx = new_idx as i64 + step;
            while idx != cur && (0..=max).contains(&idx) {
                if let OutlineItem::File {
                    cs_idx, file_idx, ..
                } = &items[idx as usize]
                {
                    self.switch_changeset(*cs_idx, *file_idx);
                    break;
                }
                idx += step;
            }
        }
        self.derive_outline_scroll(items.len());
    }

    /// `g`/`G` while the outline has focus: jump the cursor straight to row `idx` (clamped into
    /// the current row list), landing on it in one step — unlike [`Self::outline_move_by`], there
    /// is NO burst back-scan here: a jump to a HEADER/DIR row simply doesn't move the diff (`g`
    /// typically lands on the stack's first header), and a jump to a FILE row jumps the diff
    /// straight there (`G` typically lands on the last file). Used by [`Self::outline_top`]/
    /// [`Self::outline_bottom`].
    fn outline_move_to(&mut self, idx: usize) {
        let items = self.outline_items();
        if items.is_empty() {
            self.outline.cursor = 0;
            self.derive_outline_scroll(0);
            return;
        }
        let idx = idx.min(items.len() - 1);
        self.outline.cursor = idx;
        if let OutlineItem::File {
            cs_idx, file_idx, ..
        } = &items[idx]
        {
            self.switch_changeset(*cs_idx, *file_idx);
        }
        self.derive_outline_scroll(items.len());
    }

    /// `g` while the outline has focus: jump the cursor to the first row.
    pub fn outline_top(&mut self) {
        self.outline_move_to(0);
    }

    /// `G` while the outline has focus: jump the cursor to the last row.
    pub fn outline_bottom(&mut self) {
        let last = self.outline_items().len().saturating_sub(1);
        self.outline_move_to(last);
    }

    /// `Enter` while the outline has focus: jump the diff to the row under the outline cursor (a
    /// file row jumps straight there; a header row jumps to that changeset's first file — the
    /// one case [`Self::outline_move_by`] deliberately does NOT do on a bare cursor move), then
    /// return focus to the diff.
    pub fn outline_confirm(&mut self) {
        let items = self.outline_items();
        match items.get(self.outline.cursor) {
            Some(OutlineItem::File {
                cs_idx, file_idx, ..
            }) => self.switch_changeset(*cs_idx, *file_idx),
            Some(OutlineItem::Header { cs_idx, .. }) => {
                let cs_idx = *cs_idx;
                self.goto_changeset(cs_idx);
                // `goto_changeset` is the shared outline/diff core and deliberately does not
                // self-sync (see its doc comment) — this outline-initiated call syncs explicitly
                // so the cursor follows off the header row onto the file it just jumped to.
                self.sync_outline_to_current();
            }
            // A directory row (Tree/StackTree modes) is not a jump target — no expand/collapse
            // state exists to toggle (CS4 decision), so Enter here is a no-op beyond the
            // unconditional unfocus below, same as confirming on nothing at all.
            Some(OutlineItem::Dir { .. }) | None => {}
        }
        self.outline.focused = false;
    }

    // ── Outline staging (CS7) ───────────────────────────────────────────────────

    /// Whether the changeset at `cs_idx` is a committed range rather than the uncommitted
    /// worktree layer — the per-index counterpart to [`Self::is_committed`] (which only reads the
    /// ACTIVE changeset). CS7's outline verbs need this because the acted-on row's changeset is
    /// whichever one the outline cursor rests on, not necessarily the diff's current changeset.
    fn is_committed_at(&self, cs_idx: usize) -> bool {
        self.changesets.get(cs_idx).is_some_and(|view| {
            matches!(
                view.cs.span,
                ChangesetSpan::Committed { .. } | ChangesetSpan::CommittedRoot { .. }
            )
        })
    }

    /// Resolve the outline row at `idx` to its [`OutlineRowIdentity`] plus the `(cs_idx,
    /// file_idx)` pairs an outline stage/discard verb applies to — `None` for a
    /// [`OutlineItem::Header`] row (never a staging target) or an out-of-range `idx`.
    ///
    /// A [`OutlineItem::File`] row resolves to its own single target. A [`OutlineItem::Dir`] row
    /// resolves to every file under its `path` (segment-boundary match, [`summary::path_is_under`]
    /// — the same rule the summary panel's [`summary::dir_summary`] uses): scoped to that row's own
    /// changeset in [`OutlineMode::StackTree`] (`cs_idx: Some`), or to the cross-stack
    /// last-write-wins de-duped set [`outline::latest_by_path`] returns in [`OutlineMode::Tree`]
    /// (`cs_idx: None`) — mirrors [`Self::summary_for`]'s own Dir-row branching.
    fn outline_row_targets(&self, idx: usize) -> Option<(OutlineRowIdentity, Vec<(usize, usize)>)> {
        let items = self.outline_items();
        match items.get(idx)? {
            OutlineItem::Header { .. } => None,
            OutlineItem::File {
                cs_idx, file_idx, ..
            } => {
                let path = self
                    .changesets
                    .get(*cs_idx)?
                    .files()
                    .get(*file_idx)?
                    .path
                    .clone();
                Some((
                    OutlineRowIdentity::File {
                        cs_idx: *cs_idx,
                        path,
                    },
                    vec![(*cs_idx, *file_idx)],
                ))
            }
            OutlineItem::Dir { path, cs_idx, .. } => {
                let identity = OutlineRowIdentity::Dir {
                    cs_idx: *cs_idx,
                    path: path.clone(),
                };
                let targets = match cs_idx {
                    Some(cs_idx) => self
                        .changesets
                        .get(*cs_idx)?
                        .files()
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| summary::path_is_under(&f.path, path))
                        .map(|(file_idx, _)| (*cs_idx, file_idx))
                        .collect(),
                    None => {
                        let snapshot = self.outline_snapshot();
                        let latest = outline::latest_by_path(&snapshot);
                        latest
                            .iter()
                            .filter(|(p, _)| summary::path_is_under(p, path))
                            .map(|(_, occ)| (occ.cs_idx, occ.file_idx))
                            .collect()
                    }
                };
                Some((identity, targets))
            }
        }
    }

    /// Per-file verb selection by [`outline::StagedStatus`] — mirrors [`Self::verb_for_role`]'s
    /// toggle direction (unstaged stages, staged unstages), but keyed off the FILE's own status
    /// rather than a pane role, since a Dir row's files can each carry a different status.
    /// [`outline::StagedStatus::None`] shouldn't normally occur on the uncommitted changeset's own
    /// file (a changed file always has SOME status) — treated as a Stage attempt so the op surfaces
    /// whatever git reports rather than silently refusing.
    fn outline_target_verb(&self, cs_idx: usize, file_idx: usize) -> StageVerb {
        match self.changesets[cs_idx].staged_status(file_idx) {
            outline::StagedStatus::Staged => StageVerb::Unstage,
            outline::StagedStatus::Unstaged
            | outline::StagedStatus::Partial
            | outline::StagedStatus::None => StageVerb::Stage,
        }
    }

    /// Footer refusal for an outline stage/discard verb — parallels [`Self::notify_combined_refusal`]
    /// but for the two CS7-specific refusal reasons: `committed` (the row's changeset — or, for a
    /// Dir row, at least one file under it — is a committed range, not the uncommitted worktree
    /// layer) or not (the cursor sits on a [`OutlineItem::Header`] row, which is never a target).
    fn notify_outline_refusal(&mut self, verb: &str, committed: bool) {
        if committed {
            self.notify(
                format!("changeset is already committed — nothing to {verb}"),
                Severity::Error,
            );
        } else {
            self.notify(
                format!("select a file or directory to {verb}"),
                Severity::Error,
            );
        }
    }

    /// The shared resolve-and-gate preamble of the outline staging verbs (`s`/`d`): resolve the
    /// row under the outline cursor to its identity + targets, refusing (with `verb` naming the
    /// action in the notice) on a Header row or when any target belongs to a committed changeset,
    /// and bailing silently on an empty target list. One helper so the two verbs' gates can't
    /// drift apart.
    fn outline_verb_targets(
        &mut self,
        verb: &str,
    ) -> Option<(OutlineRowIdentity, Vec<(usize, usize)>)> {
        let Some((identity, targets)) = self.outline_row_targets(self.outline.cursor) else {
            self.notify_outline_refusal(verb, false);
            return None;
        };
        if targets
            .iter()
            .any(|&(cs_idx, _)| self.is_committed_at(cs_idx))
        {
            self.notify_outline_refusal(verb, true);
            return None;
        }
        if targets.is_empty() {
            return None;
        }
        Some((identity, targets))
    }

    /// `s` while the outline has focus: stage or unstage the file/directory under the cursor. A
    /// [`OutlineItem::File`] row stages or unstages per its own [`Self::outline_target_verb`]; a
    /// [`OutlineItem::Dir`] row applies the same per-file verb selection to every file under it
    /// (each file stages or unstages independently — a mixed-status directory is not an all-stage
    /// or all-unstage op). Refuses on a [`OutlineItem::Header`] row or when any target belongs to
    /// a committed changeset (see [`Self::notify_outline_refusal`]).
    pub fn outline_stage(&mut self) {
        let Some((identity, targets)) = self.outline_verb_targets("stage") else {
            return;
        };
        let ops: Vec<Box<dyn StagingOp>> = targets
            .iter()
            .filter_map(|&(cs_idx, file_idx)| {
                let file = self.changesets.get(cs_idx)?.files().get(file_idx)?.clone();
                let verb = self.outline_target_verb(cs_idx, file_idx);
                Some(Box::new(FileStagingOp::file(file, verb)) as Box<dyn StagingOp>)
            })
            .collect();
        self.outline_run_ops(ops, identity);
    }

    /// `d` while the outline has focus: request confirmation to discard the file/directory under
    /// the cursor from the worktree — a [`OutlineItem::File`] row discards just that file; a
    /// [`OutlineItem::Dir`] row discards every file under it, and the confirm prompt names the
    /// scope. Same refusal gates as [`Self::outline_stage`]. The discard itself runs when the user
    /// answers `y` (see [`Self::resolve_confirm`]'s [`PendingOp::DiscardOutlineFiles`] arm).
    pub fn outline_discard(&mut self) {
        let Some((identity, targets)) = self.outline_verb_targets("discard") else {
            return;
        };
        let prompt = match &identity {
            OutlineRowIdentity::File { path, .. } => {
                format!("Discard all changes to `{path}`? (y/n)")
            }
            OutlineRowIdentity::Dir { path, .. } => format!(
                "Discard changes to {} files under {path}/? (y/n)",
                targets.len()
            ),
        };
        let files: Vec<(String, String)> = targets
            .iter()
            .filter_map(|&(cs_idx, file_idx)| {
                let view = self.changesets.get(cs_idx)?;
                let path = view.files().get(file_idx)?.path.clone();
                Some((view.cs.name.clone(), path))
            })
            .collect();
        self.request_confirm(prompt, PendingOp::DiscardOutlineFiles { files, identity });
    }

    /// The outline-facing counterpart to [`Self::run_op`]: drain `ops` through [`Self::run_ops`],
    /// then restore the OUTLINE cursor to (or nearest to) `identity`'s row rather
    /// than a diff-pane position (CS6's [`PositionMemento`]/[`Self::restore_position`] only make
    /// sense when the diff pane, not the outline, was the focused surface the op started from).
    /// [`Self::coordinated_refresh`] (inside `run_ops`) itself calls `sync_outline_to_current`,
    /// which can leave the outline cursor on a wholly unrelated row (wherever the DIFF's current
    /// file happens to be) — this runs after that and overwrites it with the acted-on row's own
    /// position, or the nearest surviving row if it's gone (e.g. a fully-discarded file).
    fn outline_run_ops(&mut self, ops: Vec<Box<dyn StagingOp>>, identity: OutlineRowIdentity) {
        let pre_op_cursor = self.outline.cursor;
        // Restore after BOTH outcomes: `run_ops` refreshes (and thereby yanks the outline cursor
        // via `sync_outline_to_current`) even on a partial failure, and the acted-on row is where
        // the user is looking either way.
        let _ = self.run_ops(ops);
        self.restore_outline_position(&identity, pre_op_cursor);
    }

    /// Re-find `identity`'s row in the freshly rebuilt [`Self::outline_items`] and reseat
    /// [`OutlineState::cursor`] there; clamps into bounds instead when the row is gone (a fully
    /// discarded file drops out of the combined diff — and with it its row — entirely). Does not
    /// touch [`OutlineState::focused`] — an outline-initiated op
    /// only ever runs while the outline already has focus, and nothing here changes that.
    fn restore_outline_position(&mut self, identity: &OutlineRowIdentity, pre_op_cursor: usize) {
        let items = self.outline_items();
        let found = items.iter().position(|item| match item {
            OutlineItem::File {
                cs_idx, file_idx, ..
            } => {
                let full_path = self
                    .changesets
                    .get(*cs_idx)
                    .and_then(|v| v.files().get(*file_idx))
                    .map(|f| f.path.as_str());
                full_path.is_some_and(|p| identity.matches_file(*cs_idx, p))
            }
            OutlineItem::Dir { .. } => identity.matches_dir(item),
            OutlineItem::Header { .. } => false,
        });
        match found {
            Some(idx) => self.outline.cursor = idx,
            // Row gone (the NORMAL outcome of a successful discard — the file left the combined
            // diff and took its row with it): stay near where the user was ACTING, not wherever
            // the refresh's `sync_outline_to_current` just parked the cursor (the diff's current
            // file, unrelated to the acted-on row). `pre_op_cursor` is the acted-on row's own
            // pre-op position; clamping it lands on the nearest surviving neighbor.
            None => self.outline.cursor = pre_op_cursor.min(items.len().saturating_sub(1)),
        }
        self.derive_outline_scroll(items.len());
    }

    /// Reposition (never rebuild/refocus) the outline cursor onto the row matching the CURRENT
    /// diff changeset+file, or clamp it into bounds if no such row exists (e.g. Flat mode
    /// deduped the current file's changeset out of the list). The sync-follow discipline's echo
    /// break: called ONLY from the diff-initiated nav entry points (`next_file`/`prev_file`/
    /// `next_changeset`/`prev_changeset`/`refresh`, plus the two outline actions that explicitly
    /// opt in after a header jump) — never from `switch_changeset`/`goto_changeset` themselves,
    /// since those are the shared core an OUTLINE-initiated jump also calls, and an
    /// outline-initiated jump has already set [`OutlineState::cursor`] to the row the user
    /// selected. If this ran unconditionally inside `switch_changeset`, an outline `j`/`k` move
    /// past a HEADER row (which never calls `switch_changeset`, so nothing would resync) would
    /// be fine, but any accidental future call site wired into the shared core would instantly
    /// stomp a manually-positioned outline cursor back onto the diff's last position — the exact
    /// oscillation the prototype's `_suppress_sync` flag existed to prevent. Keeping the sync
    /// calls only at the diff-facing entry points achieves the same break without needing a
    /// mutable suppression flag on `App`.
    fn sync_outline_to_current(&mut self) {
        let items = self.outline_items();
        if items.is_empty() {
            self.outline.cursor = 0;
            self.derive_outline_scroll(0);
            return;
        }
        if let Some(idx) = items.iter().position(|it| {
            matches!(
                it,
                OutlineItem::File { cs_idx, file_idx, .. }
                    if *cs_idx == self.current_cs && *file_idx == self.current
            )
        }) {
            self.outline.cursor = idx;
        } else {
            self.outline.cursor = self.outline.cursor.min(items.len() - 1);
        }
        self.derive_outline_scroll(items.len());
    }

    /// Row count of file `idx`'s `role` view in the active layout's space (0 if absent/unloaded).
    fn role_row_count(&self, idx: usize, role: Role) -> usize {
        self.role_view_ref(idx, role)
            .map(|v| match self.layout {
                Layout::Sbs => v.display.len(),
                Layout::Inline => v.inline.len(),
            })
            .unwrap_or(0)
    }

    fn row_count(&self) -> usize {
        self.role_row_count(self.current, self.focused_role_for(self.current))
    }

    /// Max `scroll` value keeping the last row reachable — the focused pane's row count minus its
    /// height. Now that scroll derivation lives in [`derive_scroll_value`], this survives only as a
    /// bound the scroll tests assert against.
    #[cfg(test)]
    fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.pane_height.max(1))
    }

    /// Clamp `cursor` into `[0, row_count() - 1]` (or `0` for an empty row list) — used after an
    /// operation that can leave `cursor` referring to a row the active layout/file no longer has
    /// (a layout toggle, most notably; see [`Self::toggle_layout`]).
    fn clamp_cursor(&mut self) {
        let rows = self.row_count();
        self.cursor = if rows == 0 {
            0
        } else {
            self.cursor.min(rows - 1)
        };
    }

    /// Re-derive the FOCUSED pane's `scroll` from its `cursor` so the cursor stays visible. See
    /// [`derive_scroll_value`] for the margin/edge behavior; every cursor-moving method ends by
    /// calling this — `scroll` is otherwise never written directly. `pub(crate)` so the split
    /// renderer can re-derive the focused pane's scroll once it knows the (render-time-only) pane
    /// height.
    pub(crate) fn derive_scroll(&mut self) {
        let rows = self.row_count();
        self.scroll = derive_scroll_value(self.cursor, self.scroll, rows, self.pane_height);
    }

    /// Re-derive the UNFOCUSED split pane's scroll against its own cursor, row count, and
    /// [`Self::alt_height`] — called by the renderer each split frame, after the pane heights are
    /// known.
    pub(crate) fn derive_alt_scroll(&mut self) {
        let role = self.unfocused_split_role();
        let rows = self.role_row_count(self.current, role);
        self.alt.scroll =
            derive_scroll_value(self.alt.cursor, self.alt.scroll, rows, self.alt_height);
    }

    /// Re-derive the outline pane's `scroll` from its `cursor` — the outline's counterpart to
    /// [`Self::derive_scroll`], reusing the same [`derive_scroll_value`] core against
    /// [`Self::outline_height`]. Called after every outline-cursor mutation (mirroring how every
    /// diff-cursor mutator ends with `derive_scroll`); the renderer also re-derives each frame,
    /// which covers resizes. Takes the outline row count from the caller — every call site has
    /// just built (or is about to paint from) [`Self::outline_items`], and rebuilding the whole
    /// snapshot here again just for `.len()` would double the work on every keypress and frame.
    pub(crate) fn derive_outline_scroll(&mut self, rows: usize) {
        self.outline.scroll = derive_scroll_value(
            self.outline.cursor,
            self.outline.scroll,
            rows,
            self.outline_height,
        );
    }

    /// The `(scroll, cursor)` a split pane renders with: the focused pane contributes its own
    /// `scroll` and `Some(cursor)` (so the cursor highlight draws there); the unfocused pane
    /// contributes its stashed scroll and `None` (no highlight). Combined resolves to the focused
    /// (single) state.
    pub(crate) fn pane_render_state(&self, role: Role) -> (usize, Option<usize>) {
        let pane = match role {
            Role::Unstaged => SplitPane::Unstaged,
            Role::Staged => SplitPane::Staged,
            Role::Combined => return (self.scroll, Some(self.cursor)),
        };
        if self.split_focus == pane {
            (self.scroll, Some(self.cursor))
        } else {
            (self.alt.scroll, None)
        }
    }

    /// Move the cursor by `delta` rows, clamped to `[0, row_count() - 1]` (a no-op on an empty
    /// file list), then re-derive `scroll`. Drives `j`/`k`/arrows (`delta = ±1`) and
    /// `Ctrl-d`/`Ctrl-u` (`delta = ±pane_height/2`).
    pub fn move_cursor_by(&mut self, delta: i64) {
        let rows = self.row_count();
        if rows == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let max = (rows - 1) as i64;
        let cur = self.cursor as i64;
        self.cursor = (cur + delta).clamp(0, max) as usize;
        self.derive_scroll();
    }

    /// Move the cursor to row 0 (`g`) and re-derive `scroll`.
    pub fn scroll_top(&mut self) {
        self.cursor = 0;
        self.derive_scroll();
    }

    /// Move the cursor to the last row (`G`) and re-derive `scroll`.
    pub fn scroll_bottom(&mut self) {
        let rows = self.row_count();
        self.cursor = rows.saturating_sub(1);
        self.derive_scroll();
    }

    /// Move the cursor to the next hunk-start row after its current position (`]h`), then
    /// re-derive `scroll`. A no-op if there is no later hunk, or the current file has no loaded
    /// view. Searches [`Self::layout`]'s own row vector and coordinate space — `cursor` is an
    /// index into `display` under [`Layout::Sbs`] but into `inline` under [`Layout::Inline`], and
    /// the two disagree on row count/position whenever a change block has unequal del/add
    /// counts.
    pub fn next_hunk_row(&mut self) {
        let Some(view) = self.current_view_ref() else {
            return;
        };
        let next = match self.layout {
            Layout::Sbs => find_next_hunk_row(&view.display, self.cursor),
            Layout::Inline => find_next_inline_hunk_row(&view.inline, self.cursor),
        };
        if let Some(row) = next {
            self.cursor = row;
            self.derive_scroll();
        }
    }

    /// Move the cursor to the previous hunk-start row before its current position (`[h`), then
    /// re-derive `scroll`. A no-op if there is no earlier hunk, or the current file has no loaded
    /// view. See [`Self::next_hunk_row`] for why the search dispatches on [`Self::layout`].
    pub fn prev_hunk_row(&mut self) {
        let Some(view) = self.current_view_ref() else {
            return;
        };
        let prev = match self.layout {
            Layout::Sbs => find_prev_hunk_row(&view.display, self.cursor),
            Layout::Inline => find_prev_inline_hunk_row(&view.inline, self.cursor),
        };
        if let Some(row) = prev {
            self.cursor = row;
            self.derive_scroll();
        }
    }

    /// Reveal more of the collapsed gap under the cursor (`Enter`), or the WHOLE gap (`E`, when
    /// `full`) — CS8's progressive unfold, extended by CS9 with a two-tier `Enter`: A silent
    /// no-op when the cursor isn't on a `Gap` row (or there's no loaded view): unlike a staging
    /// refusal this isn't a mode error worth interrupting the user over, same precedent as
    /// [`Self::next_hunk_row`] finding no later hunk.
    ///
    /// - `full` (`E`): unchanged from CS8 — always the flat full-run reveal via
    ///   [`FileView::expand_gap`], regardless of grammar.
    /// - `!full` (`Enter`, CS9): FIRST tries a tree-sitter scope-reveal —
    ///   [`gap_scope_start`] resolves the gap's anchor (the following row's new-side lineno,
    ///   preferring new like CS6's [`Self::restore_position`], old-side for delete-only files)
    ///   to the smallest enclosing [`crate::scope`] node, and [`FileView::scope_expand_gap`]
    ///   widens the gap's trailing edge to uncover it. Falls back to the flat +10/+10 reveal
    ///   (same as CS8) when: the file's extension has no bundled grammar, no allowlisted
    ///   ancestor encloses the anchor, or the scope reveals nothing new (already fully visible)
    ///   — so repeated `Enter` presses always widen the gap, uniformly.
    ///
    /// `self.cursor`'s INDEX is left untouched either way. Rows revealed at the gap's leading
    /// edge insert immediately before the gap's own row (shifting the gap marker — and
    /// everything after it — down), so after [`FileView::rebuild_rows`] the row now sitting at
    /// the old index is the first newly revealed line rather than the gap marker itself: the
    /// cursor visually lands on the start of the revealed region without this method needing to
    /// compute a new index. The scope-reveal path only ever widens the TRAILING edge (see
    /// [`FileView::scope_expand_gap`]'s doc for why), so this holds there too.
    pub fn expand_gap_at_cursor(&mut self, full: bool) {
        let cursor = self.cursor;
        let layout = self.layout;
        // Read out before taking `current_view()`'s exclusive borrow — `gap_scope_start` only
        // needs the path strings, not the file, so cloning two short `String`s here avoids a
        // `self.cur()`/`self.current_view()` borrow conflict for the whole rest of the method.
        let anchor_paths = self.cur().diff.files.get(self.current).map(|f| {
            let new_path = f.path.clone();
            let old_path = f.old_path.clone().unwrap_or_else(|| f.path.clone());
            (new_path, old_path)
        });
        let Some(view) = self.current_view() else {
            return;
        };
        let key = match layout {
            Layout::Sbs => match view.display.get(cursor) {
                Some(DisplayRow::Gap { key, .. }) => *key,
                _ => return,
            },
            Layout::Inline => match view.inline.get(cursor) {
                Some(InlineRow::Gap { key, .. }) => *key,
                _ => return,
            },
        };

        let scope_revealed = !full
            && anchor_paths
                .as_ref()
                .and_then(|(new_path, old_path)| {
                    gap_scope_start(view, layout, cursor, new_path, old_path)
                })
                .is_some_and(|(scope_start, anchor_prefers_new)| {
                    view.scope_expand_gap(key, scope_start, anchor_prefers_new)
                });

        if !scope_revealed {
            view.expand_gap(key, 10, 10, full);
        }
        // The expansion just reshaped the focused pane's row space — whichever tier did it —
        // so cancel any active selection rather than translating it, per `selection_anchor`'s
        // invariant (same rule as layout toggles, zoom changes, file switches, and split-focus
        // swaps). Only reached when a gap actually expanded; the non-gap no-op above leaves a
        // selection alone.
        self.cancel_selection();
        self.derive_scroll();
        self.clamp_cursor();
    }

    /// Toggle between side-by-side and inline layouts (`L`). Deliberately does not try to
    /// re-derive an exactly equivalent `cursor` position for the new layout — the two layouts'
    /// row vectors track the same underlying content in a different shape, and translating
    /// exactly isn't worth the complexity for M4; the user re-orients same as they would after a
    /// resize. It DOES clamp `cursor` to the new layout's `row_count()` (see
    /// [`Self::clamp_cursor`]) and re-derive `scroll` from it, so the result is always a valid,
    /// visible position even though it isn't a semantic equivalent of the old one.
    pub fn toggle_layout(&mut self) {
        self.layout = match self.layout {
            Layout::Sbs => Layout::Inline,
            Layout::Inline => Layout::Sbs,
        };
        // The two layouts' row vectors are different coordinate spaces (a paired del/add is one
        // SBS row but two inline rows), so a selection anchor doesn't translate — cancel it, the
        // simplest defensible choice (locked decision #8's "press L for per-side precision" flow
        // starts a fresh selection anyway).
        self.selection_anchor = None;
        self.clamp_cursor();
        // In a split the flip is global (both panes reflow), so the unfocused pane's cursor needs
        // the same clamp against ITS role's new-layout row count. Its scroll is re-derived at
        // render time.
        if let EffectiveZoom::Split = self.effective_zoom_for(self.current) {
            let role = self.unfocused_split_role();
            let rows = self.role_row_count(self.current, role);
            self.alt.cursor = if rows == 0 {
                0
            } else {
                self.alt.cursor.min(rows - 1)
            };
        }
        self.derive_scroll();
    }

    /// Set the render layout directly — the config-startup (CS7) counterpart to
    /// [`Self::toggle_layout`]. Called before the first [`Self::open_current`], whose
    /// `reset_panes` derives `cursor`/`scroll` fresh for whichever layout is active, so —
    /// unlike `toggle_layout`, which must clamp an EXISTING cursor into the new layout's row
    /// count — no separate clamp is needed here. Does NOT call `open_current` itself — the
    /// caller applies every CS7 setting first, then opens once.
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    /// Apply `workon.review.outline.width|mode` and `workon.review.diff.layout|zoom` (CS7) as
    /// the App's initial view-config state, via the same setters the interactive keys drive
    /// (see each setter's doc comment for why that's enough to stay on the gated path). Call
    /// once, right after construction and before [`Self::open_current`] (see `main.rs`) — the
    /// setters here don't themselves re-derive `cursor`/`scroll`, and the caller's
    /// `open_current` is what does that for whichever settings just landed.
    ///
    /// `raw` is read via [`crate::config::ReviewConfig::view_config`] BEFORE `repo` moves into
    /// `App` (see `main.rs`) — its fields already collapsed an unset setting and a config-read
    /// error to the same `None` (CS7 applies the current hardcoded default for either case, no
    /// warning). Each setting additionally falls back to the default when SET but invalid — out
    /// of range (width), or an unrecognized string (mode/layout/zoom) — collecting a warning for
    /// those cases, same non-fatal posture as the keymap/theme resolution (ADR-034).
    pub fn apply_view_config(&mut self, raw: &RawViewConfig) -> Vec<String> {
        let mut warnings = Vec::new();

        let width = match raw.outline_width {
            Some(w) => match u16::try_from(w) {
                Ok(w) if (MIN_OUTLINE_WIDTH..=MAX_OUTLINE_WIDTH).contains(&w) => w,
                _ => {
                    warnings.push(format!(
                        "workon.review.outline.width = {w} out of range \
                         ({MIN_OUTLINE_WIDTH}-{MAX_OUTLINE_WIDTH}); using default"
                    ));
                    DEFAULT_OUTLINE_WIDTH
                }
            },
            None => DEFAULT_OUTLINE_WIDTH,
        };
        self.set_outline_width(width);

        let mode = match &raw.outline_mode {
            Some(m) => parse_outline_mode(m).unwrap_or_else(|| {
                warnings.push(format!(
                    "workon.review.outline.mode = '{m}' unrecognized; using default"
                ));
                OutlineMode::default()
            }),
            None => OutlineMode::default(),
        };
        self.set_outline_mode(mode);

        let order = match &raw.outline_order {
            Some(o) => parse_outline_order(o).unwrap_or_else(|| {
                warnings.push(format!(
                    "workon.review.outline.order = '{o}' unrecognized; using default"
                ));
                OutlineOrder::default()
            }),
            None => OutlineOrder::default(),
        };
        self.set_outline_order(order);

        let icons = match &raw.icons {
            Some(i) => parse_icon_mode(i).unwrap_or_else(|| {
                warnings.push(format!(
                    "workon.review.icons = '{i}' unrecognized; using default"
                ));
                IconMode::default()
            }),
            None => IconMode::default(),
        };
        self.set_icon_mode(icons);

        let layout = match &raw.diff_layout {
            Some(l) => parse_diff_layout(l).unwrap_or_else(|| {
                warnings.push(format!(
                    "workon.review.diff.layout = '{l}' unrecognized; using default"
                ));
                Layout::default()
            }),
            None => Layout::default(),
        };
        self.set_layout(layout);

        let zoom = match &raw.diff_zoom {
            Some(z) => parse_diff_zoom(z).unwrap_or_else(|| {
                warnings.push(format!(
                    "workon.review.diff.zoom = '{z}' unrecognized; using default"
                ));
                Zoom::default()
            }),
            None => Zoom::default(),
        };
        self.set_zoom(zoom);

        warnings
    }

    /// Set a transient footer notice (see [`Self::notice`]'s doc comment). Overwrites any
    /// currently-showing notice rather than queuing — only one message is ever on screen.
    pub fn notify(&mut self, text: impl Into<String>, severity: Severity) {
        self.notice = Some(Notice {
            text: text.into(),
            severity,
        });
    }

    /// Dismiss the current footer notice, if any (a no-op if there isn't one).
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// The hunk (index into the FOCUSED role view's hunks) under the cursor, in whichever
    /// coordinate space [`Self::layout`] is active — `None` when the cursor sits on context
    /// outside every hunk, or there's no loaded view. Staging ops resolve their hunk target
    /// through this.
    pub fn hunk_at_cursor(&self) -> Option<usize> {
        let view = self.current_view_ref()?;
        match self.layout {
            Layout::Sbs => view.hunk_at_display_row(self.cursor),
            Layout::Inline => view.hunk_at_inline_row(self.cursor),
        }
    }

    /// The role a staging verb acts in for the current file: the single effective role, or the
    /// focused split pane's role. `None` for [`Role::Combined`] — the combined view fuses both
    /// sub-diffs, so staging there has no unambiguous direction and the verbs refuse (locked
    /// decision #1).
    fn staging_role(&self) -> Option<Role> {
        match self.effective_zoom_for(self.current) {
            EffectiveZoom::Single(Role::Combined) => None,
            EffectiveZoom::Single(role) => Some(role),
            EffectiveZoom::Split => Some(self.split_focus_role()),
        }
    }

    /// Toggle-direction by role (locked decision #1): the unstaged pane stages, the staged pane
    /// unstages. `None` for [`Role::Combined`] (never a staging target).
    fn verb_for_role(role: Role) -> Option<StageVerb> {
        match role {
            Role::Unstaged => Some(StageVerb::Stage),
            Role::Staged => Some(StageVerb::Unstage),
            Role::Combined => None,
        }
    }

    /// Mode-aware refusal notice for a staging verb / line-selection start that only makes sense
    /// outside the combined view — i.e. every call site below whose `staging_role()`/
    /// `staging_role().is_none()` guard failed (locked decision #2's "targeted guard"). A
    /// committed changeset is ALWAYS combined-only (no staged/unstaged split exists to zoom
    /// into — see [`Self::is_committed`]), so telling the user to "cycle zoom" there is actively
    /// wrong; state the real reason instead. `verb` ("stage"/"select") keeps each call site's
    /// original non-committed wording.
    fn notify_combined_refusal(&mut self, verb: &str) {
        if self.is_committed() {
            self.notify(
                "changeset is already committed — nothing to stage",
                Severity::Error,
            );
        } else {
            self.notify(
                format!("{verb} in the unstaged/staged pane — cycle zoom (z)"),
                Severity::Error,
            );
        }
    }

    /// Stage (unstaged pane) or unstage (staged pane) the hunk under the cursor (`s`). Refuses on
    /// the combined view, or when the cursor isn't in a hunk.
    ///
    /// When a line selection is active, `s` acts on the SELECTION instead
    /// ([`Self::stage_selection`]) — the hunk under the cursor is irrelevant once the user has
    /// marked exact lines.
    pub fn stage_hunk(&mut self) {
        if self.selection_anchor.is_some() {
            self.stage_selection();
            return;
        }
        if self.cur().diff.files.is_empty() {
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        let Some(verb) = Self::verb_for_role(role) else {
            return;
        };
        let Some(hunk_idx) = self.hunk_at_cursor() else {
            self.notify("no hunk under cursor", Severity::Error);
            return;
        };
        // The hunk index is into the ROLE's own hunks, so the op must apply against that role's
        // sub-`FileChange`, not the combined one.
        let Some(file) = self.role_change(self.current, role).cloned() else {
            return;
        };
        self.run_op(FileStagingOp::hunk(file, hunk_idx, verb));
    }

    /// Stage (unstaged pane) or unstage (staged pane) the whole current file (`S`) — ignores the
    /// cursor. Refuses on the combined view.
    pub fn stage_file(&mut self) {
        if self.cur().diff.files.is_empty() {
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        let Some(verb) = Self::verb_for_role(role) else {
            return;
        };
        // A whole-file op routes on path + status only ([`crate::ops::apply_file`]), which the
        // combined file carries authoritatively (e.g. Untracked-ness for a discard).
        let file = self.cur().diff.files[self.current].clone();
        self.run_op(FileStagingOp::file(file, verb));
    }

    /// Request confirmation to discard the hunk under the cursor from the worktree (`d`). Refuses
    /// on the combined view, in a staged pane (discard only reverts worktree changes), or when the
    /// cursor isn't in a hunk. The discard itself runs when the user answers `y`.
    ///
    /// When a line selection is active, `d` acts on the SELECTION instead
    /// ([`Self::discard_selection`]).
    pub fn discard_hunk(&mut self) {
        if self.selection_anchor.is_some() {
            self.discard_selection();
            return;
        }
        if self.cur().diff.files.is_empty() {
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        if role != Role::Unstaged {
            self.notify("discard acts in the unstaged pane", Severity::Error);
            return;
        }
        let Some(hunk_idx) = self.hunk_at_cursor() else {
            self.notify("no hunk under cursor", Severity::Error);
            return;
        };
        self.request_confirm(
            "Discard this hunk from the worktree? (y/n)".to_string(),
            PendingOp::DiscardHunk {
                file_idx: self.current,
                hunk_idx,
            },
        );
    }

    /// Request confirmation to discard the whole current file's worktree changes (`D`). Refuses on
    /// the combined view or in a staged pane; the discard runs on `y`.
    pub fn discard_file(&mut self) {
        if self.cur().diff.files.is_empty() {
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        if role != Role::Unstaged {
            self.notify("discard acts in the unstaged pane", Severity::Error);
            return;
        }
        let path = self.cur().diff.files[self.current].path.clone();
        self.request_confirm(
            format!("Discard all changes to `{path}`? (y/n)"),
            PendingOp::DiscardFile {
                file_idx: self.current,
            },
        );
    }

    /// Set a pending confirm (see [`Self::pending_confirm`]). Overwrites any current one.
    pub fn request_confirm(&mut self, prompt: impl Into<String>, op: PendingOp) {
        self.pending_confirm = Some(Confirm {
            prompt: prompt.into(),
            op,
        });
    }

    /// Resolve a pending confirm: on `accept` run its op, then clear it either way. A no-op with
    /// no confirm pending. A cancel (`accept == false`) is left silent — the cleared prompt is
    /// feedback enough.
    pub fn resolve_confirm(&mut self, accept: bool) {
        let Some(confirm) = self.pending_confirm.take() else {
            return;
        };
        // Answering the modal consumes any active selection either way: its snapshot is already
        // baked into the `PendingOp` for a line discard, and a lingering highlight after the modal
        // closes would be confusing. A no-op when nothing is selected.
        self.selection_anchor = None;
        if !accept {
            return;
        }
        match confirm.op {
            PendingOp::DiscardHunk { file_idx, hunk_idx } => {
                // The hunk index came from the unstaged pane's view, so discard against the
                // unstaged role's sub-`FileChange`.
                let Some(file) = self.role_change(file_idx, Role::Unstaged).cloned() else {
                    return;
                };
                self.run_op(FileStagingOp::hunk(file, hunk_idx, StageVerb::Discard));
            }
            PendingOp::DiscardFile { file_idx } => {
                let Some(file) = self.cur().diff.files.get(file_idx).cloned() else {
                    return;
                };
                self.run_op(FileStagingOp::file(file, StageVerb::Discard));
            }
            PendingOp::DiscardLines {
                file_idx,
                selections,
            } => {
                // Line discard acts in the unstaged pane, so its selections are keyed against the
                // unstaged role's sub-`FileChange`.
                let Some(file) = self.role_change(file_idx, Role::Unstaged).cloned() else {
                    return;
                };
                self.run_op(LineSelectionOp::new(file, selections, StageVerb::Discard));
            }
            PendingOp::DiscardOutlineFiles { files, identity } => {
                // Re-resolve each (changeset name, path) pair against the LIVE changesets — an
                // intervening tick refresh may have shifted every index since `d` was pressed
                // (see the variant's doc); a pair that no longer resolves is silently skipped
                // (its file already left the diff, so there's nothing left to discard).
                let ops: Vec<Box<dyn StagingOp>> = files
                    .iter()
                    .filter_map(|(cs_name, path)| {
                        let view = self.changesets.iter().find(|v| v.cs.name == *cs_name)?;
                        let file = view.files().iter().find(|f| f.path == *path)?.clone();
                        Some(Box::new(FileStagingOp::file(file, StageVerb::Discard))
                            as Box<dyn StagingOp>)
                    })
                    .collect();
                self.outline_run_ops(ops, identity);
            }
        }
    }

    /// Enqueue `op`, drain the queue on the same beat, then act on the outcome: a failure or panic
    /// surfaces on the footer (and the views still refresh — see [`Self::run_ops`] for why); a
    /// `Completed` drain refreshes, rebuilding the views + attribution from the new index (locked
    /// decision #5), then restores the reviewer's pre-op DIFF position (CS6) — a staging op is
    /// the ONE nav path that does not reset to the role's first hunk; every manual nav still
    /// does, via `reset_panes` unchanged.
    ///
    /// A thin diff-facing wrapper over [`Self::run_ops`] (one op, one memento) — the diff pane's
    /// staging verbs (`s`/`S`/`d`/`D`) are the only callers, so the shared drain/refresh core
    /// lives on `run_ops` and this just supplies the diff-position memento CS7's outline verbs
    /// don't want (see [`Self::outline_run_ops`], which restores the OUTLINE cursor instead).
    fn run_op(&mut self, op: impl StagingOp + 'static) {
        let memento = self.capture_position();
        if self.run_ops(vec![Box::new(op)]).is_ok() {
            if let Some(memento) = memento {
                self.restore_position(memento);
            }
        }
    }

    /// Enqueue every op in `ops`, drain the queue on the same beat, then run a
    /// [`Self::coordinated_refresh`] REGARDLESS of outcome — the drain never stops on a failure,
    /// so a partial multi-op batch has already mutated the index/worktree and the views must
    /// re-read that reality even while a failure notice shows. Returns `Err` after notifying the
    /// first failure/panic, `Ok(())` otherwise. Callers own what happens next (a diff-position or
    /// outline-cursor restore, or nothing) — this only owns the queue mechanics.
    ///
    /// Generic over any [`StagingOp`] — a hunk/file op ([`FileStagingOp`]), a (possibly
    /// multi-hunk) line selection ([`LineSelectionOp`], which applies as ONE merged patch rather
    /// than enqueueing one op per hunk — see that type's docs for why splitting is wrong), or
    /// (CS7) several independent whole-file ops from an outline Dir row. The queue's trap-4
    /// live-index staleness doesn't apply here: every op resolves its own direction from the live
    /// index inside `run` (see `queue.rs`'s module doc), so draining several back-to-back is safe.
    fn run_ops(&mut self, ops: Vec<Box<dyn StagingOp>>) -> Result<(), ()> {
        for op in ops {
            self.queue.enqueue(op);
        }
        // Distinct fields (`queue` mutable, `repo`/`applier` shared) — the borrow checker permits
        // the disjoint borrows in one call, so the queue needn't be taken out and put back.
        let outcomes = self.queue.drain(&self.repo, &self.applier);
        let failure = outcomes.iter().find_map(|outcome| match outcome {
            OpOutcome::Failed(_, err) => Some(format!("staging failed: {err}")),
            OpOutcome::Panicked(_) => Some("staging operation panicked".to_string()),
            OpOutcome::Completed(_) => None,
        });
        // Refresh in BOTH arms: the queue's drain never stops on a failure (`pump` runs every
        // queued op regardless), so in a multi-op batch a single failure still leaves up to N-1
        // other ops applied to the index/worktree — the views must re-read that reality even
        // while the failure notice shows. (For a single-op batch the refresh is a harmless
        // re-read of unchanged state.)
        self.coordinated_refresh();
        match failure {
            Some(message) => {
                self.notify(message, Severity::Error);
                Err(())
            }
            None => Ok(()),
        }
    }

    /// Snapshot the focused pane's file/role/position ahead of a staging op, for
    /// [`Self::restore_position`] to reseat after the op's `coordinated_refresh` (CS6). `None`
    /// when there's no current file, the current view is the combined role (never a staging
    /// target — [`Self::staging_role`]), or the focused role's view isn't loaded; restore is then
    /// a no-op and today's `reset_panes` first-hunk behavior stands.
    fn capture_position(&self) -> Option<PositionMemento> {
        let path = self.files().get(self.current)?.path.clone();
        let role = self.staging_role()?;
        let view = self.role_view_ref(self.current, role)?;
        // Reuse the same row -> lineno extraction `FileView::load` builds its hunk maps from
        // (a Gap row yields (None, None), which restore treats as nothing-to-search-for).
        let (old_lineno, new_lineno) = match self.layout {
            Layout::Sbs => view
                .display
                .get(self.cursor)
                .map(display_row_linenos)
                .unwrap_or((None, None)),
            Layout::Inline => view
                .inline
                .get(self.cursor)
                .map(inline_row_linenos)
                .unwrap_or((None, None)),
        };
        Some(PositionMemento {
            path,
            role,
            old_lineno,
            new_lineno,
        })
    }

    /// Reseat the focused pane to a pre-staging-op position after `coordinated_refresh` rebuilds
    /// the views (CS6) — the staging-path counterpart to `reset_panes`' first-hunk reseat, which
    /// this deliberately leaves untouched for every manual nav (file/changeset switch, zoom
    /// cycle). Falls back to whatever `reset_panes` already produced (today's first-hunk
    /// behavior) when the acted-on file's path is gone (fully discarded) or its memento carried
    /// no lineno at all (the cursor sat on a `Gap` row pre-op — nothing to search for).
    fn restore_position(&mut self, m: PositionMemento) {
        if self.files().get(self.current).map(|f| f.path.as_str()) != Some(m.path.as_str()) {
            return;
        }
        // Force the load `reset_panes` may have deferred so the view below actually exists.
        self.complete_pending_open();

        // Target role: a still-`Split` file keeps both panes, so stay on the memento's own role
        // (locked decision: same file, same pane, unless that pane's role is now gone). A
        // collapsed-to-`Single` file has exactly one surviving role — THAT is the target
        // regardless of which pane the op started in, which is what lands "fully staging a file
        // in Split" in the staged pane of the same file.
        let target_role = match self.effective_zoom_for(self.current) {
            EffectiveZoom::Split => m.role,
            EffectiveZoom::Single(role) => role,
        };
        if matches!(self.effective_zoom_for(self.current), EffectiveZoom::Split)
            && self.split_focus_role() != target_role
        {
            // Never assign `split_focus` directly — this swaps the cursor/scroll/pane-height
            // stashes along with it.
            self.toggle_split_focus();
        }

        let Some(view) = self.role_view_ref(self.current, target_role) else {
            return;
        };

        // The memento's linenos were captured in `m.role`'s own frame (new = worktree for
        // Unstaged/Combined, new = index for Staged — see `FileView::load`'s table). Preferring
        // new over old is correct BOTH when the role is unchanged (the common case: same pane,
        // same frame) AND on the one role change that can happen here — unstaged -> staged after
        // fully staging a file in Split. In that case the staged view's new side (index) now
        // holds exactly what the unstaged view's new side (worktree) held a moment ago, because
        // staging made index == worktree for this file; so new -> new is still the right
        // mapping. Whichever side supplies the target, the SEARCH stays in that same frame —
        // `find_nearest_row` never falls back across sides (see its doc for why mixing frames
        // mis-lands the cursor).
        let (target_lineno, new_frame) = match (m.new_lineno, m.old_lineno) {
            (Some(n), _) => (n, true),
            (None, Some(o)) => (o, false),
            (None, None) => return,
        };
        let Some(cursor) = find_nearest_row(view, self.layout, target_lineno, new_frame) else {
            return;
        };
        self.cursor = cursor;
        self.clamp_cursor();
        self.derive_scroll();
    }

    /// Start a line selection anchored at the current cursor (`v`). Refuses (a notice, no anchor
    /// set) on the combined view or any non-staging role — you can only select lines where you can
    /// stage them (same gate as the verbs). A no-op on an empty file list.
    pub fn start_selection(&mut self) {
        if self.cur().diff.files.is_empty() {
            return;
        }
        if self.staging_role().is_none() {
            self.notify_combined_refusal("select");
            return;
        }
        self.selection_anchor = Some(self.cursor);
    }

    /// Cancel an active line selection (`Esc`). A no-op when none is active.
    pub fn cancel_selection(&mut self) {
        self.selection_anchor = None;
    }

    /// The inclusive `[lo, hi]` row range of the active selection in the focused pane's active
    /// layout coordinate space, or `None` when no selection is active. Derived fresh from
    /// anchor+cursor so `j`/`k` extend it for free.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Map the active selection to one [`LineSelection`] per hunk it overlaps — locked decision
    /// #8's keep-predicate mapping. Returns `(hunk_idx, LineSelection)` per hunk, ascending by
    /// `hunk_idx`, keeping only hunks with at least one changed line inside the range (a hunk
    /// grazed by only context/gap rows is dropped). Empty when there's no selection, no loaded
    /// focused view, or the range covers only context.
    ///
    /// The two layouts differ in what a selected row contributes (locked decision #8):
    /// - **SBS** row-pair semantics: a selected `AlignedRow` keeps BOTH sides it changes — its Del
    ///   cell's old line and its Add cell's new line — because a side-by-side row can't split a
    ///   paired edit (per-side precision is what inline is for).
    /// - **Inline**: a selected `Del` keeps only its old-side line, an `Add` only its new-side
    ///   line.
    ///
    /// A [`LineSelection`]'s keys are indices into the hunk's `lines` vec, not line numbers, so
    /// the collected old-del / new-add line numbers are resolved back to `HunkLine` positions via
    /// the role's own [`FileChange`] — see [`line_selection_for_hunk`].
    fn selection_line_ops(&self) -> Vec<(usize, LineSelection)> {
        let Some((lo, hi)) = self.selection_range() else {
            return Vec::new();
        };
        let Some(role) = self.staging_role() else {
            return Vec::new();
        };
        let Some(view) = self.current_view_ref() else {
            return Vec::new();
        };
        let Some(file) = self.role_change(self.current, role) else {
            return Vec::new();
        };

        // Per hunk: (selected old-side deletion linenos, selected new-side addition linenos).
        let mut per_hunk: BTreeMap<usize, (BTreeSet<u32>, BTreeSet<u32>)> = BTreeMap::new();
        match self.layout {
            Layout::Sbs => {
                for r in lo..=hi {
                    let Some(DisplayRow::Row(row)) = view.display.get(r) else {
                        continue;
                    };
                    let Some(hunk_idx) = view.hunk_at_display_row(r) else {
                        continue;
                    };
                    let entry = per_hunk.entry(hunk_idx).or_default();
                    if row.old_kind == CellKind::Del {
                        if let Row::Line(n) = row.old {
                            entry.0.insert(n as u32);
                        }
                    }
                    if row.new_kind == CellKind::Add {
                        if let Row::Line(n) = row.new {
                            entry.1.insert(n as u32);
                        }
                    }
                }
            }
            Layout::Inline => {
                for r in lo..=hi {
                    let Some(row) = view.inline.get(r) else {
                        continue;
                    };
                    let Some(hunk_idx) = view.hunk_at_inline_row(r) else {
                        continue;
                    };
                    match *row {
                        InlineRow::Del { old, .. } => {
                            per_hunk.entry(hunk_idx).or_default().0.insert(old as u32);
                        }
                        InlineRow::Add { new, .. } => {
                            per_hunk.entry(hunk_idx).or_default().1.insert(new as u32);
                        }
                        InlineRow::Context { .. } | InlineRow::Gap { .. } => {}
                    }
                }
            }
        }

        per_hunk
            .into_iter()
            .filter_map(|(hunk_idx, (dels, adds))| {
                let hunk = file.hunks.get(hunk_idx)?;
                let sel = line_selection_for_hunk(hunk, &dels, &adds);
                if sel.keep_dels.is_empty() && sel.keep_adds.is_empty() {
                    None
                } else {
                    Some((hunk_idx, sel))
                }
            })
            .collect()
    }

    /// Stage (unstaged pane) / unstage (staged pane) the active line selection (`s` with a
    /// selection up). Refuses on the combined view (cycle-zoom notice), on a file no hunk patch
    /// can express (the modified-file notice — line ops need a two-sided hunk, per
    /// [`ops::is_hunk_patchable`]), and on a selection that covers no changed lines. Otherwise
    /// applies every overlapped hunk's kept lines as ONE merged patch via [`LineSelectionOp`]
    /// (never one op per hunk — see that type's docs), drains once, and clears the selection.
    fn stage_selection(&mut self) {
        if self.cur().diff.files.is_empty() {
            self.cancel_selection();
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        let Some(verb) = Self::verb_for_role(role) else {
            return;
        };
        if !ops::is_hunk_patchable(&self.cur().diff.files[self.current]) {
            self.notify(
                "line staging needs a modified file — use s/S for the whole file",
                Severity::Error,
            );
            return;
        }
        let selections = self.selection_line_ops();
        if selections.is_empty() {
            self.notify("no changed lines in selection", Severity::Error);
            return;
        }
        let Some(file) = self.role_change(self.current, role).cloned() else {
            return;
        };
        self.run_op(LineSelectionOp::new(file, selections, verb));
        self.cancel_selection();
    }

    /// Request confirmation to discard the active line selection from the worktree (`d` with a
    /// selection up). Discard acts only in the unstaged pane; refuses otherwise, on a
    /// non-hunk-patchable file, or on a selection with no changed lines. The confirm prompt states
    /// the TRUE scope (total lines across N hunks); the discard runs on `y`.
    fn discard_selection(&mut self) {
        if self.cur().diff.files.is_empty() {
            self.cancel_selection();
            return;
        }
        let Some(role) = self.staging_role() else {
            self.notify_combined_refusal("stage");
            return;
        };
        if role != Role::Unstaged {
            self.notify("discard acts in the unstaged pane", Severity::Error);
            return;
        }
        if !ops::is_hunk_patchable(&self.cur().diff.files[self.current]) {
            self.notify(
                "line staging needs a modified file — use s/S for the whole file",
                Severity::Error,
            );
            return;
        }
        let selections = self.selection_line_ops();
        if selections.is_empty() {
            self.notify("no changed lines in selection", Severity::Error);
            return;
        }
        let total: usize = selections
            .iter()
            .map(|(_, s)| s.keep_dels.len() + s.keep_adds.len())
            .sum();
        let hunks = selections.len();
        let prompt = format!(
            "Discard {total} line{} across {hunks} hunk{} from the worktree? (y/n)",
            if total == 1 { "" } else { "s" },
            if hunks == 1 { "" } else { "s" },
        );
        self.request_confirm(
            prompt,
            PendingOp::DiscardLines {
                file_idx: self.current,
                selections,
            },
        );
    }
}

/// Resolve a selection's kept old-del / new-add LINE NUMBERS to a [`LineSelection`] — whose keys
/// are indices into `hunk.lines`, not line numbers (see [`LineSelection`]'s own docs). Walks the
/// hunk once, keeping each deletion whose `old_lnum` is in `keep_old_dels` and each addition whose
/// `new_lnum` is in `keep_new_adds`; context lines contribute nothing.
fn line_selection_for_hunk(
    hunk: &Hunk,
    keep_old_dels: &BTreeSet<u32>,
    keep_new_adds: &BTreeSet<u32>,
) -> LineSelection {
    let mut sel = LineSelection::default();
    for (i, line) in hunk.lines.iter().enumerate() {
        match line.kind {
            LineKind::Deletion => {
                if line.old_lnum.is_some_and(|o| keep_old_dels.contains(&o)) {
                    sel.keep_dels.insert(i);
                }
            }
            LineKind::Addition => {
                if line.new_lnum.is_some_and(|n| keep_new_adds.contains(&n)) {
                    sel.keep_adds.insert(i);
                }
            }
            LineKind::Context => {}
        }
    }
    sel
}

/// The diff-derived pieces one [`ChangesetView`] carries — everything EXCEPT the view caches
/// (which [`ChangesetView::new`] and [`App::refresh`] reset to freshly-sized `None` vectors) and
/// the App-level navigation/UI state that survives a refresh (`current`, `cursor`, `layout`,
/// `zoom`, etc. — see [`App::refresh`]'s doc comment).
struct DiffState {
    files: Vec<FileChange>,
    unstaged_model: DiffModel,
    staged_model: DiffModel,
    unstaged_idx: Vec<Option<usize>>,
    staged_idx: Vec<Option<usize>>,
}

impl From<WorktreeDiffs> for DiffState {
    fn from(diffs: WorktreeDiffs) -> Self {
        let WorktreeDiffs {
            staged,
            unstaged,
            combined,
        } = diffs;
        let files = combined.files;
        let unstaged_idx = files
            .iter()
            .map(|f| find_role_change(&unstaged, f))
            .collect();
        let staged_idx = files.iter().map(|f| find_role_change(&staged, f)).collect();
        Self {
            files,
            unstaged_model: unstaged,
            staged_model: staged,
            unstaged_idx,
            staged_idx,
        }
    }
}

impl DiffState {
    /// An empty [`DiffState`] — every field zero-length. Used for `Pending`/`Failed`
    /// [`ChangesetView`] slots (ADR-037), which carry no real diff; existing `.diff.` read sites
    /// already treat an empty `files` list as "nothing to show," so this alone is enough to make
    /// those slots render/navigate as inert with no per-site Pending/Failed branch.
    fn empty() -> Self {
        Self {
            files: Vec::new(),
            unstaged_model: DiffModel { files: Vec::new() },
            staged_model: DiffModel { files: Vec::new() },
            unstaged_idx: Vec::new(),
            staged_idx: Vec::new(),
        }
    }

    /// Build a [`DiffState`] for a COMMITTED changeset's [`DiffModel`] (`base..head`, already
    /// diffed by [`crate::acquire::diff_committed`]) — there is no staged/unstaged split for a
    /// committed range, so both sub-models are empty and every index map entry is `None`. This
    /// alone is enough to render the changeset read-only: [`effective_zoom`] collapses
    /// `Split`/`Unstaged`/`Staged` to [`EffectiveZoom::Single(Role::Combined)`] whenever both
    /// sub-diffs are absent, so no committed-specific rendering path is needed for M5's spine.
    fn from_committed(model: DiffModel) -> Self {
        let n = model.files.len();
        Self {
            files: model.files,
            unstaged_model: DiffModel { files: Vec::new() },
            staged_model: DiffModel { files: Vec::new() },
            unstaged_idx: (0..n).map(|_| None).collect(),
            staged_idx: (0..n).map(|_| None).collect(),
        }
    }
}

/// Index of the changeset the lib marked `current` (locked decision #6), or `0` if none is —
/// the shared rule [`App::from_changesets`] uses to open, and [`App::refresh`] falls back to
/// when the previously-active changeset's name no longer exists after a re-assembly.
fn current_cs_index(changesets: &[ChangesetView]) -> usize {
    changesets.iter().position(|v| v.cs.current).unwrap_or(0)
}

/// The `(cs_idx, file_idx)` identity an [`OutlineItem::Header`]/[`OutlineItem::File`] row
/// carries — `file_idx` is `None` for a header row. `OutlineItem::Dir` carries no `cs_idx` at
/// all (see its doc comment) and has no identity to preserve. Used by
/// [`App::apply_changeset_ready`] (F3) to re-find the row the outline cursor was on after a
/// streamed diff landing inserts/removes rows ahead of it in the row-index space.
fn outline_row_identity(item: &OutlineItem) -> Option<(usize, Option<usize>)> {
    match item {
        OutlineItem::Header { cs_idx, .. } => Some((*cs_idx, None)),
        OutlineItem::File {
            cs_idx, file_idx, ..
        } => Some((*cs_idx, Some(*file_idx))),
        OutlineItem::Dir { .. } => None,
    }
}

// ── ADR-037: the loader thread's stateless request/job shape ────────────────────

/// Everything the ADR-037 loader job needs to reproduce one file's [`App::ensure_loaded`] work
/// against its OWN `Repository` + [`TsHighlighter`] — the loader is stateless between jobs (see
/// the ADR's "Protocol": "each request carries what it needs"). Built by
/// [`App::current_load_spec`] from live `App` state at request-send time; every field is owned
/// (cloned out of `App`), so the spec outlives the borrow and crosses to the loader thread.
#[derive(Debug, Clone)]
pub struct FileLoadSpec {
    span: ChangesetSpan,
    combined_file: FileChange,
    /// The [`EffectiveZoom`] `App` had AT DISPATCH TIME — the views built are shaped by this,
    /// not by whatever `App`'s zoom/current file happen to be when the result lands (which may
    /// have changed by then; that's fine, see the ADR's "Generations": within a generation, a
    /// result is warmth even after the user navigated away).
    zoom: EffectiveZoom,
    unstaged_file: Option<FileChange>,
    staged_file: Option<FileChange>,
}

/// The [`FileView`]s [`build_file_views`] built for one [`FileLoadSpec`], shaped exactly like the
/// [`EffectiveZoom`] it was built for — [`App::apply_file_ready`] reads this shape to know which
/// cache slot(s) to fill without re-deriving the zoom itself (which could disagree with the zoom
/// the views were actually built against — see [`FileLoadSpec::zoom`]'s doc comment).
/// `FileView` fields (`Box`ed here, see below) — a `FileReady` `AppEvent` carrying this unboxed
/// would otherwise make the WHOLE `AppEvent` enum balloon to `FileView`'s size on every variant
/// (clippy's `large_enum_variant`), even the plain `Key`/`Tick` ones sent on every keystroke.
#[derive(Debug)]
pub enum LoadedViews {
    Single(Role, Option<Box<FileView>>),
    Split {
        unstaged: Option<Box<FileView>>,
        staged: Option<Box<FileView>>,
    },
}

/// Whether a loaded result's SHAPE — what zoom it was built against, per [`FileLoadSpec::zoom`]
/// — still matches `current_zoom`, the current file's effective zoom at result-apply time. Used
/// by [`App::apply_file_ready`] to tell a still-useful deferred-open result apart from one a
/// mid-load `z` cycle outran: `Single` satisfies only the SAME role's `Single`, `Split`
/// satisfies only `Split` (never the reverse — a `Split` result doesn't seat a `Single` open,
/// and vice versa, even though `set_if_absent` already caches whichever roles it carries).
fn loaded_views_satisfy(views: &LoadedViews, current_zoom: EffectiveZoom) -> bool {
    match (views, current_zoom) {
        (LoadedViews::Single(role, _), EffectiveZoom::Single(want)) => *role == want,
        (LoadedViews::Split { .. }, EffectiveZoom::Split) => true,
        _ => false,
    }
}

/// Build every [`FileView`] a [`FileLoadSpec`] needs, against `repo`/`ts` — the ADR-037 loader
/// thread's pure job body: unit-testable directly against a fixture repo, no threads or channels
/// involved. Routes through the SAME [`build_combined_view`]/[`build_sub_role_view`] free
/// functions [`App::ensure_role_loaded`] calls, so a deferred-then-loader-completed open is
/// byte-identical to an eager [`App::open_current`] — the invariant ADR-037 carries over from
/// CS4's `complete_pending_open`.
pub fn build_file_views(
    repo: &Repository,
    ts: &mut TsHighlighter,
    spec: &FileLoadSpec,
) -> LoadedViews {
    match spec.zoom {
        EffectiveZoom::Single(role) => {
            let view = match role {
                Role::Combined => build_combined_view(repo, ts, spec.span, &spec.combined_file),
                Role::Unstaged => spec
                    .unstaged_file
                    .as_ref()
                    .and_then(|f| build_sub_role_view(repo, ts, Role::Unstaged, f)),
                Role::Staged => spec
                    .staged_file
                    .as_ref()
                    .and_then(|f| build_sub_role_view(repo, ts, Role::Staged, f)),
            };
            LoadedViews::Single(role, view.map(Box::new))
        }
        EffectiveZoom::Split => LoadedViews::Split {
            unstaged: spec
                .unstaged_file
                .as_ref()
                .and_then(|f| build_sub_role_view(repo, ts, Role::Unstaged, f))
                .map(Box::new),
            staged: spec
                .staged_file
                .as_ref()
                .and_then(|f| build_sub_role_view(repo, ts, Role::Staged, f))
                .map(Box::new),
        },
    }
}

/// Cache `view` into changeset `cs`'s `role` view slot for file `idx`, UNLESS that slot is
/// already `Some` — [`App::apply_file_ready`]'s "a result for an already-cached file is
/// discarded" rule (the loader is a pure cache-warmer, never an overwriter). A no-op if `idx` is
/// out of range (the changeset shrank across a refresh — should already be unreachable, since a
/// refresh bumps the generation and `apply_file_ready` drops stale-generation results before
/// this ever runs, but `get_mut` stays defensive rather than indexing).
fn set_if_absent(cs: &mut ChangesetView, role: Role, idx: usize, view: Option<Box<FileView>>) {
    let view = view.map(|boxed| *boxed);
    let slots = match role {
        Role::Combined => &mut cs.views_combined,
        Role::Unstaged => &mut cs.views_unstaged,
        Role::Staged => &mut cs.views_staged,
    };
    if let Some(slot) = slots.get_mut(idx) {
        if slot.is_none() {
            *slot = view;
        }
    }
}

/// [`App::base_label`] for the changeset that would become active — a committed changeset's
/// base rev (7-char short-sha), or `"HEAD"` for the uncommitted layer (worktree ↔ `HEAD`,
/// unchanged from M2–M4).
fn base_label_for(cs: &Changeset) -> String {
    match cs.span {
        ChangesetSpan::Committed { base, .. } => {
            let full = base.to_string();
            full.chars().take(7).collect()
        }
        // No real base commit to abbreviate — the base is the empty tree.
        ChangesetSpan::CommittedRoot { .. } => "(empty)".to_string(),
        ChangesetSpan::Uncommitted => "HEAD".to_string(),
    }
}

/// Index of the [`FileChange`] in a role's [`DiffModel`] that corresponds to combined `file`, or
/// `None` when the role has no change for it (e.g. an untracked file in the staged model).
///
/// Matches by `path` (the common case), with rename-aware fallbacks: the combined and sub-diffs
/// agree on a rename's new `path`, but a file renamed in only one role can leave the match to
/// `old_path` on either side. Path equality wins for the overwhelming majority; the fallbacks just
/// avoid dropping the odd asymmetric-rename pairing.
fn find_role_change(model: &DiffModel, file: &FileChange) -> Option<usize> {
    model.files.iter().position(|m| {
        m.path == file.path
            || (m.old_path.is_some() && m.old_path == file.old_path)
            || m.old_path.as_deref() == Some(file.path.as_str())
            || (file.old_path.as_deref().is_some()
                && file.old_path.as_deref() == Some(m.path.as_str()))
    })
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

/// Inline-layout analog of [`is_hunk_content_row`]: true for a `Del`/`Add` row (inline has no
/// `Filler` variant — see [`InlineRow`]'s doc comment), false for `Context`/`Gap`.
fn is_inline_hunk_content_row(row: &InlineRow) -> bool {
    matches!(row, InlineRow::Del { .. } | InlineRow::Add { .. })
}

/// The 1-based line number a [`Row`] carries on its side, or `None` for a filler.
fn row_lineno(row: Row) -> Option<usize> {
    match row {
        Row::Line(n) => Some(n),
        Row::Filler => None,
    }
}

/// The (old, new) 1-based line numbers a display row occupies — `None` on a filler side, and
/// `(None, None)` for a gap row (which belongs to no hunk).
fn display_row_linenos(row: &DisplayRow) -> (Option<usize>, Option<usize>) {
    match row {
        DisplayRow::Row(r) => (row_lineno(r.old), row_lineno(r.new)),
        DisplayRow::Gap { .. } => (None, None),
    }
}

/// CS9's tree-sitter scope-reveal inputs for the gap at `gap_cursor`: the anchor line and which
/// side it's in (`true` = new, `false` = old), resolved from the row immediately FOLLOWING the
/// gap in `layout`'s row vector — the plan's rationale: the next hunk is what you're reading
/// toward, so its enclosing scope is what's worth revealing. Prefers the new-side lineno when
/// present, falling back to old (CS6's [`App::restore_position`] convention) for the rows a
/// delete-only file's `Filler` new side never populates.
///
/// Returns `None` when: there's no row after the gap (a trailing gap with nothing beyond it to
/// anchor on), the anchor path's extension has no bundled grammar, or
/// [`enclosing_scope_lines`] finds no enclosing scope for the anchor line — every case the
/// caller treats identically, falling back to the flat +10/+10 reveal.
fn gap_scope_start(
    view: &FileView,
    layout: Layout,
    gap_cursor: usize,
    new_path: &str,
    old_path: &str,
) -> Option<(usize, bool)> {
    let (old, new) = match layout {
        Layout::Sbs => display_row_linenos(view.display.get(gap_cursor + 1)?),
        Layout::Inline => inline_row_linenos(view.inline.get(gap_cursor + 1)?),
    };

    let (anchor_line, anchor_prefers_new, text, lang_path) = match new {
        Some(n) => (n, true, view.new_text(), new_path),
        None => (old?, false, view.old_text(), old_path),
    };

    let ext = Path::new(lang_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let lang_key = lang_key_for_ext(ext)?;
    let (scope_start, _scope_end) = enclosing_scope_lines(lang_key, text, anchor_line)?;
    Some((scope_start, anchor_prefers_new))
}

/// Inline-coordinate analog of [`display_row_linenos`].
fn inline_row_linenos(row: &InlineRow) -> (Option<usize>, Option<usize>) {
    match *row {
        InlineRow::Context { old, new } => (Some(old), Some(new)),
        InlineRow::Del { old, .. } => (Some(old), None),
        InlineRow::Add { new, .. } => (None, Some(new)),
        InlineRow::Gap { .. } => (None, None),
    }
}

/// Which hunk (index into `hunks`) a row occupying old line `old` / new line `new` falls in, by
/// matching its line number against each hunk's `old_start`/`old_count` (or
/// `new_start`/`new_count`) span — the same counters [`align_file`] reads. A row inside a hunk's
/// span, INCLUDING a context line git kept within the `@@` block, belongs to that hunk; a row
/// outside every span (a between-hunks gap, or leading/trailing context) is `None`. First match
/// wins on the rare touching-span boundary between two adjacent hunks.
fn hunk_for_linenos(hunks: &[Hunk], old: Option<usize>, new: Option<usize>) -> Option<usize> {
    hunks.iter().position(|h| {
        if let Some(o) = old {
            let (start, count) = (h.old_start as usize, h.old_count as usize);
            if count > 0 && o >= start && o < start + count {
                return true;
            }
        }
        if let Some(n) = new {
            let (start, count) = (h.new_start as usize, h.new_count as usize);
            if count > 0 && n >= start && n < start + count {
                return true;
            }
        }
        false
    })
}

/// Inline-layout analog of [`find_next_hunk_row`]: row index of the next "hunk start" (a
/// `Del`/`Add` row whose predecessor is `Context`/`Gap`/absent) strictly after `after`, searching
/// [`crate::app::FileView::inline`] instead of `display`.
fn find_next_inline_hunk_row(inline: &[InlineRow], after: usize) -> Option<usize> {
    (after + 1..inline.len()).find(|&i| {
        is_inline_hunk_content_row(&inline[i])
            && (i == 0 || !is_inline_hunk_content_row(&inline[i - 1]))
    })
}

/// Inline-layout analog of [`find_prev_hunk_row`]. See [`find_next_inline_hunk_row`].
fn find_prev_inline_hunk_row(inline: &[InlineRow], before: usize) -> Option<usize> {
    (0..before.min(inline.len())).rev().find(|&i| {
        is_inline_hunk_content_row(&inline[i])
            && (i == 0 || !is_inline_hunk_content_row(&inline[i - 1]))
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
        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let owned = Repository::open(repo.workdir().expect("fixture has a workdir"))
            .expect("reopen fixture repo");
        App::new(owned, diffs)
    }
}

#[cfg(test)]
mod tests {
    use git2::Repository;
    use git_workon_fixture::prelude::*;
    use workon::{Changeset, ChangesetSpan};

    use super::test_support::app_from_fixture;
    use super::{
        build_file_views, find_next_hunk_row, find_prev_hunk_row, App, ChangesetView, DiffState,
        EffectiveZoom, HitRegions, Layout, LoadedViews, Region, Role, Severity, Summary,
        SummaryTarget, Zoom, DEFAULT_OUTLINE_WIDTH,
    };
    use crate::align::{AlignedRow, CellKind, DisplayRow, InlineRow, Row};
    use crate::config::ReviewConfig;
    use crate::icons::IconMode;
    use crate::model::FileStatus;
    use crate::outline::{OutlineItem, OutlineMode, OutlineOrder, StagedStatus};

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
        let paths: Vec<&str> = app.files().iter().map(|f| f.path.as_str()).collect();
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
        assert_eq!(app.files()[0].status, FileStatus::Added);
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
        assert_eq!(app.files()[0].status, FileStatus::Deleted);
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
        assert_eq!(app.files().len(), 1);
        assert_eq!(app.files()[0].status, FileStatus::Renamed);
        assert_eq!(app.files()[0].old_path.as_deref(), Some("old_name.txt"));
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
        assert!(app.files()[0].is_binary);
        app.ensure_loaded(0);
        assert!(app.current_view_ref().is_none());
    }

    // ── CS4: idle-deferred loads ──────────────────────────────────────────────

    /// A twin pair: one `App` with `defer_loads` off (the eager baseline), one with it on. Both
    /// built from independent copies of the SAME fixture so their diffs (and hunks) line up.
    fn defer_and_eager_twins(fixture: &git_workon_fixture::fixture::Fixture) -> (App, App) {
        let mut eager = app_from_fixture(fixture);
        eager.open_current();

        let mut deferred = app_from_fixture(fixture);
        deferred.set_defer_loads(true);
        deferred.open_current();

        (deferred, eager)
    }

    #[test]
    fn open_current_defers_load_and_complete_matches_eager_open() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "tracked.txt",
                "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold\nl10\nl11\nl12\n",
                "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew\nl10\nl11\nl12\n",
            )
            .build()
            .unwrap();

        let (mut deferred, eager) = defer_and_eager_twins(&fixture);

        // `open_current` under defer mode loads NOTHING and marks the open pending.
        assert!(
            deferred.current_view_ref().is_none(),
            "deferred open_current must not have loaded the current view"
        );
        assert!(deferred.open_pending(), "the open must be marked pending");

        deferred.complete_pending_open();

        assert!(
            !deferred.open_pending(),
            "complete_pending_open must clear the pending flag"
        );
        assert_eq!(
            deferred.cursor, eager.cursor,
            "cursor must land on the same (first-hunk) row an eager open would have"
        );
        assert_eq!(deferred.scroll, eager.scroll);
        let deferred_view = deferred.current_view_ref().expect("view now loaded");
        let eager_view = eager.current_view_ref().expect("eager view loaded");
        assert_eq!(deferred_view.old_text(), eager_view.old_text());
        assert_eq!(deferred_view.new_text(), eager_view.new_text());
        assert_eq!(deferred_view.display.len(), eager_view.display.len());
    }

    #[test]
    fn revisiting_a_cached_file_reopens_eagerly_without_a_pending_window() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "1\n2\n3\n", "1\nA\n3\n")
            .unstaged_file("b.txt", "1\n2\n3\n", "1\nB\n3\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current(); // a.txt: uncached — defers
        assert!(app.open_pending(), "an uncached file defers its open");
        app.complete_pending_open();

        app.current = 1;
        app.open_current(); // b.txt: uncached — defers
        assert!(app.open_pending(), "a different uncached file still defers");
        app.complete_pending_open();

        app.current = 0;
        app.open_current(); // back to a.txt: cached — must NOT defer
        assert!(
            !app.open_pending(),
            "revisiting a cached file must reopen eagerly — a pending window here would \
             flash the loading placeholder over an instantly-renderable diff"
        );
        assert!(
            app.current_view_ref().is_some(),
            "the cached view is available the moment the open returns"
        );
    }

    #[test]
    fn complete_pending_open_is_a_no_op_when_nothing_pending() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("tracked.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current();
        app.complete_pending_open();
        assert!(!app.open_pending());

        let cursor_before = app.cursor;
        let scroll_before = app.scroll;
        // Calling again with nothing pending must not touch cursor/scroll or reload anything.
        app.complete_pending_open();
        assert!(!app.open_pending());
        assert_eq!(app.cursor, cursor_before);
        assert_eq!(app.scroll, scroll_before);
    }

    // ── ADR-037: the loader's request/result seam ────────────────────────────────

    #[test]
    fn build_file_views_matches_ensure_loaded_for_the_combined_role() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("tracked.txt", "line1\nline2\n", "line1\nCHANGED\n")
            .build()
            .unwrap();

        let mut eager = app_from_fixture(&fixture);
        // The file only has an unstaged change, so the default `Split` zoom would collapse to
        // `Role::Unstaged` — force `Combined` explicitly so this test exercises the role its
        // name promises (a separate test would be needed for the Split/sub-role shape).
        eager.set_zoom(Zoom::Combined);
        eager.ensure_loaded(0);
        let eager_view = eager.current_view_ref().expect("eager view loaded");

        // A SEPARATE `App` gives us `current_load_spec()` for the same file, and a SEPARATE
        // `Repository` handle + fresh `TsHighlighter` stands in for the loader thread's own —
        // exactly the two-handle shape `Tui::run`/`spawn_loader_thread` build for real.
        let mut spec_app = app_from_fixture(&fixture);
        spec_app.set_zoom(Zoom::Combined);
        let spec = spec_app
            .current_load_spec()
            .expect("fixture has a file at index 0");
        let repo = fixture.repo().unwrap();
        let loader_repo =
            Repository::open(repo.workdir().unwrap()).expect("loader's own repo handle");
        let mut loader_ts = crate::highlight::TsHighlighter::new();
        let views = build_file_views(&loader_repo, &mut loader_ts, &spec);

        let LoadedViews::Single(role, Some(loader_view)) = views else {
            panic!("expected a loaded Combined-role view");
        };
        assert_eq!(role, Role::Combined);
        assert_eq!(loader_view.old_text(), eager_view.old_text());
        assert_eq!(loader_view.new_text(), eager_view.new_text());
        assert_eq!(loader_view.display.len(), eager_view.display.len());
    }

    #[test]
    fn apply_file_ready_completes_a_pending_open_byte_identical_to_eager_open() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "tracked.txt",
                "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold\nl10\nl11\nl12\n",
                "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew\nl10\nl11\nl12\n",
            )
            .build()
            .unwrap();

        let (mut deferred, eager) = defer_and_eager_twins(&fixture);
        assert!(deferred.open_pending());

        // The ASYNC path: take the pending spec (as `tui.rs`'s event loop would on the debounce
        // `Tick`), build its views through a SEPARATE repo/highlighter (standing in for the
        // loader thread's own), then apply the result — never `complete_pending_open`.
        let (gen, cs_idx, file_idx, spec) = deferred
            .take_pending_load_spec()
            .expect("a fresh pending open has an undispatched spec");
        let repo = fixture.repo().unwrap();
        let loader_repo = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut loader_ts = crate::highlight::TsHighlighter::new();
        let views = build_file_views(&loader_repo, &mut loader_ts, &spec);

        deferred.apply_file_ready(gen, cs_idx, file_idx, Ok(views));

        assert!(
            !deferred.open_pending(),
            "apply_file_ready must clear the pending flag for the file it just seated"
        );
        assert_eq!(
            deferred.cursor, eager.cursor,
            "cursor must land on the same (first-hunk) row an eager open would have"
        );
        assert_eq!(deferred.scroll, eager.scroll);
        let deferred_view = deferred.current_view_ref().expect("view now loaded");
        let eager_view = eager.current_view_ref().expect("eager view loaded");
        assert_eq!(deferred_view.old_text(), eager_view.old_text());
        assert_eq!(deferred_view.new_text(), eager_view.new_text());
        assert_eq!(deferred_view.display.len(), eager_view.display.len());
    }

    #[test]
    fn apply_file_ready_redispatches_when_zoom_outran_the_in_flight_load() {
        // F2 regression: a zoom cycled mid-load must not let the stale-shaped in-flight result
        // seat and clear the pending open — the new zoom's view would then never be dispatched.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file("f.txt", "committed\n", "staged\n", "workdir\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        // Default zoom is `Split`; this file has both staged and unstaged sub-diffs, so the
        // effective zoom stays `Split` too.
        app.open_current();
        assert!(app.open_pending(), "deferred open must be pending");

        let (gen, cs_idx, file_idx, spec) = app
            .take_pending_load_spec()
            .expect("first take dispatches against the Split zoom");
        assert_eq!(spec.zoom, EffectiveZoom::Split);

        // Mid-load `z`: CycleZoom is exempt from force-completion, so this re-defers the open
        // against the NEW zoom instead of blocking for it.
        app.cycle_zoom();
        assert!(
            app.open_pending(),
            "cycling zoom while a load is pending must still be pending"
        );

        // The loader answers the now-STALE (Split) request.
        let repo = fixture.repo().unwrap();
        let loader_repo = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut loader_ts = crate::highlight::TsHighlighter::new();
        let views = build_file_views(&loader_repo, &mut loader_ts, &spec);

        app.apply_file_ready(gen, cs_idx, file_idx, Ok(views));

        assert!(
            app.open_pending(),
            "a stale-shaped result must not clear the pending open"
        );
        assert!(
            app.take_pending_load_spec().is_some(),
            "the next Tick must re-dispatch against the current (Combined) zoom"
        );
    }

    #[test]
    fn take_pending_load_spec_dispatches_at_most_once_per_pending_open() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current();
        assert!(
            app.take_pending_load_spec().is_some(),
            "first take dispatches"
        );
        assert!(
            app.take_pending_load_spec().is_none(),
            "a second take before the result lands must not re-dispatch"
        );
    }

    #[test]
    fn take_pending_load_spec_is_none_for_a_fileless_changeset_without_panicking() {
        // A clean uncommitted layer diffs to zero files. A pending open onto it must not panic
        // `current_load_spec`'s file-list indexing — F7's regression. Where this test was born
        // (the loader changeset), a fileless `open_current` still MARKED the open pending and
        // only the spec-building was total; this changeset's flag hygiene supersedes that —
        // a fileless open now never marks (and actively clears) `open_pending`, so both the
        // flag and the spec must come back empty.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert!(app.files().is_empty(), "fixture must have no diffed files");
        app.set_defer_loads(true);
        app.open_current();
        assert!(
            !app.open_pending(),
            "a fileless open never marks pending (the non-deferred path clears the flags)"
        );

        assert!(
            app.take_pending_load_spec().is_none(),
            "no spec can be built for a file that doesn't exist"
        );
    }

    #[test]
    fn apply_file_ready_drops_a_stale_generation_result() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current();
        let (gen, cs_idx, file_idx, spec) = app.take_pending_load_spec().unwrap();

        let repo = fixture.repo().unwrap();
        let loader_repo = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut loader_ts = crate::highlight::TsHighlighter::new();
        let views = build_file_views(&loader_repo, &mut loader_ts, &spec);

        // A refresh between dispatch and result bumps the generation — the result now belongs
        // to a world that no longer exists and must be dropped outright, leaving `open_pending`
        // untouched (a FRESH open may since be pending for a different generation).
        app.generation += 1;
        app.apply_file_ready(gen, cs_idx, file_idx, Ok(views));

        assert!(
            app.open_pending(),
            "a stale-generation result must not clear a (possibly fresh) pending open"
        );
        assert!(
            app.current_view_ref().is_none(),
            "a stale-generation result must not populate the view cache"
        );
    }

    #[test]
    fn apply_file_ready_caches_a_result_even_after_navigating_away() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .unstaged_file("b.txt", "two\n", "two\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_zoom(Zoom::Combined);
        app.set_defer_loads(true);
        app.open_current(); // a.txt: uncached — defers
        let (gen, cs_idx, file_idx, spec) = app.take_pending_load_spec().unwrap();
        assert_eq!(file_idx, 0);

        // Navigate away from a.txt BEFORE the (simulated) loader result lands.
        app.current = 1;
        app.open_current();

        let repo = fixture.repo().unwrap();
        let loader_repo = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut loader_ts = crate::highlight::TsHighlighter::new();
        let views = build_file_views(&loader_repo, &mut loader_ts, &spec);
        app.apply_file_ready(gen, cs_idx, file_idx, Ok(views));

        // Still within the same generation — the result is warmth, not staleness: a.txt's cache
        // is populated even though the user is no longer looking at it.
        assert!(
            app.role_view_ref(0, Role::Combined).is_some(),
            "a within-generation result must cache even after the user navigated away"
        );
    }

    #[test]
    fn apply_file_ready_discards_a_result_for_an_already_cached_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_zoom(Zoom::Combined);
        app.ensure_loaded(0); // eagerly cached already
        assert!(app.role_view_ref(0, Role::Combined).is_some());
        let old_text_before = app
            .role_view_ref(0, Role::Combined)
            .unwrap()
            .old_text()
            .to_string();

        // A result claiming NOTHING loaded for this role (e.g. a stale/racing answer) must not
        // clobber the already-cached view — the loader is a pure cache-warmer, never an
        // overwriter.
        app.apply_file_ready(
            app.generation(),
            app.current_cs(),
            0,
            Ok(LoadedViews::Single(Role::Combined, None)),
        );

        let view = app.role_view_ref(0, Role::Combined).expect("still cached");
        assert_eq!(view.old_text(), old_text_before);
    }

    #[test]
    fn apply_file_ready_err_surfaces_a_footer_notice_and_clears_pending() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current();
        let (gen, cs_idx, file_idx, _spec) = app.take_pending_load_spec().unwrap();
        assert!(app.notice.is_none());

        app.apply_file_ready(gen, cs_idx, file_idx, Err("boom".to_string()));

        assert!(
            !app.open_pending(),
            "a failed load must not strand the placeholder pending forever"
        );
        let notice = app
            .notice
            .as_ref()
            .expect("a failed load surfaces a notice");
        assert_eq!(notice.severity, Severity::Error);
        assert!(notice.text.contains("boom"));
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
        DisplayRow::Gap { key: 0, skipped }
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
    fn app_next_and_prev_hunk_row_move_cursor_between_hunks() {
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
        let first_hunk_row = app.cursor;

        app.next_hunk_row();
        assert!(
            app.cursor > first_hunk_row,
            "should move the cursor to the later hunk"
        );
        let second_hunk_row = app.cursor;

        // No hunk after the last one: no-op.
        app.next_hunk_row();
        assert_eq!(app.cursor, second_hunk_row);

        app.prev_hunk_row();
        assert_eq!(app.cursor, first_hunk_row);

        // No hunk before the first one: no-op.
        app.prev_hunk_row();
        assert_eq!(app.cursor, first_hunk_row);
    }

    #[test]
    fn cursor_move_after_hunk_jump_is_sane_when_whole_file_fits_one_pane() {
        // A small file (fits in the default pane height) with two hunks: `max_scroll() == 0`.
        // Under the OLD scroll-primary model (M3), `next_hunk_row` jumped raw `scroll` to the
        // second hunk's row unclamped, over-scrolling past `max_scroll()`, and `scroll_by` had a
        // "don't snap backward" carve-out just to keep that over-scrolled position sane on the
        // very next relative move. The cursor-primary model makes the carve-out unnecessary:
        // `scroll` is *derived* from `cursor` and `max_scroll()`, so it's simply pinned to 0 for
        // a file that fits in one pane — there's nothing to snap back from, and `cursor` itself
        // just moves normally.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "small.txt",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n",
                "1\nCHANGED\n3\n4\n5\n6\n7\n8\n9\nCHANGED_TOO\n11\n",
            )
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(app.max_scroll(), 0, "whole file must fit in one pane");

        let first_hunk_cursor = app.cursor;
        app.next_hunk_row();
        let second_hunk_cursor = app.cursor;
        assert!(
            second_hunk_cursor > first_hunk_cursor,
            "hunk jump should move the cursor forward"
        );
        assert_eq!(
            app.scroll, 0,
            "scroll stays pinned to 0 — the whole file already fits in the pane"
        );

        let last_row = app.current_view_ref().unwrap().display.len() - 1;

        // `j` (cursor +1) after the hunk jump moves the cursor further, not backward.
        app.move_cursor_by(1);
        assert_eq!(app.cursor, (second_hunk_cursor + 1).min(last_row));
        assert_eq!(app.scroll, 0);

        // `k` (cursor -1) moves back up normally — no special-cased snap needed anymore.
        app.move_cursor_by(-1);
        assert_eq!(app.cursor, second_hunk_cursor);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn move_cursor_by_clamps_at_file_edges() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", "1\n2\n3\n4\n5\n", "1\nCHANGED\n3\n4\n5\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let last_row = app.current_view_ref().unwrap().display.len() - 1;

        app.move_cursor_by(-1000);
        assert_eq!(app.cursor, 0, "cursor must clamp at row 0, not go negative");

        app.move_cursor_by(1000);
        assert_eq!(
            app.cursor, last_row,
            "cursor must clamp at the last row, not run past it"
        );
    }

    #[test]
    fn derive_scroll_keeps_scrolloff_margin_and_slides_minimally() {
        // 40 single-line rows, no changes worth hunk-jumping over — this test is purely about
        // the cursor/scroll follow relationship, not hunk content.
        let lines: String = (1..=40).map(|n| format!("l{n}\n")).collect();
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("big.txt", &lines)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.ensure_loaded(0);
        app.pane_height = 10;
        app.cursor = 0;
        app.scroll = 0;

        // Bottom margin is `pane_height - 1 - SCROLLOFF` = 7: the cursor can move down to row 7
        // without `scroll` moving at all.
        app.move_cursor_by(7);
        assert_eq!(app.cursor, 7);
        assert_eq!(
            app.scroll, 0,
            "scroll must not move before the cursor hits the margin"
        );

        // One more step crosses the bottom margin: scroll slides by exactly 1, the minimum
        // needed to keep the cursor `SCROLLOFF` rows from the bottom — not a re-center.
        app.move_cursor_by(1);
        assert_eq!(app.cursor, 8);
        assert_eq!(
            app.scroll, 1,
            "scroll should slide by the minimum amount, not re-center"
        );

        // Scrolling back up mirrors the same margin on the top edge (SCROLLOFF = 2): with
        // `scroll` at 1, the cursor can move back up to `scroll + SCROLLOFF` = 3 before `scroll`
        // follows.
        app.move_cursor_by(-5);
        assert_eq!(app.cursor, 3);
        assert_eq!(
            app.scroll, 1,
            "scroll must not move while the cursor is still within the top margin"
        );
        app.move_cursor_by(-1);
        assert_eq!(app.cursor, 2);
        assert_eq!(
            app.scroll, 0,
            "scroll should slide back by the minimum amount once the cursor crosses the top margin"
        );
    }

    #[test]
    fn move_cursor_by_on_empty_file_list_does_not_panic() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert!(app.files().is_empty(), "fixture must have no dirty files");

        app.move_cursor_by(5);
        app.move_cursor_by(-5);
        app.scroll_top();
        app.scroll_bottom();
        app.next_hunk_row();
        app.prev_hunk_row();
        app.toggle_layout();

        assert_eq!(app.cursor, 0);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn toggle_layout_flips_and_persists_across_file_nav() {
        use super::Layout;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .untracked_file("b.txt", "hello\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert_eq!(app.layout, Layout::Sbs, "default layout is side-by-side");

        app.toggle_layout();
        assert_eq!(app.layout, Layout::Inline);

        // Navigating files must not reset the layout choice.
        app.next_file();
        assert_eq!(
            app.layout,
            Layout::Inline,
            "layout must persist across next_file"
        );
        app.prev_file();
        assert_eq!(
            app.layout,
            Layout::Inline,
            "layout must persist across prev_file"
        );

        app.toggle_layout();
        assert_eq!(app.layout, Layout::Sbs, "toggling back returns to Sbs");
    }

    #[test]
    fn inline_word_spans_cache_and_peek_round_trip() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", "old word here\n", "new word here\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.ensure_loaded(0);
        let view = app.current_view().unwrap();

        // The single change block here is a 1-del/1-add pair: inline row 0 is the Del, row 1 is
        // the paired Add.
        assert!(view.inline[0].is_word_diff_pair());
        assert!(view.inline[1].is_word_diff_pair());

        // Uncached before the populating call.
        assert_eq!(view.peek_inline_word_spans(0), (Vec::new(), Vec::new()));

        let (old_spans, new_spans) = view.inline_word_spans_for_row(0);
        assert!(!old_spans.is_empty(), "expected the changed word's span");

        // Now cached: peek returns the same spans without recomputing.
        assert_eq!(view.peek_inline_word_spans(0), (old_spans, new_spans));
    }

    #[test]
    fn inline_layout_scroll_bottom_reaches_full_tail() {
        use super::Layout;

        // Every line paired-changed: each display row (one Del/Add pair) expands to TWO inline
        // rows (Del then Add), so `inline.len() > display.len()` — under the F1 bug, scroll
        // bounds were still clamped against the shorter `display` length, leaving the inline
        // tail unreachable.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", "1\n2\n3\n4\n5\n", "a\nb\nc\nd\ne\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.layout = Layout::Inline;
        app.pane_height = 2;

        let inline_len = app.current_view_ref().unwrap().inline.len();
        let display_len = app.current_view_ref().unwrap().display.len();
        assert!(
            inline_len > display_len,
            "paired changed lines must expand under inline layout"
        );

        app.scroll_bottom();
        assert_eq!(
            app.scroll,
            inline_len - app.pane_height,
            "scroll_bottom must reach the inline tail, not the shorter SBS tail"
        );
    }

    #[test]
    fn inline_layout_next_and_prev_hunk_row_jump_between_change_blocks() {
        use super::Layout;

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
        app.ensure_loaded(0);
        app.layout = Layout::Inline;
        app.cursor = 0;
        app.scroll = 0;

        app.next_hunk_row();
        let first_block_row = app.cursor;
        assert!(
            matches!(
                app.current_view_ref().unwrap().inline[first_block_row],
                InlineRow::Del { .. } | InlineRow::Add { .. }
            ),
            "next_hunk_row must land on a Del/Add inline row"
        );

        app.next_hunk_row();
        let second_block_row = app.cursor;
        assert!(
            second_block_row > first_block_row,
            "should jump to the later change block"
        );
        assert!(matches!(
            app.current_view_ref().unwrap().inline[second_block_row],
            InlineRow::Del { .. } | InlineRow::Add { .. }
        ));

        app.prev_hunk_row();
        assert_eq!(
            app.cursor, first_block_row,
            "prev_hunk_row should return to the earlier block"
        );
    }

    #[test]
    fn toggle_layout_clamps_cursor_and_rederives_scroll() {
        use super::Layout;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", "1\n2\n3\n4\n5\n", "a\nb\nc\nd\ne\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.layout = Layout::Inline;
        app.pane_height = 2;
        app.scroll_bottom();
        assert!(app.scroll > 0, "inline scroll should be over the SBS max");
        let inline_last_row = app.cursor;

        app.toggle_layout();
        assert_eq!(app.layout, Layout::Sbs, "toggling back returns to Sbs");
        let sbs_rows = app.current_view_ref().unwrap().display.len();
        assert!(
            app.cursor < sbs_rows,
            "cursor must be clamped into the new (shorter) SBS row range, was {} of {}",
            app.cursor,
            sbs_rows
        );
        assert!(
            app.cursor <= inline_last_row,
            "clamping should only ever pull the cursor down, never push it further out"
        );
        assert!(
            app.scroll <= app.max_scroll(),
            "scroll must be clamped to the new layout's max_scroll"
        );
    }

    // ---- M4 zoom: gate, cycling, and split per-pane state ----------------------------------

    /// A file with three genuinely distinct HEAD / index / worktree states — so it has BOTH a
    /// staged sub-diff (HEAD ↔ index) and an unstaged one (index ↔ worktree), the precondition
    /// for the split. The changed tokens on each side (`BETAEDIT` staged, `GAMMAEDIT` unstaged)
    /// don't collide with the `UNSTAGED`/`STAGED` captions.
    fn partially_staged_fixture() -> Fixture {
        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file(
                "f.txt",
                "alpha\nbeta\ngamma\n",
                "alpha\nBETAEDIT\ngamma\n",
                "alpha\nBETAEDIT\nGAMMAEDIT\n",
            )
            .build()
            .unwrap()
    }

    #[test]
    fn effective_zoom_gate_truth_table() {
        use super::effective_zoom;
        use super::EffectiveZoom::{Single, Split};
        use super::Role::{Combined, Staged, Unstaged};
        use super::Zoom;

        // Not stageable (binary) collapses to Combined regardless of the requested zoom or which
        // sub-diffs exist.
        for req in [Zoom::Split, Zoom::Combined, Zoom::Unstaged, Zoom::Staged] {
            for hu in [false, true] {
                for hs in [false, true] {
                    assert_eq!(
                        effective_zoom(req, hu, hs, false),
                        Single(Combined),
                        "req={req:?} hu={hu} hs={hs} can_stage=false"
                    );
                }
            }
        }

        // Combined requested: always Combined.
        for hu in [false, true] {
            for hs in [false, true] {
                assert_eq!(
                    effective_zoom(Zoom::Combined, hu, hs, true),
                    Single(Combined)
                );
            }
        }

        // Unstaged requested: its sub-diff if present, else Combined.
        assert_eq!(
            effective_zoom(Zoom::Unstaged, true, false, true),
            Single(Unstaged)
        );
        assert_eq!(
            effective_zoom(Zoom::Unstaged, true, true, true),
            Single(Unstaged)
        );
        assert_eq!(
            effective_zoom(Zoom::Unstaged, false, true, true),
            Single(Combined)
        );
        assert_eq!(
            effective_zoom(Zoom::Unstaged, false, false, true),
            Single(Combined)
        );

        // Staged requested: its sub-diff if present, else Combined.
        assert_eq!(
            effective_zoom(Zoom::Staged, false, true, true),
            Single(Staged)
        );
        assert_eq!(
            effective_zoom(Zoom::Staged, true, true, true),
            Single(Staged)
        );
        assert_eq!(
            effective_zoom(Zoom::Staged, true, false, true),
            Single(Combined)
        );
        assert_eq!(
            effective_zoom(Zoom::Staged, false, false, true),
            Single(Combined)
        );

        // Split requested: Split only with BOTH; else downgrade to the single sub-diff; else
        // Combined.
        assert_eq!(effective_zoom(Zoom::Split, true, true, true), Split);
        assert_eq!(
            effective_zoom(Zoom::Split, true, false, true),
            Single(Unstaged)
        );
        assert_eq!(
            effective_zoom(Zoom::Split, false, true, true),
            Single(Staged)
        );
        assert_eq!(
            effective_zoom(Zoom::Split, false, false, true),
            Single(Combined)
        );
    }

    #[test]
    fn partially_staged_file_resolves_to_split_by_default() {
        use super::EffectiveZoom;

        let fixture = partially_staged_fixture();
        let app = app_from_fixture(&fixture);
        assert_eq!(app.zoom, super::Zoom::Split, "default zoom is split");
        assert_eq!(
            app.effective_zoom_for(0),
            EffectiveZoom::Split,
            "a file with both a staged and an unstaged sub-diff renders as a split"
        );
    }

    #[test]
    fn sub_view_context_text_reads_the_index_not_the_worktree() {
        use super::Role;

        // HEAD: alpha/beta/gamma; index: alpha/BETAEDIT/gamma; worktree: alpha/BETAEDIT/GAMMAEDIT.
        // The staged view (HEAD ↔ index) diffs beta→BETAEDIT and leaves gamma as a CONTEXT line —
        // whose new side is the index copy "gamma", NOT the worktree's "GAMMAEDIT". Symmetrically,
        // the unstaged view (index ↔ worktree) diffs gamma→GAMMAEDIT and leaves BETAEDIT context,
        // whose old side is the index copy "BETAEDIT", NOT HEAD's "beta". Before per-role text
        // sourcing, both sub-views read old=HEAD/new=worktree, so these context lines showed one
        // revision on one side and a different one on the other.
        let fixture = partially_staged_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // loads both split roles

        let staged = app
            .role_view_ref(0, Role::Staged)
            .expect("staged sub-view loaded");
        assert!(
            staged.new_text().contains("gamma") && !staged.new_text().contains("GAMMAEDIT"),
            "staged pane's new side must be the INDEX copy (gamma), not the worktree \
             (GAMMAEDIT); got: {:?}",
            staged.new_text()
        );

        let unstaged = app
            .role_view_ref(0, Role::Unstaged)
            .expect("unstaged sub-view loaded");
        assert!(
            unstaged.old_text().contains("BETAEDIT"),
            "unstaged pane's old side must be the INDEX copy (BETAEDIT), not HEAD (beta); \
             got: {:?}",
            unstaged.old_text()
        );
    }

    #[test]
    fn cycle_zoom_walks_the_four_states_and_persists_across_file_nav() {
        use super::Zoom;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file(
                "f.txt",
                "alpha\nbeta\ngamma\n",
                "alpha\nBETAEDIT\ngamma\n",
                "alpha\nBETAEDIT\nGAMMAEDIT\n",
            )
            .untracked_file("z_other.txt", "hello\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        assert_eq!(app.zoom, Zoom::Split, "default");
        app.cycle_zoom();
        assert_eq!(app.zoom, Zoom::Combined);
        app.cycle_zoom();
        assert_eq!(app.zoom, Zoom::Unstaged);
        app.cycle_zoom();
        assert_eq!(app.zoom, Zoom::Staged);
        app.cycle_zoom();
        assert_eq!(app.zoom, Zoom::Split, "cycles back to split");

        // Persists across file navigation, like layout.
        app.cycle_zoom(); // -> Combined
        assert_eq!(app.zoom, Zoom::Combined);
        app.next_file();
        assert_eq!(
            app.zoom,
            Zoom::Combined,
            "zoom must persist across next_file"
        );
        app.prev_file();
        assert_eq!(app.zoom, Zoom::Combined, "and across prev_file");
    }

    #[test]
    fn split_panes_keep_independent_cursors_and_swap_on_focus_toggle() {
        use super::SplitPane;

        let fixture = partially_staged_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        // The top (unstaged) pane is focused; each pane opened at its own role's first hunk.
        assert_eq!(app.split_focus, SplitPane::Unstaged);
        let staged_cursor = app.alt.cursor;

        // Moving the focused (unstaged) cursor must not disturb the stashed staged pane.
        app.scroll_top();
        assert_eq!(app.cursor, 0, "focused pane cursor moved to the top");
        assert_eq!(
            app.alt.cursor, staged_cursor,
            "the unfocused pane's cursor is independent of focused-pane moves"
        );

        // `w` swaps the two panes' state and flips focus to the bottom (staged) pane.
        app.toggle_split_focus();
        assert_eq!(app.split_focus, SplitPane::Staged);
        assert_eq!(
            app.cursor, staged_cursor,
            "focus swap brings the staged pane's own cursor into the focused slot"
        );
        assert_eq!(
            app.alt.cursor, 0,
            "the moved unstaged cursor is stashed as the now-unfocused pane"
        );
    }

    #[test]
    fn toggle_split_focus_is_a_noop_outside_a_split() {
        use super::SplitPane;

        // An untracked file has only an unstaged sub-diff, so the default split downgrades to a
        // single unstaged pane — there is no second pane to focus.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("only.txt", "one\ntwo\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        assert_eq!(app.split_focus, SplitPane::Unstaged);
        app.toggle_split_focus();
        assert_eq!(
            app.split_focus,
            SplitPane::Unstaged,
            "focus toggle must do nothing when the file isn't a split"
        );
    }

    #[test]
    fn notify_sets_a_notice_with_the_given_text_and_severity() {
        use super::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        assert!(app.notice.is_none(), "no notice by default");

        app.notify("something went wrong", Severity::Error);
        let notice = app.notice.as_ref().expect("notice set by notify");
        assert_eq!(notice.text, "something went wrong");
        assert_eq!(notice.severity, Severity::Error);
    }

    #[test]
    fn clear_notice_clears_a_set_notice() {
        use super::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);

        app.notify("saved", Severity::Info);
        assert!(app.notice.is_some());

        app.clear_notice();
        assert!(app.notice.is_none(), "clear_notice must clear a set notice");
    }

    // ---- M4 refresh: in-place re-diff + rebuild -------------------------------------------

    #[test]
    fn refresh_after_external_worktree_edit_picks_up_the_change() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert!(!app.current_view_ref().unwrap().new_text().contains("THREE"));

        // Mutate the fixture repo's WORKTREE directly (not this crate's own working tree) — an
        // edit made outside the TUI, same as a user switching to another editor mid-review.
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join("a.txt"), "one\nCHANGED\nTHREE\n").unwrap();

        app.refresh();

        let view = app
            .current_view_ref()
            .expect("current file still has a view after refresh");
        assert!(
            view.new_text().contains("THREE"),
            "refresh must re-read the worktree file, got: {:?}",
            view.new_text()
        );
    }

    #[test]
    fn refresh_preserves_the_current_file_by_path() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .unstaged_file("b.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.next_file();
        assert_eq!(app.current, 1, "path-sorted: b.txt is index 1");
        let path = app.files()[app.current].path.clone();

        app.refresh();

        assert_eq!(
            app.files()[app.current].path,
            path,
            "refresh must keep tracking the same file by path"
        );
    }

    #[test]
    fn refresh_when_the_current_file_vanished_clamps_without_panicking() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .unstaged_file("b.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.next_file();
        assert_eq!(app.current, 1, "path-sorted: b.txt is index 1");

        // Revert b.txt's worktree copy back to its committed content, outside the TUI — its dirt
        // disappears, so the rebuilt file list no longer has an entry for it.
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join("b.txt"), "one\n").unwrap();

        app.refresh();

        assert_eq!(app.files().len(), 1, "only a.txt is still dirty");
        assert!(
            app.current < app.files().len(),
            "current must be clamped in-range, got {}",
            app.current
        );
        assert_eq!(app.files()[app.current].path, "a.txt");
    }

    #[test]
    fn refresh_failure_leaves_state_intact_and_sets_an_error_notice() {
        use super::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let files_before: Vec<String> = app.files().iter().map(|f| f.path.clone()).collect();

        // Corrupt the throwaway fixture repo's OWN `.git/HEAD` so `diff_uncommitted`'s
        // `repo.head()` call fails cheaply — never done against a real working tree.
        let repo = fixture.repo().unwrap();
        std::fs::write(repo.path().join("HEAD"), b"garbage-not-a-ref\n").unwrap();

        app.refresh();

        let files_after: Vec<String> = app.files().iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            files_after, files_before,
            "a failed refresh must leave existing state untouched"
        );
        let notice = app
            .notice
            .as_ref()
            .expect("refresh failure must set a notice");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("refresh failed"),
            "got notice text: {:?}",
            notice.text
        );
    }

    #[test]
    fn refresh_preserves_zoom_and_layout() {
        use super::{Layout, Zoom};

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.layout = Layout::Inline;
        app.zoom = Zoom::Combined;

        app.refresh();

        assert_eq!(app.layout, Layout::Inline, "refresh must not reset layout");
        assert_eq!(app.zoom, Zoom::Combined, "refresh must not reset zoom");
    }

    /// M7 CS2 fix: a session launched with an explicit `[SOURCE]` argument must have `refresh`
    /// re-resolve THAT source, never silently downgrade to no-argument auto-detect. A Graphite
    /// stack is active (`assemble_changesets` would return the whole `a`/`b` stack for
    /// auto-detect), but the session was launched with `uncommitted` — so both the manual `r`
    /// key and the tick-driven index watcher must keep showing only the single uncommitted
    /// changeset, not swap in the full stack.
    #[test]
    fn refresh_re_resolves_the_launched_source_instead_of_auto_detecting() {
        use crate::source::Source;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .graphite_config(&["main"])
            .branch_metadata("a", "main")
            .branch_metadata("b", "a")
            .untracked_file("scratch.txt", "hi\n")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        // `App::refresh` re-derives the branch from the repo's ACTUAL `HEAD`, not from a name
        // handed to `resolve_source` — so the fixture's checkout must really be on "b" for
        // auto-detect (were the fix absent) to see the `a`/`b` stack, not `main`.
        repo.set_head("refs/heads/b").unwrap();
        repo.checkout_head(None).unwrap();

        let source = Source::Uncommitted;
        let changesets =
            crate::source::resolve_source(repo, "b", source.clone()).expect("resolve_source");
        assert_eq!(
            changesets.len(),
            1,
            "uncommitted keyword always resolves to exactly one changeset"
        );
        let mut views = Vec::with_capacity(changesets.len());
        for cs in changesets {
            let diff = crate::acquire::diff_changeset(repo, &cs).unwrap();
            views.push(ChangesetView::from_changeset_diff(cs, diff));
        }

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, views);
        app.set_review_source(source);
        app.open_current();

        app.refresh();

        assert_eq!(
            app.changeset_count(),
            1,
            "refresh must keep reviewing only the uncommitted changeset, not the full \
             Graphite stack auto-detect would find"
        );
        assert_eq!(app.cur().cs.span, ChangesetSpan::Uncommitted);
    }

    /// A PR-sourced review must survive refresh untouched: re-resolving would hit the network
    /// (gh + fetch), so [`App::refresh`] no-ops for [`Source::Pr`]. The fixture has no PR and no
    /// gh — if refresh DID try to re-resolve, `resolve_pr` would fail and raise a "refresh
    /// failed" notice; asserting no notice (and unchanged views) pins the no-op.
    #[test]
    fn refresh_is_a_no_op_for_a_pr_source() {
        use crate::source::Source;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        fixture
            .commit("main")
            .file("a.txt", "one\n")
            .create("first")
            .unwrap();
        fixture
            .commit("main")
            .file("a.txt", "two\n")
            .create("second")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base = head.parent(0).unwrap();
        let cs = workon::Changeset {
            name: "pr-1".to_string(),
            span: ChangesetSpan::Committed {
                base: base.id(),
                head: head.id(),
            },
            title: Some("a pr".to_string()),
            current: true,
            needs_restack: false,
        };
        let diff = crate::acquire::diff_changeset(repo, &cs).unwrap();
        let views = vec![ChangesetView::from_changeset_diff(cs, diff)];

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, views);
        app.set_review_source(Source::Pr("pr-1".to_string()));
        app.open_current();

        app.refresh();

        assert_eq!(app.changeset_count(), 1);
        assert_eq!(app.cur().cs.name, "pr-1");
        assert!(
            app.notice.is_none(),
            "a PR-source refresh must no-op, not attempt (and fail) a network re-resolution"
        );
    }

    // ---- ADR-037 refresh: span-keyed reuse, uncommitted always sync, async waves ----------

    /// Build a two-commit chain (`root` then `head`) on the fixture's default branch and return
    /// both `Oid`s — the shared setup every span-keyed-reuse test below diffs a
    /// [`ChangesetSpan::Committed`] across.
    fn root_and_head_commits(fixture: &Fixture) -> (git2::Oid, git2::Oid) {
        let root = fixture
            .commit("main")
            .file("r.txt", "r\n")
            .create("root")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .file("b.txt", "b\n")
            .create("head")
            .unwrap();
        (root, head)
    }

    #[test]
    fn refresh_reuses_a_ready_committed_slot_with_unchanged_span_keeping_warm_caches() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let (root, head) = root_and_head_commits(&fixture);
        let repo = fixture.repo().unwrap();

        // `head_text: "main"` re-resolves through the branch ref on every refresh — the span
        // stays `Committed { base: root, head }` as long as `main` doesn't move, exercising the
        // REAL re-resolve path (not a hand-frozen span) for the "unchanged" case.
        let cs = Changeset {
            name: format!("{root}..main"),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.set_review_source(crate::source::Source::Range {
            base_text: root.to_string(),
            head_text: "main".to_string(),
            dots: crate::source::RangeDots::Two,
        });

        app.open_current(); // caches file 0 ("a.txt")
        assert_eq!(app.files().len(), 2, "root..head touches a.txt and b.txt");
        app.next_file(); // caches file 1 ("b.txt") too
        app.prev_file(); // back to file 0 — the file `refresh`'s tail will re-seat
        assert!(app.role_view_ref(1, Role::Combined).is_some());

        let gen_before = app.generation();
        app.refresh();

        assert_eq!(
            app.generation(),
            gen_before + 1,
            "every refresh bumps the generation, reused slot or not"
        );
        assert!(
            !app.is_current_pending(),
            "an unchanged span must be carried over Ready, never go through Pending"
        );
        assert_eq!(app.files().len(), 2);
        assert_eq!(app.current, 0, "file position by path is preserved");
        assert!(
            app.role_view_ref(1, Role::Combined).is_some(),
            "file 1's view cache must survive untouched — refresh's tail only (re)opens the \
             CURRENT file (0), so a still-populated cache at 1 proves the whole ChangesetView \
             (not just its diff) was carried over rather than rebuilt fresh"
        );
        assert!(
            app.take_pending_wave().is_none(),
            "a fully-reused refresh has nothing left to diff asynchronously"
        );
    }

    #[test]
    fn refresh_sends_a_changed_committed_span_through_a_wave_instead_of_reusing_it() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let (root, old_head) = root_and_head_commits(&fixture);
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: format!("{root}..main"),
            span: ChangesetSpan::Committed {
                base: root,
                head: old_head,
            },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.set_review_source(crate::source::Source::Range {
            base_text: root.to_string(),
            head_text: "main".to_string(),
            dots: crate::source::RangeDots::Two,
        });
        app.open_current();

        // Advance `main` past `old_head` — the same shape as a real amend/restack: the name
        // ("{root}..main") stays identical, but the span's `head` moves.
        let new_head = fixture
            .commit("main")
            .file("c.txt", "c\n")
            .create("new head")
            .unwrap();

        let gen_before = app.generation();
        app.refresh();
        let gen_after = app.generation();
        assert_eq!(gen_after, gen_before + 1);

        assert!(
            app.is_current_pending(),
            "a changed span must NOT be reused — it goes Pending for the wave to diff"
        );
        assert!(app.files().is_empty());

        let (wave_gen, to_diff) = app
            .take_pending_wave()
            .expect("a changed committed span must queue a wave request");
        assert_eq!(wave_gen, gen_after);
        assert_eq!(to_diff.len(), 1);
        assert_eq!(
            to_diff[0].0, 0,
            "the stack index the result must be seated at"
        );
        assert_eq!(
            to_diff[0].1.span,
            ChangesetSpan::Committed {
                base: root,
                head: new_head,
            },
            "the wave must diff the NEW span, not the stale one"
        );
        assert!(
            app.take_pending_wave().is_none(),
            "take_pending_wave is a take-once — a second call must find nothing left"
        );

        // A stale-generation result (as if it were still in flight for the pre-refresh world)
        // must be dropped outright.
        app.apply_changeset_ready(
            gen_before,
            0,
            Ok(crate::acquire::ChangesetDiff::Committed(
                crate::acquire::diff_committed(repo, root, new_head).unwrap(),
            )),
        );
        assert!(
            app.is_current_pending(),
            "a stale-generation ChangesetReady must be dropped, not seat the changeset"
        );

        // The NEW generation's result lands and seats the (still active) changeset.
        app.apply_changeset_ready(
            gen_after,
            0,
            Ok(crate::acquire::ChangesetDiff::Committed(
                crate::acquire::diff_committed(repo, root, new_head).unwrap(),
            )),
        );
        assert!(!app.is_current_pending());
        assert_eq!(
            app.files().len(),
            3,
            "root..new_head accumulates a.txt/b.txt (from `head`) and c.txt (from `new_head`)"
        );
        assert_eq!(
            app.cur().cs.span,
            ChangesetSpan::Committed {
                base: root,
                head: new_head,
            }
        );
    }

    #[test]
    fn refresh_that_makes_the_active_changeset_pending_clears_stale_deferred_open_flags() {
        // F1 regression: a pending open still in flight (dispatched, awaiting a loader result)
        // right before `r`, followed by a refresh that turns the active changeset Pending (a
        // changed span goes through the wave, landing empty files) must not leave
        // `open_pending`/`open_pending_dispatched` wedged. `open_current`'s empty-file guard
        // skips the defer branch (nothing to load) after the refresh, so nothing else would
        // clear stale flags without this fix.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let (root, old_head) = root_and_head_commits(&fixture);
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: format!("{root}..main"),
            span: ChangesetSpan::Committed {
                base: root,
                head: old_head,
            },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.set_review_source(crate::source::Source::Range {
            base_text: root.to_string(),
            head_text: "main".to_string(),
            dots: crate::source::RangeDots::Two,
        });
        app.set_defer_loads(true);
        app.open_current();
        let _ = app
            .take_pending_load_spec()
            .expect("a fresh pending open dispatches");
        assert!(app.open_pending(), "the open is pending, awaiting a result");

        // Advance `main`, changing the span — refresh must send this through the wave, landing
        // the active changeset Pending (empty files) rather than reusing the stale slot.
        fixture
            .commit("main")
            .file("c.txt", "c\n")
            .create("new head")
            .unwrap();

        app.refresh();

        assert!(
            app.is_current_pending(),
            "a changed span must go Pending for the wave"
        );
        assert!(app.files().is_empty());
        assert!(
            !app.open_pending(),
            "a stale deferred open must not survive a refresh that empties the active changeset"
        );
        assert!(
            app.take_pending_load_spec().is_none(),
            "no load spec can be produced for a Pending slot with no files"
        );
    }

    #[test]
    fn refresh_retries_a_failed_committed_slot_via_a_wave_rather_than_reusing_it() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let (root, head) = root_and_head_commits(&fixture);
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: format!("{root}..main"),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        // Seed the slot as `Failed` for this exact (name, span) — as if a previous wave's diff
        // for it had errored.
        let view = ChangesetView::failed(cs, "a previous diff attempt failed");
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.set_review_source(crate::source::Source::Range {
            base_text: root.to_string(),
            head_text: "main".to_string(),
            dots: crate::source::RangeDots::Two,
        });
        assert!(app.current_failure().is_some());

        app.refresh(); // span is UNCHANGED from the Failed slot's — reuse must still skip it

        assert!(
            app.is_current_pending(),
            "reuse only carries `Ready` slots — a `Failed` one goes back through Pending+wave, \
             which is what makes `r` a retry with no separate retry machinery"
        );
        let (_, to_diff) = app
            .take_pending_wave()
            .expect("the retried span must be queued for the wave");
        assert_eq!(to_diff.len(), 1);
        assert_eq!(
            to_diff[0].1.span,
            ChangesetSpan::Committed { base: root, head }
        );
    }

    #[test]
    fn coordinated_refresh_after_staging_rebuilds_the_uncommitted_layer_synchronously_while_reusing_an_unchanged_committed_span(
    ) {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .graphite_config(&["main"])
            .branch_metadata("a", "main")
            .unstaged_file("dirty.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        repo.set_head("refs/heads/a").unwrap();
        repo.checkout_head(None).unwrap();

        let changesets = crate::acquire::resolve_changesets(repo, "a").unwrap();
        assert_eq!(
            changesets.len(),
            2,
            "expected the 'a' Graphite node plus the dirty tree's uncommitted layer"
        );
        let diffs = crate::acquire::diff_changesets(repo, &changesets).unwrap();
        let views: Vec<ChangesetView> = changesets
            .into_iter()
            .zip(diffs)
            .map(|(cs, diff)| ChangesetView::from_changeset_diff(cs, diff))
            .collect();

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, views);
        assert_eq!(
            app.cur().cs.span,
            ChangesetSpan::Uncommitted,
            "opens on the uncommitted layer (lib-`current`)"
        );

        app.open_current(); // cursor lands on dirty.txt's one hunk
        app.stage_hunk(); // run_op -> coordinated_refresh -> refresh, synchronously

        // The post-op world is visible SYNCHRONOUSLY, before the next event loop iteration.
        repo.assert(predicate::repo::has_staged_file("dirty.txt"));
        assert!(
            !app.is_current_pending(),
            "the uncommitted layer always re-diffs sync — it must never go through Pending"
        );
        assert!(
            app.take_pending_wave().is_none(),
            "the 'a' node's committed span didn't change — staging must not have dispatched a \
             wave for it"
        );
    }

    #[test]
    fn refresh_marks_the_uncommitted_layer_failed_when_its_sync_diff_errors() {
        use super::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_review_source(crate::source::Source::Uncommitted);
        // Deliberately do NOT call `app.open_current()` here: reading a file's content already
        // walks `HEAD`'s commit/tree chain through `app`'s OWN `Repository` handle, which would
        // warm libgit2's per-handle object cache for the exact commit this test corrupts below —
        // a cache hit would silently mask the corruption instead of exercising the failure path.

        // Corrupt the loose object `HEAD` points to (not the `HEAD` ref itself): `repo.head()`'s
        // SHORTHAND still resolves fine (refresh's own branch-name read, and
        // `Source::Uncommitted`'s resolution, which is a pure string wrap needing no repo access
        // at all), but `diff_uncommitted`'s `repo.head()?.peel_to_tree()` — which walks all the
        // way to the commit object, through `app`'s never-yet-used-for-this-object handle — fails.
        let repo = fixture.repo().unwrap();
        let head_oid = repo.head().unwrap().target().unwrap();
        let hex = head_oid.to_string();
        let object_path = repo.path().join("objects").join(&hex[0..2]).join(&hex[2..]);
        // Loose objects are written read-only by git — reclaim write permission before
        // clobbering the bytes, or the write itself fails with EACCES.
        let mut perms = std::fs::metadata(&object_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&object_path, perms).unwrap();
        std::fs::write(&object_path, b"garbage-not-a-git-object\n").unwrap();

        app.refresh();

        assert!(
            !app.is_current_pending(),
            "a sync diff failure sets Failed, not Pending — nothing async is retrying this"
        );
        assert!(
            app.current_failure().is_some(),
            "the uncommitted layer's failed sync re-diff must become a Failed slot"
        );
        let notice = app
            .notice
            .as_ref()
            .expect("a failed uncommitted sync re-diff must set a footer notice");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("uncommitted diff failed"),
            "got notice text: {:?}",
            notice.text
        );
        assert!(app.take_pending_wave().is_none());
    }

    // ---- M4 index watcher (`on_tick`) -------------------------------------------------------

    /// Stage `path` in the fixture's index, exactly as an external `git add` would — the write
    /// [`App::on_tick`] is meant to notice, since [`crate::refresh::IndexSignature`] only
    /// fingerprints `.git/index` (not the worktree; see that module's docs for why the watcher is
    /// index-only, not a general filesystem watcher).
    fn stage_externally(fixture: &Fixture, path: &str) {
        let repo = fixture.repo().unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
    }

    #[test]
    fn on_tick_after_external_index_change_rebuilds_the_view() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert!(
            app.role_change(app.current, Role::Staged).is_none(),
            "a.txt starts unstaged only"
        );

        // An external `git add` — nothing this process did — changes `.git/index`'s signature.
        stage_externally(&fixture, "a.txt");

        app.on_tick();

        assert!(
            app.role_change(app.current, Role::Staged).is_some(),
            "on_tick must pick up the externally staged change"
        );
        assert!(
            app.role_change(app.current, Role::Unstaged).is_none(),
            "a.txt is now fully staged, no unstaged sub-diff remains"
        );
    }

    #[test]
    fn on_tick_with_unchanged_index_does_not_refresh() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        // `start_selection` is a marker a refresh always clears (`reset_panes`, called from
        // `open_current` at the end of every refresh, zeroes `selection_anchor`) — if it survives
        // a tick, no refresh ran.
        app.start_selection();
        assert!(app.selection_anchor.is_some());

        app.on_tick();
        app.on_tick();

        assert!(
            app.selection_anchor.is_some(),
            "an unchanged index must not trigger a refresh"
        );
    }

    #[test]
    fn on_tick_right_after_new_does_not_spuriously_refresh_the_initial_seed() {
        // `App::new` seeds the coordinator with the index signature as it stood at construction
        // time, so the FIRST tick — with nothing having changed since — must not treat that
        // baseline as a "new" external event.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.start_selection();
        assert!(app.selection_anchor.is_some());

        app.on_tick();

        assert!(
            app.selection_anchor.is_some(),
            "the seeded initial signature must suppress a spurious first-tick refresh"
        );
    }

    #[test]
    fn on_tick_immediately_after_a_staging_op_does_not_double_refresh() {
        // The whole point of recording the POST-refresh signature in `coordinated_refresh`: our
        // own staging write's echo must not look like a fresh external change to the very next
        // tick.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();

        app.stage_file();
        assert!(
            app.role_change(app.current, Role::Staged).is_some(),
            "stage_file must have staged the whole file"
        );

        // Marker: a refresh clears `selection_anchor` via `reset_panes`; `stage_file`'s own
        // `coordinated_refresh` already ran and cleared it once (fine, unobserved). Set a fresh
        // marker afterward so the NEXT tick's (non-)refresh is what's under test.
        app.start_selection();
        assert!(app.selection_anchor.is_some());

        app.on_tick();

        assert!(
            app.selection_anchor.is_some(),
            "the post-op signature recorded by coordinated_refresh must suppress the echo, \
             so this tick must not refresh again"
        );
    }

    // ---- M4 staging: hunk identity ---------------------------------------------------------

    /// A modified file whose only two changes are its first and last line, with a dozen unchanged
    /// lines between — so the two hunks are far enough apart to leave a collapsed gap between
    /// them (the between-hunks `None` case for the row→hunk mapping).
    fn two_hunk_fixture() -> Fixture {
        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file(
                "many.txt",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n",
                "ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\nTWENTY\n",
            )
            .build()
            .unwrap()
    }

    #[test]
    fn hunk_at_display_row_maps_change_rows_to_hunks_and_gap_to_none() {
        let fixture = two_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let view = app.current_view_ref().unwrap();

        // A gap row between the two hunks maps to no hunk.
        let gap_row = view
            .display
            .iter()
            .position(|r| matches!(r, DisplayRow::Gap { .. }))
            .expect("a collapsed gap sits between the two far-apart hunks");
        assert_eq!(view.hunk_at_display_row(gap_row), None);

        // The earliest change row belongs to hunk 0, the latest to hunk 1.
        let first_change = view
            .display
            .iter()
            .position(|r| matches!(r, DisplayRow::Row(a) if a.old_kind != CellKind::Context))
            .expect("hunk 0 change row");
        let last_change = view
            .display
            .iter()
            .rposition(|r| matches!(r, DisplayRow::Row(a) if a.old_kind != CellKind::Context))
            .expect("hunk 1 change row");
        assert_eq!(view.hunk_at_display_row(first_change), Some(0));
        assert_eq!(view.hunk_at_display_row(last_change), Some(1));
    }

    #[test]
    fn hunk_at_inline_row_maps_change_rows_to_hunks_and_gap_to_none() {
        let fixture = two_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let view = app.current_view_ref().unwrap();

        let gap_row = view
            .inline
            .iter()
            .position(|r| matches!(r, InlineRow::Gap { .. }))
            .expect("a collapsed gap sits between the two far-apart hunks");
        assert_eq!(view.hunk_at_inline_row(gap_row), None);

        let first_change = view
            .inline
            .iter()
            .position(|r| matches!(r, InlineRow::Del { .. } | InlineRow::Add { .. }))
            .expect("hunk 0 change row");
        let last_change = view
            .inline
            .iter()
            .rposition(|r| matches!(r, InlineRow::Del { .. } | InlineRow::Add { .. }))
            .expect("hunk 1 change row");
        assert_eq!(view.hunk_at_inline_row(first_change), Some(0));
        assert_eq!(view.hunk_at_inline_row(last_change), Some(1));
    }

    #[test]
    fn hunk_at_cursor_dispatches_on_layout_and_reports_none_between_hunks() {
        let fixture = two_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // cursor lands on the first hunk
        assert_eq!(app.hunk_at_cursor(), Some(0));

        // Park the cursor on the gap row → no hunk under it.
        let gap_row = app
            .current_view_ref()
            .unwrap()
            .display
            .iter()
            .position(|r| matches!(r, DisplayRow::Gap { .. }))
            .unwrap();
        app.cursor = gap_row;
        assert_eq!(app.hunk_at_cursor(), None);
    }

    // ---- M4 staging: verbs -----------------------------------------------------------------

    /// A file with three distinct HEAD/index/worktree states — both a staged and an unstaged
    /// sub-diff, and hunk-patchable (Modified). Same shape the zoom tests use.
    fn partial_fixture() -> Fixture {
        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file(
                "f.txt",
                "alpha\nbeta\ngamma\n",
                "alpha\nBETAEDIT\ngamma\n",
                "alpha\nBETAEDIT\nGAMMAEDIT\n",
            )
            .build()
            .unwrap()
    }

    #[test]
    fn stage_hunk_in_unstaged_pane_stages_the_hunk() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current(); // downgrades to a single unstaged pane; cursor on the hunk
        app.stage_hunk();

        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_staged_file("a.txt"));
        // The worktree copy is untouched by a stage.
        repo.assert(predicate::repo::workdir_file_equals(
            "a.txt",
            "one\nCHANGED\n",
        ));
    }

    #[test]
    fn stage_hunk_in_staged_pane_unstages_the_hunk() {
        use super::Zoom;

        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.zoom = Zoom::Staged;
        app.open_current();
        app.stage_hunk(); // staged pane → unstage direction

        // Unstaging the only staged hunk reverts the index entry to HEAD.
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::index_blob_equals(
            "f.txt",
            "alpha\nbeta\ngamma\n",
        ));
    }

    #[test]
    fn stage_file_in_unstaged_pane_stages_whole_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.stage_file();

        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_staged_file("a.txt"));
    }

    #[test]
    fn stage_file_in_staged_pane_unstages_whole_file() {
        use super::Zoom;

        // A freshly `git add`ed (Added) file has only a staged sub-diff.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("new.txt", "hello\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.zoom = Zoom::Staged;
        app.open_current();
        app.stage_file(); // staged pane → unstage; Added file has no HEAD entry, so it goes untracked

        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_untracked_file("new.txt"));
    }

    // ---- CS6: staging preserves diff position ----------------------------------------------

    /// Three single-line edits well-separated (>6 lines of pure context apart, git's own
    /// hunk-splitting threshold) so each is its own hunk AND the context between any two
    /// collapses to a [`DisplayRow::Gap`] — exercising both the mid-file-hunk and the
    /// lands-in-a-gap restore paths.
    fn three_hunk_fixture() -> Fixture {
        let head: String = (1..=24).map(|n| format!("L{n}\n")).collect();
        let worktree: String = (1..=24)
            .map(|n| {
                if n == 2 || n == 12 || n == 22 {
                    format!("L{n}X\n")
                } else {
                    format!("L{n}\n")
                }
            })
            .collect();
        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", &head, &worktree)
            .build()
            .unwrap()
    }

    /// The row-native lineno `App::restore_position` would target for `row` — new side,
    /// falling back to old — used by these tests to check where the cursor actually landed
    /// without re-deriving the production search itself.
    fn row_lineno(row: &DisplayRow) -> Option<usize> {
        match row {
            DisplayRow::Row(r) => match r.new {
                Row::Line(n) => Some(n),
                Row::Filler => match r.old {
                    Row::Line(n) => Some(n),
                    Row::Filler => None,
                },
            },
            DisplayRow::Gap { .. } => None,
        }
    }

    #[test]
    fn stage_hunk_on_a_middle_hunk_lands_the_cursor_near_it_not_at_the_first_hunk() {
        let fixture = three_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // Single(Unstaged): no staged half exists yet.
        let first_hunk_row = app.cursor;

        app.next_hunk_row(); // hunk 1 (line 2) -> hunk 2 (line 12)
        let hunk2_row = app.cursor;
        assert_ne!(
            hunk2_row, first_hunk_row,
            "test setup: must have moved off hunk 1"
        );

        app.stage_hunk(); // stages ONLY hunk 2 -> the file now has both sub-diffs again

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Split,
            "hunks 1/3 stayed unstaged, hunk 2 is now staged — both halves exist"
        );
        assert_eq!(
            app.split_focus_role(),
            Role::Unstaged,
            "the memento's own role (Unstaged) survives, so it stays the target"
        );
        assert_ne!(
            app.cursor, first_hunk_row,
            "must NOT reset to the first hunk (today's manual-nav-only behavior)"
        );

        let view = app.role_view_ref(app.current, Role::Unstaged).unwrap();
        let lineno = row_lineno(&view.display[app.cursor])
            .expect("restore must not land the cursor back on a Gap row");
        assert!(
            lineno > 2 && lineno < 22,
            "expected the cursor between hunk 1 (line 2) and hunk 3 (line 22) — near hunk 2's \
             old position (line 12) — got line {lineno}"
        );
    }

    #[test]
    fn fully_staging_a_file_in_split_lands_the_cursor_in_the_staged_pane_at_the_same_lines() {
        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // Split; focused pane defaults to Unstaged, on gamma's hunk (line 3)
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert_eq!(app.split_focus_role(), Role::Unstaged);

        app.stage_hunk(); // stages the only unstaged hunk -> the file is now fully staged

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Single(Role::Staged),
            "no unstaged half survives a full stage"
        );

        let view = app.role_view_ref(app.current, Role::Staged).unwrap();
        let staged_first_hunk_row = match app.layout {
            Layout::Sbs => view.first_hunk_row,
            Layout::Inline => view.first_inline_hunk_row,
        };
        assert_ne!(
            app.cursor, staged_first_hunk_row,
            "must land on gamma's own row, not beta's (the staged view's first hunk)"
        );
        let lineno = row_lineno(&view.display[app.cursor]).expect("gamma's row has a lineno");
        assert_eq!(
            lineno, 3,
            "gamma is line 3 in both HEAD and the fully-staged index"
        );
    }

    #[test]
    fn unstaging_in_the_staged_pane_keeps_focus_there_when_it_survives() {
        let head: String = (1..=14).map(|n| format!("L{n}\n")).collect();
        // Index stages two well-separated edits (lines 2 and 10); the worktree matches the
        // index except for one MORE edit (line 14) that was never staged.
        let index: String = (1..=14)
            .map(|n| {
                if n == 2 || n == 10 {
                    format!("L{n}X\n")
                } else {
                    format!("L{n}\n")
                }
            })
            .collect();
        let worktree: String = (1..=14)
            .map(|n| {
                if n == 2 || n == 10 || n == 14 {
                    format!("L{n}X\n")
                } else {
                    format!("L{n}\n")
                }
            })
            .collect();
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file("f.txt", &head, &index, &worktree)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current(); // Split; focused pane defaults to Unstaged (line 14's hunk)
        app.toggle_split_focus(); // -> Staged pane, cursor on hunk 1 (line 2)
        let first_hunk_row = app.cursor;
        app.next_hunk_row(); // -> hunk 2 (line 10)
        assert_ne!(
            app.cursor, first_hunk_row,
            "test setup: must have moved off hunk 1"
        );

        app.stage_hunk(); // staged pane -> unstage direction: reverts line 10's index entry

        assert!(
            app.notice.is_none(),
            "unstage must succeed: {:?}",
            app.notice
        );
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Split,
            "line 2 stays staged and line 10/14 are both unstaged now — both halves survive"
        );
        assert_eq!(
            app.split_focus_role(),
            Role::Staged,
            "the memento's own role (Staged) survives, so focus stays there"
        );

        let view = app.role_view_ref(app.current, Role::Staged).unwrap();
        let staged_first_hunk_row = match app.layout {
            Layout::Sbs => view.first_hunk_row,
            Layout::Inline => view.first_inline_hunk_row,
        };
        assert_ne!(
            app.cursor, staged_first_hunk_row,
            "must NOT reset to the (now sole) first hunk at line 2"
        );
    }

    #[test]
    fn discarding_the_only_file_in_the_changeset_falls_back_gracefully_without_panicking() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("only.txt", "hello\nworld\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(app.files().len(), 1);

        app.discard_file();
        assert!(app.pending_confirm.is_some());
        app.resolve_confirm(true); // runs the discard through run_op -> restore_position

        assert!(
            app.notice.is_none(),
            "discard must succeed: {:?}",
            app.notice
        );
        assert!(
            app.files().is_empty(),
            "the untracked file's only diff vanishes once discarded"
        );
        assert_eq!(
            app.cursor, 0,
            "the path check bails out; reset_panes' fallback stands"
        );
    }

    #[test]
    fn staging_with_the_cursor_on_a_gap_row_falls_back_without_panicking() {
        let fixture = three_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = {
            let view = app.role_view_ref(app.current, Role::Unstaged).unwrap();
            view.display
                .iter()
                .position(|r| matches!(r, DisplayRow::Gap { .. }))
                .expect("three well-separated hunks must collapse a gap between them")
        };
        app.cursor = gap_row;

        app.stage_file(); // whole-file op: ignores the cursor for WHAT it stages

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Single(Role::Staged),
            "no unstaged half survives a whole-file stage"
        );
        // The pre-op cursor sat on a Gap row, so the memento carried no lineno — restore is a
        // no-op and today's `reset_panes` first-hunk reseat stands.
        let view = app.role_view_ref(app.current, Role::Staged).unwrap();
        let expected = match app.layout {
            Layout::Sbs => view.first_hunk_row,
            Layout::Inline => view.first_inline_hunk_row,
        };
        assert_eq!(app.cursor, expected);
    }

    // ---- M4 staging: discard confirm flow --------------------------------------------------

    #[test]
    fn discard_hunk_requests_confirm_then_y_reverts_the_worktree() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.discard_hunk();

        // Requesting a confirm must NOT mutate anything yet.
        assert!(
            app.pending_confirm.is_some(),
            "discard must request a confirm"
        );
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals(
            "a.txt",
            "one\nCHANGED\n",
        ));

        app.resolve_confirm(true);
        assert!(app.pending_confirm.is_none(), "y must clear the confirm");
        // Discard reverts the worktree hunk back to the index (== HEAD here).
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("a.txt", "one\ntwo\n"));
    }

    #[test]
    fn discard_confirm_n_cancels_and_leaves_the_worktree_unchanged() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.discard_hunk();
        app.resolve_confirm(false);

        assert!(app.pending_confirm.is_none(), "n must clear the confirm");
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals(
            "a.txt",
            "one\nCHANGED\n",
        ));
    }

    #[test]
    fn discard_file_requests_confirm_then_y_reverts_the_whole_worktree_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\nthree\n", "ONE\ntwo\nTHREE\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.discard_file();
        assert!(app.pending_confirm.is_some());

        app.resolve_confirm(true);
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals(
            "a.txt",
            "one\ntwo\nthree\n",
        ));
    }

    // ---- M4 staging: refusals --------------------------------------------------------------

    #[test]
    fn stage_hunk_in_combined_view_refuses_without_touching_the_index() {
        use super::{Severity, Zoom};

        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.zoom = Zoom::Combined;
        app.open_current();
        app.stage_hunk();

        let notice = app.notice.as_ref().expect("combined stage must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(notice.text.contains("cycle zoom"), "got: {:?}", notice.text);
        // The index is untouched — still the originally-staged content.
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::index_blob_equals(
            "f.txt",
            "alpha\nBETAEDIT\ngamma\n",
        ));
    }

    #[test]
    fn discard_hunk_in_staged_pane_refuses() {
        use super::{Severity, Zoom};

        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.zoom = Zoom::Staged;
        app.open_current();
        app.discard_hunk();

        assert!(
            app.pending_confirm.is_none(),
            "a staged-pane discard must refuse, not request a confirm"
        );
        let notice = app.notice.as_ref().expect("staged discard must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("unstaged pane"),
            "got: {:?}",
            notice.text
        );
    }

    #[test]
    fn stage_hunk_between_hunks_refuses_with_no_hunk_under_cursor() {
        use super::Severity;

        let fixture = two_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        // Park the cursor on the gap row between the two hunks.
        let gap_row = app
            .current_view_ref()
            .unwrap()
            .display
            .iter()
            .position(|r| matches!(r, DisplayRow::Gap { .. }))
            .unwrap();
        app.cursor = gap_row;
        app.stage_hunk();

        let notice = app
            .notice
            .as_ref()
            .expect("between-hunks stage must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("no hunk under cursor"),
            "got: {:?}",
            notice.text
        );
        // Nothing was staged.
        let repo = fixture.repo().unwrap();
        assert!(
            !predicate::repo::has_staged_file("many.txt").eval(repo),
            "a refused stage must not touch the index"
        );
    }

    // ---- M4 line selection -----------------------------------------------------------------

    /// One hunk with two independent paired changes (line 2 `b`->`B`, line 4 `d`->`D`, one
    /// context line `c` between them). SBS display rows: 0 ctx, 1 del/add (b/B), 2 ctx, 3 del/add
    /// (d/D), 4 ctx — so `open_current` lands the cursor on row 1 (the first change).
    fn two_changes_one_hunk_fixture() -> Fixture {
        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", "a\nb\nc\nd\ne\n", "a\nB\nc\nD\ne\n")
            .build()
            .unwrap()
    }

    #[test]
    fn line_selection_for_hunk_resolves_linenos_to_hunk_line_indices() {
        use super::line_selection_for_hunk;
        use crate::model::{Hunk, HunkLine, LineKind};
        use std::collections::BTreeSet;

        let line = |kind, content: &str, old, new| HunkLine {
            kind,
            content: content.as_bytes().to_vec(),
            old_lnum: old,
            new_lnum: new,
            missing_newline: false,
        };
        let hunk = Hunk {
            old_start: 1,
            old_count: 5,
            new_start: 1,
            new_count: 5,
            header: b"@@ -1,5 +1,5 @@\n".to_vec(),
            lines: vec![
                line(LineKind::Context, "a\n", Some(1), Some(1)),
                line(LineKind::Deletion, "b\n", Some(2), None),
                line(LineKind::Addition, "B\n", None, Some(2)),
                line(LineKind::Deletion, "d\n", Some(4), None),
                line(LineKind::Addition, "D\n", None, Some(4)),
            ],
        };
        // Keep the b->B change (old-del line 2, new-add line 2), drop the line-4 change: the keep
        // sets are HUNK-LINE INDICES (1 = the `b` deletion, 2 = the `B` addition), not linenos.
        let sel = line_selection_for_hunk(&hunk, &BTreeSet::from([2]), &BTreeSet::from([2]));
        assert_eq!(sel.keep_dels, BTreeSet::from([1]));
        assert_eq!(sel.keep_adds, BTreeSet::from([2]));
    }

    #[test]
    fn sbs_selection_of_a_paired_row_keeps_both_its_del_and_add() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // cursor lands on row 1 (the b->B paired change)
        app.selection_anchor = Some(app.cursor); // single-row selection

        let ops = app.selection_line_ops();
        assert_eq!(ops.len(), 1, "one hunk overlapped");
        let (hunk_idx, sel) = &ops[0];
        assert_eq!(*hunk_idx, 0);
        // SBS row-pair semantics (locked decision #8): a paired row keeps BOTH sides.
        assert_eq!(sel.keep_dels.len(), 1, "SBS keeps the row's deleted line");
        assert_eq!(sel.keep_adds.len(), 1, "SBS keeps the row's added line too");
    }

    #[test]
    fn inline_selection_of_a_del_row_keeps_only_the_del() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.toggle_layout(); // -> inline (a paired change becomes a Del row then an Add row)

        let del_row = app
            .current_view_ref()
            .unwrap()
            .inline
            .iter()
            .position(|r| matches!(r, InlineRow::Del { old: 2, .. }))
            .expect("the b-deletion has its own inline Del row");
        app.cursor = del_row;
        app.selection_anchor = Some(del_row);

        let ops = app.selection_line_ops();
        assert_eq!(ops.len(), 1);
        let (_, sel) = &ops[0];
        // Inline keeps exactly the one side the selected row shows (locked decision #8).
        assert_eq!(
            sel.keep_dels.len(),
            1,
            "the selected Del contributes its del"
        );
        assert_eq!(sel.keep_adds.len(), 0, "and NOT its paired add");
    }

    #[test]
    fn multi_hunk_selection_splits_into_one_line_selection_per_hunk() {
        let fixture = two_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        // Anchor at the top, sweep the cursor to the last row: the range spans both hunks.
        app.cursor = 0;
        app.selection_anchor = Some(0);
        app.cursor = app.current_view_ref().unwrap().display.len() - 1;

        let ops = app.selection_line_ops();
        assert_eq!(ops.len(), 2, "two overlapped hunks -> two line selections");
        assert_eq!(ops[0].0, 0, "ascending hunk order");
        assert_eq!(ops[1].0, 1);
        assert!(
            ops.iter()
                .all(|(_, s)| !s.keep_dels.is_empty() || !s.keep_adds.is_empty()),
            "each entry carries at least one changed line"
        );
    }

    #[test]
    fn context_only_selection_yields_no_line_ops() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        // Row 0 is the leading context line `a`.
        app.cursor = 0;
        app.selection_anchor = Some(0);
        assert!(
            app.selection_line_ops().is_empty(),
            "a context-only selection maps to no line ops"
        );
    }

    #[test]
    fn stage_selection_stages_only_the_selected_lines_of_a_multi_change_hunk() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // cursor on the first change row (b->B)
        app.start_selection(); // anchor there -> single-row selection
        app.stage_hunk(); // routes to the selection stage

        assert!(
            app.notice.is_none(),
            "a clean line stage sets no error notice"
        );
        assert!(
            app.selection_anchor.is_none(),
            "the selection clears after applying"
        );
        let repo = fixture.repo().unwrap();
        // ONLY the b->B change landed in the index; d->D is still just in the worktree.
        repo.assert(predicate::repo::index_blob_equals(
            "f.txt",
            "a\nB\nc\nd\ne\n",
        ));
        repo.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            "a\nB\nc\nD\ne\n",
        ));
        repo.assert(predicate::repo::has_unstaged_file("f.txt"));
    }

    #[test]
    fn discard_selection_confirm_states_scope_then_y_reverts_only_selected_lines() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.start_selection(); // first change (b->B) only
        app.discard_hunk(); // routes to the selection discard

        let confirm = app
            .pending_confirm
            .as_ref()
            .expect("line discard requests a confirm");
        // True-scope message: 1 del + 1 add kept = 2 lines, in 1 hunk.
        assert!(
            confirm.prompt.contains("Discard 2 lines across 1 hunk"),
            "got: {:?}",
            confirm.prompt
        );
        // Nothing reverted until `y`.
        fixture.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            "a\nB\nc\nD\ne\n",
        ));

        app.resolve_confirm(true);
        assert!(
            app.selection_anchor.is_none(),
            "answering clears the selection"
        );
        // Only the selected b->B change reverted in the worktree; d->D stays.
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            "a\nb\nc\nD\ne\n",
        ));
    }

    #[test]
    fn discard_selection_confirm_n_leaves_the_worktree_unchanged() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.start_selection();
        app.discard_hunk();
        app.resolve_confirm(false);

        assert!(app.pending_confirm.is_none(), "n clears the confirm");
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            "a\nB\nc\nD\ne\n",
        ));
    }

    /// Two well-separated hunks in one file. The FIRST (earlier) hunk is a net +2 lines (a
    /// modification plus two insertions), so its change shifts every later line number in
    /// whichever text already contains it. A per-hunk patch is synthesized against the FULL
    /// unstaged (index ↔ worktree) diff, so hunk 2's header numbers already assume hunk 1's +2
    /// shift is present — draining hunk 1 and hunk 2 as two SEPARATE applies against the index
    /// (which starts with neither hunk's change) leaves hunk 2's patch inconsistent with
    /// whatever the index actually contains at that point, and libgit2 (strict, no fuzz) rejects
    /// it regardless of which hunk goes first. Merging both hunks into ONE patch (ascending
    /// order, exactly as they appear in the source diff) keeps the numbering internally
    /// consistent, so the whole selection applies in a single shot — see
    /// [`crate::ops::apply_line_selections`].
    fn two_hunks_net_shift_fixture() -> Fixture {
        let committed: String = (1..=20).map(|n| format!("L{n}\n")).collect();
        let mut worktree = String::from("L1\nL2X\nINS_A\nINS_B\n");
        for n in 3..=17 {
            worktree.push_str(&format!("L{n}\n"));
        }
        worktree.push_str("L18X\nL19\nL20\n");

        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", &committed, &worktree)
            .build()
            .unwrap()
    }

    #[test]
    fn line_stage_across_two_hunks_applies_both_despite_the_earlier_hunks_line_shift() {
        let fixture = two_hunks_net_shift_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        // Select the whole file (both hunks and the gap between them), then stage the selection.
        app.cursor = 0;
        app.start_selection();
        app.scroll_bottom(); // cursor -> last display row
        app.stage_hunk(); // active selection -> stage_selection over both hunks

        assert!(
            app.notice.is_none(),
            "both hunks must stage cleanly; got notice: {:?}",
            app.notice
        );
        assert!(
            app.selection_anchor.is_none(),
            "selection clears after apply"
        );
        // Both changes landed in the index in ONE apply: it now equals the worktree in full.
        let repo = fixture.repo().unwrap();
        let worktree = std::fs::read_to_string(repo.workdir().unwrap().join("f.txt")).unwrap();
        repo.assert(predicate::repo::index_blob_equals(
            "f.txt",
            worktree.as_str(),
        ));
    }

    #[test]
    fn line_discard_across_two_hunks_reverts_both_despite_the_earlier_hunks_line_shift() {
        // Mirrors the stage tripwire above for the DISCARD verb (worktree -> index, reversed): a
        // multi-hunk line discard must also merge into one patch rather than one apply per hunk.
        let fixture = two_hunks_net_shift_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        app.cursor = 0;
        app.start_selection();
        app.scroll_bottom();
        app.discard_hunk(); // active selection -> discard_selection over both hunks

        let confirm = app
            .pending_confirm
            .as_ref()
            .expect("multi-hunk line discard requests a confirm");
        assert!(
            confirm.prompt.contains("hunks"),
            "expected the true-scope prompt to name multiple hunks, got: {:?}",
            confirm.prompt
        );

        app.resolve_confirm(true);
        assert!(
            app.notice.is_none(),
            "both hunks must discard cleanly in one apply; got notice: {:?}",
            app.notice
        );
        assert!(
            app.selection_anchor.is_none(),
            "selection clears after resolving the confirm"
        );

        // Both changes reverted in the worktree in ONE apply: it now equals HEAD in full.
        let repo = fixture.repo().unwrap();
        let head: String = (1..=20).map(|n| format!("L{n}\n")).collect();
        repo.assert(predicate::repo::workdir_file_equals("f.txt", head.as_str()));
    }

    #[test]
    fn line_stage_on_untracked_file_refuses_with_modified_file_message() {
        use super::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("new.txt", "x\ny\nz\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.start_selection(); // untracked file has an unstaged change, so selection is allowed
        app.stage_hunk();

        let notice = app.notice.as_ref().expect("line staging must refuse here");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("line staging needs a modified file"),
            "got: {:?}",
            notice.text
        );
        let repo = fixture.repo().unwrap();
        assert!(
            !predicate::repo::has_staged_file("new.txt").eval(repo),
            "a refused line stage must not touch the index"
        );
    }

    #[test]
    fn line_stage_of_context_only_selection_refuses() {
        use super::Severity;

        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.cursor = 0; // leading context row
        app.start_selection();
        app.stage_hunk();

        let notice = app.notice.as_ref().expect("context-only stage must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("no changed lines in selection"),
            "got: {:?}",
            notice.text
        );
        let repo = fixture.repo().unwrap();
        assert!(!predicate::repo::has_staged_file("f.txt").eval(repo));
    }

    #[test]
    fn start_selection_in_combined_view_refuses() {
        use super::{Severity, Zoom};

        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.zoom = Zoom::Combined;
        app.open_current();
        app.start_selection();

        assert!(
            app.selection_anchor.is_none(),
            "the combined view has no staging direction, so selection is refused"
        );
        let notice = app.notice.as_ref().expect("combined selection must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(notice.text.contains("cycle zoom"), "got: {:?}", notice.text);
    }

    #[test]
    fn cancel_and_layout_toggle_both_clear_an_active_selection() {
        let fixture = two_changes_one_hunk_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        app.start_selection();
        assert!(app.selection_anchor.is_some());
        app.cancel_selection();
        assert!(
            app.selection_anchor.is_none(),
            "cancel clears the selection"
        );

        // A layout toggle reshapes the coordinate space, so it also cancels.
        app.start_selection();
        assert!(app.selection_anchor.is_some());
        app.toggle_layout();
        assert!(
            app.selection_anchor.is_none(),
            "toggling layout cancels the selection"
        );
    }

    // ── M5 CS1: the changeset-stack spine ─────────────────────────────────────

    #[test]
    fn single_uncommitted_changeset_matches_m4_shape() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let app = app_from_fixture(&fixture);
        assert_eq!(
            app.changeset_count(),
            1,
            "a non-Graphite repo degrades to a single synthetic changeset"
        );
        assert_eq!(app.current_cs(), 0);
        assert_eq!(app.base_label, "HEAD");
        assert!(matches!(
            app.current_changeset().span,
            ChangesetSpan::Uncommitted
        ));
    }

    #[test]
    fn committed_changeset_view_has_empty_sub_diffs_and_renders_read_only() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("a.txt", "one\n")
            .create("first")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("a.txt", "one\nCHANGED\n")
            .create("second")
            .unwrap();

        let repo = fixture.repo().unwrap();
        let cs = Changeset {
            name: "main".to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let diff = crate::acquire::diff_changeset(repo, &cs).expect("diff_changeset");
        let view = ChangesetView::from_changeset_diff(cs, diff);
        assert_eq!(view.file_count(), 1);

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();

        assert_eq!(app.files().len(), 1);
        assert_eq!(
            app.effective_zoom_for(0),
            EffectiveZoom::Single(Role::Combined),
            "empty staged/unstaged sub-models collapse every zoom to combined-only, for free"
        );

        // Read-only follows from the natural collapse above; the refusal MESSAGE is
        // committed-mode-aware (m5-changeset-nav locked decision #2) — a plain "already
        // committed" notice, not the uncommitted "cycle zoom" hint (there's no zoom that would
        // help here).
        app.stage_hunk();
        let notice = app
            .notice
            .as_ref()
            .expect("staging must refuse on a combined-only (committed) changeset");
        assert!(
            notice.text.contains("already committed"),
            "got: {:?}",
            notice.text
        );
    }

    #[test]
    fn base_label_is_committed_changeset_base_short_sha() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("a.txt", "one\n")
            .create("first")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("a.txt", "two\n")
            .create("second")
            .unwrap();

        let repo = fixture.repo().unwrap();
        let cs = Changeset {
            name: "main".to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let diff = crate::acquire::diff_changeset(repo, &cs).expect("diff_changeset");
        let view = ChangesetView::from_changeset_diff(cs, diff);

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let app = App::from_changesets(owned, vec![view]);

        assert_eq!(app.base_label, base.to_string()[..7].to_string());
    }

    #[test]
    fn current_cs_honors_lib_current_flag() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("a.txt", "one\n")
            .create("first")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("a.txt", "two\n")
            .create("second")
            .unwrap();
        let repo = fixture.repo().unwrap();

        // Deliberately NOT current — listed first, so a naive "open index 0" would pick it.
        let not_current = Changeset {
            name: "not-current".to_string(),
            span: ChangesetSpan::Committed { base, head: base },
            title: None,
            current: false,
            needs_restack: false,
        };
        let current = Changeset {
            name: "current".to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };

        let not_current_view = ChangesetView::from_changeset_diff(
            not_current.clone(),
            crate::acquire::diff_changeset(repo, &not_current).unwrap(),
        );
        let current_view = ChangesetView::from_changeset_diff(
            current.clone(),
            crate::acquire::diff_changeset(repo, &current).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let app = App::from_changesets(owned, vec![not_current_view, current_view]);

        assert_eq!(
            app.current_cs(),
            1,
            "must open on the lib-marked current entry"
        );
        assert_eq!(app.current_changeset().name, "current");
    }

    // ── M5 CS2: continuous nav, changeset nav, committed-mode guards ─────────

    /// A two-committed-changeset stack for CS2's nav tests, hand-built the same way as the M5 CS1
    /// tests above: `cs-a` (`root..mid`, TWO files — `a1.txt`/`a2.txt`) then `cs-b` (`mid..head`,
    /// ONE file — `b1.txt`), opening on `cs-a`'s first file. The two-file first changeset lets a
    /// test distinguish "advance within a changeset" from "cross into the next changeset" at its
    /// boundary, rather than every `next_file` immediately crossing.
    fn two_committed_changesets_two_and_one_files() -> App {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mid = fixture
            .commit("main")
            .file("a1.txt", "a1\n")
            .file("a2.txt", "a2\n")
            .create("mid")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("b1.txt", "b1\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: true,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: false,
            needs_restack: false,
        };
        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::from_changeset_diff(
            cs_b.clone(),
            crate::acquire::diff_changeset(repo, &cs_b).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.open_current();
        assert_eq!(app.current_cs(), 0, "opens on cs-a (its current: true)");
        assert_eq!(app.files().len(), 2, "cs-a has two files");
        app
    }

    #[test]
    fn next_file_advances_within_a_changeset_before_crossing_into_the_next() {
        let mut app = two_committed_changesets_two_and_one_files();

        app.next_file();
        assert_eq!(app.current_cs(), 0, "still inside cs-a");
        assert_eq!(app.current, 1);
        assert_eq!(app.files()[app.current].path, "a2.txt");

        app.next_file();
        assert_eq!(
            app.current_cs(),
            1,
            "advancing past cs-a's last file crosses into cs-b"
        );
        assert_eq!(app.current, 0, "lands on cs-b's FIRST file");
        assert_eq!(app.files()[0].path, "b1.txt");
    }

    #[test]
    fn next_file_clamps_at_the_last_file_of_the_last_changeset() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.goto_changeset(1);
        assert_eq!(app.current_cs(), 1);
        assert_eq!(app.current, 0);

        app.next_file();
        assert_eq!(
            app.current_cs(),
            1,
            "the stack's very last file must clamp, not wrap to changeset 0"
        );
        assert_eq!(app.current, 0);
    }

    #[test]
    fn prev_file_crosses_backward_into_the_previous_changesets_last_file() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.goto_changeset(1);
        assert_eq!(app.current_cs(), 1);
        assert_eq!(app.current, 0);

        app.prev_file();
        assert_eq!(
            app.current_cs(),
            0,
            "retreating past cs-b's first file crosses back into cs-a"
        );
        assert_eq!(app.current, 1, "lands on cs-a's LAST file, not its first");
        assert_eq!(app.files()[1].path, "a2.txt");
    }

    #[test]
    fn prev_file_clamps_at_the_first_file_of_the_first_changeset() {
        let mut app = two_committed_changesets_two_and_one_files();
        assert_eq!(app.current_cs(), 0);
        assert_eq!(app.current, 0);

        app.prev_file();
        assert_eq!(
            app.current_cs(),
            0,
            "the stack's very first file must clamp, not wrap to the last changeset"
        );
        assert_eq!(app.current, 0);
    }

    /// Regression: navigating to an OLDER committed changeset and loading its combined view must
    /// source the new side from that changeset's `head` commit tree, not the current worktree. The
    /// same file `f.txt` is touched by both changesets, so `cs-a`'s head (`mid`) content differs
    /// from the worktree (which holds `head`'s content). Before the `new_side_tree_for` fix the new
    /// side read the worktree, whose line count disagreed with `cs-a`'s `base..head` hunks and
    /// tripped the align invariant (align.rs:165 "trailing context ... must be equal length"). No
    /// color pinning needed: `new_text()` returns the raw blob text, not highlighted spans.
    #[test]
    fn older_committed_changesets_new_side_reads_its_head_tree_not_the_worktree() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("f.txt", "one\n")
            .create("root")
            .unwrap();
        // cs-a (root..mid) adds "two" to f.txt — its head-tree copy is "one\ntwo\n".
        let mid = fixture
            .commit("main")
            .file("f.txt", "one\ntwo\n")
            .create("mid")
            .unwrap();
        // cs-b (mid..head) adds "three" — so the checked-out worktree copy is "one\ntwo\nthree\n",
        // three lines, which must NOT be what cs-a's combined new side reads.
        let head = fixture
            .commit("main")
            .file("f.txt", "one\ntwo\nthree\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::from_changeset_diff(
            cs_b.clone(),
            crate::acquire::diff_changeset(repo, &cs_b).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.open_current();
        assert_eq!(app.current_cs(), 1, "opens on cs-b (its current: true)");

        // Navigate back to the older changeset and load its combined view. Pre-fix this panics at
        // align.rs:165; post-fix it loads cleanly.
        app.prev_changeset();
        assert_eq!(app.current_cs(), 0, "prev lands on cs-a");
        let view = app.current_view().expect("cs-a's combined view must load");

        assert_eq!(
            view.new_text(),
            "one\ntwo\n",
            "new side must read cs-a's head (mid) blob, not the worktree copy"
        );
        assert_ne!(
            view.new_text(),
            "one\ntwo\nthree\n",
            "new side must NOT read the worktree (which holds cs-b's head content)"
        );
    }

    #[test]
    fn bracket_c_jumps_to_the_adjacent_changesets_first_file() {
        let mut app = two_committed_changesets_two_and_one_files();
        // Start mid-file so the jump is visibly to file 0, not just "whatever was current".
        app.current = 1;

        app.next_changeset();
        assert_eq!(app.current_cs(), 1);
        assert_eq!(app.current, 0, "]c always lands on the FIRST file");
        assert_eq!(app.files()[0].path, "b1.txt");

        app.next_changeset();
        assert_eq!(app.current_cs(), 1, "]c clamps at the last changeset");

        app.prev_changeset();
        assert_eq!(app.current_cs(), 0);
        assert_eq!(app.current, 0);

        app.prev_changeset();
        assert_eq!(app.current_cs(), 0, "[c clamps at the first changeset");
    }

    #[test]
    fn switching_changeset_updates_base_label() {
        let mut app = two_committed_changesets_two_and_one_files();
        let cs_a_label = app.base_label.clone();

        app.goto_changeset(1);
        assert_ne!(
            app.base_label, cs_a_label,
            "cs-a and cs-b have different base revisions"
        );
    }

    #[test]
    fn is_committed_true_for_a_committed_changeset_false_for_uncommitted() {
        let committed = two_committed_changesets_two_and_one_files();
        assert!(committed.is_committed());

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let uncommitted = app_from_fixture(&fixture);
        assert!(!uncommitted.is_committed());
    }

    #[test]
    fn cycle_zoom_is_a_no_op_on_a_committed_changeset() {
        let mut app = two_committed_changesets_two_and_one_files();
        let zoom_before = app.zoom;

        app.cycle_zoom();

        assert_eq!(
            app.zoom, zoom_before,
            "z must not change the requested zoom on a committed changeset"
        );
        assert!(
            app.notice.is_some(),
            "z should still surface a notice explaining why it's a no-op"
        );
    }

    #[test]
    fn staging_verbs_refuse_with_a_committed_specific_message() {
        let mut app = two_committed_changesets_two_and_one_files();

        app.stage_file();
        let notice = app
            .notice
            .as_ref()
            .expect("stage_file must refuse on a committed changeset");
        assert!(
            notice.text.contains("already committed"),
            "got: {:?}",
            notice.text
        );

        app.clear_notice();
        app.discard_file();
        let notice = app
            .notice
            .as_ref()
            .expect("discard_file must refuse on a committed changeset");
        assert!(
            notice.text.contains("already committed"),
            "got: {:?}",
            notice.text
        );

        app.clear_notice();
        app.start_selection();
        assert!(app.selection_anchor.is_none());
        let notice = app
            .notice
            .as_ref()
            .expect("start_selection must refuse on a committed changeset");
        assert!(
            notice.text.contains("already committed"),
            "got: {:?}",
            notice.text
        );
    }

    // ── M5 CS3: outline side pane ───────────────────────────────────────────────

    /// A committed changeset (`base..head`, one file, not current) beneath an uncommitted
    /// changeset (one untracked file, current) — the mix the outline's "status column only for
    /// the uncommitted changeset" test needs, hand-built the same way as every other M5 test in
    /// this module (`Changeset` literal + `diff_changeset` + `ChangesetView::from_changeset_diff`
    /// for BOTH sources — the acquisition router handles either).
    fn committed_and_uncommitted_stack() -> App {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("base.txt", "b\n")
            .create("base")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("c1.txt", "c1\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();
        std::fs::write(repo.workdir().unwrap().join("u1.txt"), "u1\n").unwrap();

        let committed = Changeset {
            name: "committed".to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: Some("Committed work".to_string()),
            current: false,
            needs_restack: false,
        };
        let uncommitted = Changeset {
            name: "uncommitted".to_string(),
            span: ChangesetSpan::Uncommitted,
            title: None,
            current: true,
            needs_restack: false,
        };
        let view_c = ChangesetView::from_changeset_diff(
            committed.clone(),
            crate::acquire::diff_changeset(repo, &committed).unwrap(),
        );
        let view_u = ChangesetView::from_changeset_diff(
            uncommitted.clone(),
            crate::acquire::diff_changeset(repo, &uncommitted).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_c, view_u]);
        app.open_current();
        assert_eq!(
            app.current_cs(),
            1,
            "opens on the uncommitted layer (its current: true)"
        );
        app
    }

    #[test]
    fn outline_default_open_for_a_multi_changeset_stack_closed_for_a_lone_changeset() {
        let multi = two_committed_changesets_two_and_one_files();
        assert!(
            multi.outline_open(),
            "a stack of more than one changeset must default-open the outline"
        );
        assert!(
            !multi.outline_focused(),
            "the diff keeps initial focus even though the outline defaults open"
        );

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let lone = app_from_fixture(&fixture);
        assert!(
            !lone.outline_open(),
            "a lone uncommitted changeset must keep the M4 full-width look (outline closed)"
        );
    }

    #[test]
    fn toggle_outline_is_a_pure_show_hide_toggle() {
        let mut app = two_committed_changesets_two_and_one_files();
        // Force a known starting state regardless of the default.
        while app.outline_open() {
            app.toggle_outline();
        }
        assert!(!app.outline_open());

        app.toggle_outline();
        assert!(
            app.outline_open() && app.outline_focused(),
            "o from closed opens AND focuses"
        );

        app.toggle_outline();
        assert!(
            !app.outline_open() && !app.outline_focused(),
            "o from open+focused closes — the toggle only ever tracks visibility"
        );

        // Re-open, then unfocus without going through `toggle_outline` (mirrors the startup
        // seed: open, but diff-focused) — `o` from THAT state must still close, not cycle
        // through a middle focused-then-unfocused state.
        app.toggle_outline();
        app.focus_diff();
        assert!(app.outline_open() && !app.outline_focused());

        app.toggle_outline();
        assert!(
            !app.outline_open() && !app.outline_focused(),
            "o from open+unfocused closes the pane"
        );
    }

    #[test]
    fn focus_outline_opens_when_closed_and_syncs_the_cursor() {
        let mut app = two_committed_changesets_two_and_one_files();
        while app.outline_open() {
            app.toggle_outline();
        }
        assert!(!app.outline_open());
        // Move the diff onto the second changeset before focusing, so a sync is observable.
        app.next_changeset();
        let current_cs = app.current_cs();

        app.focus_outline();

        assert!(app.outline_open() && app.outline_focused());
        let items = app.outline_items();
        assert!(
            matches!(
                items[app.outline_cursor()],
                crate::outline::OutlineItem::File { cs_idx, .. } if cs_idx == current_cs
            ),
            "opening via focus_outline syncs the cursor to the current diff position"
        );
    }

    #[test]
    fn focus_outline_on_an_already_open_outline_does_not_move_the_cursor() {
        let mut app = two_committed_changesets_two_and_one_files();
        while app.outline_open() {
            app.toggle_outline();
        }
        app.toggle_outline(); // open + focus, synced
        app.outline_move_by(-1); // manually reposition the outline cursor
        app.focus_diff();
        let cursor_before = app.outline_cursor();

        app.focus_outline();

        assert!(app.outline_focused());
        assert_eq!(
            app.outline_cursor(),
            cursor_before,
            "re-focusing an already-open outline must not stomp a manually positioned cursor"
        );
    }

    #[test]
    fn focus_diff_unfocuses_without_closing_the_outline() {
        let mut app = two_committed_changesets_two_and_one_files();
        while app.outline_open() {
            app.toggle_outline();
        }
        app.toggle_outline(); // open + focus
        assert!(app.outline_open() && app.outline_focused());

        app.focus_diff();

        assert!(
            app.outline_open() && !app.outline_focused(),
            "focus_diff unfocuses but leaves the outline open"
        );
    }

    #[test]
    fn toggle_help_flips_help_visible() {
        let mut app = two_committed_changesets_two_and_one_files();
        assert!(!app.help_visible, "help is closed by default");

        app.toggle_help();
        assert!(app.help_visible, "toggle_help opens it");

        app.toggle_help();
        assert!(!app.help_visible, "toggle_help closes it again");
    }

    #[test]
    fn outline_cycle_mode_round_trips_all_four_modes() {
        let mut app = two_committed_changesets_two_and_one_files();
        let start = app.outline_mode();

        let mut seen = vec![start];
        for _ in 0..3 {
            app.outline_cycle_mode();
            assert!(
                !seen.contains(&app.outline_mode()),
                "each of the first 4 cycles must be a mode not yet seen"
            );
            seen.push(app.outline_mode());
        }

        app.outline_cycle_mode();
        assert_eq!(
            app.outline_mode(),
            start,
            "the 4th cycle returns to the start"
        );
    }

    #[test]
    fn stack_mode_outline_items_carry_current_and_restack_markers() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("r.txt", "r\n")
            .create("root")
            .unwrap();
        let mid = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .create("mid")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("b.txt", "b\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: true,
            needs_restack: true,
        };
        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::from_changeset_diff(
            cs_b.clone(),
            crate::acquire::diff_changeset(repo, &cs_b).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — this test asserts per-header marker content, not
        // display order, so it doesn't need to track the new HeadFirst default.
        app.outline.order = OutlineOrder::BaseFirst;

        let items = app.outline_items();
        assert_eq!(
            items[0],
            OutlineItem::Header {
                cs_idx: 0,
                label: "cs-a".to_string(),
                current: false,
                needs_restack: false,
                loading: false,
                failed: false,
            }
        );
        let header_b = items
            .iter()
            .find(|it| matches!(it, OutlineItem::Header { cs_idx: 1, .. }))
            .expect("cs-b header present");
        assert_eq!(
            header_b,
            &OutlineItem::Header {
                cs_idx: 1,
                label: "cs-b".to_string(),
                current: true,
                needs_restack: true,
                loading: false,
                failed: false,
            }
        );
    }

    // ── ADR-037: per-changeset slots (Pending/Ready/Failed) ─────────────────────

    /// A minimal [`Changeset`] descriptor for the slot tests below — the slot model only cares
    /// about the metadata `ChangesetView::pending`/`failed` carry alongside a diff-free
    /// [`DiffState`], not any real git content.
    fn bare_changeset(name: &str, current: bool) -> Changeset {
        Changeset {
            name: name.to_string(),
            span: ChangesetSpan::Uncommitted,
            title: None,
            current,
            needs_restack: false,
        }
    }

    #[test]
    fn app_is_constructible_from_a_pending_changeset_alone() {
        // ADR-037: `App::from_changesets`'s >=1 assert survives unchanged — an all-Pending stack
        // (the streamed-launch shape, before any diff has landed) is a valid `App`.
        let view = ChangesetView::pending(bare_changeset("cs-a", true));
        let fixture = FixtureBuilder::new().build().unwrap();
        let repo = Repository::open(fixture.repo().unwrap().workdir().unwrap()).unwrap();
        let app = App::from_changesets(repo, vec![view]);

        assert!(app.is_current_pending());
        assert_eq!(app.current_failure(), None);
        assert!(app.files().is_empty());
        assert_eq!(app.changeset_count(), 1);
    }

    #[test]
    fn navigating_onto_a_pending_changeset_shows_no_files_and_stays_pending() {
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let cs_ready = bare_changeset("cs-ready", true);
        let diffs = crate::acquire::diff_uncommitted(repo).unwrap();
        let view_ready = ChangesetView::new(cs_ready, DiffState::from(diffs));
        let view_pending = ChangesetView::pending(bare_changeset("cs-pending", false));

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_ready, view_pending]);

        assert!(!app.is_current_pending(), "opens on the Ready changeset");
        assert!(!app.files().is_empty());

        app.next_changeset();

        assert_eq!(app.current_cs(), 1);
        assert!(app.is_current_pending());
        assert!(
            app.files().is_empty(),
            "a Pending changeset has no file rows to navigate onto"
        );
    }

    #[test]
    fn failed_slot_carries_its_error_message() {
        let view = ChangesetView::failed(bare_changeset("cs-a", true), "diff acquisition failed");
        let fixture = FixtureBuilder::new().build().unwrap();
        let repo = Repository::open(fixture.repo().unwrap().workdir().unwrap()).unwrap();
        let app = App::from_changesets(repo, vec![view]);

        assert!(!app.is_current_pending());
        assert_eq!(app.current_failure(), Some("diff acquisition failed"));
        assert!(app.files().is_empty());
    }

    #[test]
    fn outline_marks_pending_and_failed_changeset_headers() {
        let view_pending = ChangesetView::pending(bare_changeset("cs-pending", true));
        let view_failed = ChangesetView::failed(bare_changeset("cs-failed", false), "boom");
        let fixture = FixtureBuilder::new().build().unwrap();
        let repo = Repository::open(fixture.repo().unwrap().workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(repo, vec![view_pending, view_failed]);
        app.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — this test asserts the exact header vec, which is
        // incidental to base -> head storage order here, not what's under test (the
        // loading/failed markers).
        app.outline.order = OutlineOrder::BaseFirst;

        let items = app.outline_items();
        assert_eq!(
            items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    label: "cs-pending".to_string(),
                    current: true,
                    needs_restack: false,
                    loading: true,
                    failed: false,
                },
                OutlineItem::Header {
                    cs_idx: 1,
                    label: "cs-failed".to_string(),
                    current: false,
                    needs_restack: false,
                    loading: false,
                    failed: true,
                },
            ],
            "Pending/Failed changesets emit only their (marked) header, no file rows"
        );
    }

    // ── ADR-037: the streamed-launch wave's chokepoint ───────────────────────────

    #[test]
    fn apply_changeset_ready_seats_the_active_changeset_when_its_diff_lands() {
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let view_a = ChangesetView::pending(bare_changeset("cs-a", true));
        let view_b = ChangesetView::pending(bare_changeset("cs-b", false));
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        assert!(app.is_current_pending());

        let diffs = crate::acquire::diff_uncommitted(repo).unwrap();
        app.apply_changeset_ready(
            app.generation(),
            0,
            Ok(crate::acquire::ChangesetDiff::Uncommitted(diffs)),
        );

        assert!(
            !app.is_current_pending(),
            "the readied ACTIVE changeset must be seated, not left Pending"
        );
        assert!(!app.files().is_empty());
        assert!(
            app.current_view_ref().is_some(),
            "seating an active changeset opens its first file exactly like a fresh open would"
        );
    }

    #[test]
    fn apply_changeset_ready_marks_a_non_active_changeset_ready_without_disturbing_current() {
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let view_a = ChangesetView::pending(bare_changeset("cs-a", true));
        let view_b = ChangesetView::pending(bare_changeset("cs-b", false));
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);

        let diffs = crate::acquire::diff_uncommitted(repo).unwrap();
        app.apply_changeset_ready(
            app.generation(),
            1,
            Ok(crate::acquire::ChangesetDiff::Uncommitted(diffs)),
        );

        assert_eq!(app.current_cs(), 0, "the active changeset must not move");
        assert!(
            app.is_current_pending(),
            "cs-a is still Pending — only cs-b's slot changed"
        );
        app.next_changeset();
        assert!(
            !app.is_current_pending(),
            "cs-b's slot is now Ready after navigating onto it"
        );
    }

    #[test]
    fn apply_changeset_ready_drops_a_stale_generation_result() {
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let view = ChangesetView::pending(bare_changeset("cs-a", true));
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        let stale_gen = app.generation();
        app.generation += 1; // simulate a refresh landing between dispatch and result

        let diffs = crate::acquire::diff_uncommitted(repo).unwrap();
        app.apply_changeset_ready(
            stale_gen,
            0,
            Ok(crate::acquire::ChangesetDiff::Uncommitted(diffs)),
        );

        assert!(
            app.is_current_pending(),
            "a stale-generation result must not seat a changeset from a world that no longer exists"
        );
    }

    #[test]
    fn apply_changeset_ready_err_marks_failed_and_notifies_only_on_the_first_failure() {
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let view_a = ChangesetView::pending(bare_changeset("cs-a", true));
        let view_b = ChangesetView::pending(bare_changeset("cs-b", false));
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        assert!(app.notice.is_none());

        app.apply_changeset_ready(app.generation(), 0, Err("first failure".to_string()));
        assert!(
            app.current_failure().is_some(),
            "the active changeset's Failed slot carries the message"
        );
        let notice = app
            .notice
            .as_ref()
            .expect("the wave's first failure raises a footer notice");
        assert_eq!(notice.severity, Severity::Error);
        assert!(notice.text.contains("first failure"));

        // A SECOND failure in the same wave must not raise a second notice — only the wave's
        // FIRST failure does (see `App::wave_failure_notified`'s doc comment). The review
        // continues: cs-b's slot still becomes Failed even though no new notice fires.
        app.apply_changeset_ready(app.generation(), 1, Err("second failure".to_string()));
        let notice_after = app.notice.as_ref().unwrap();
        assert!(
            notice_after.text.contains("first failure"),
            "a second failure in the same wave must not overwrite the first's notice"
        );
    }

    #[test]
    fn apply_changeset_ready_keeps_outline_cursor_anchored_when_an_earlier_non_active_changeset_lands(
    ) {
        // F3 regression: cs-a sits BEFORE the active cs-b in the outline row list. Landing cs-a's
        // diff inserts its file rows ahead of cs-b's header, shifting every row-index cursor at
        // or after cs-a's header — a plain row-index cursor would silently drift onto one of
        // cs-a's new file rows instead of staying on cs-b's header.
        let fixture = two_changes_one_hunk_fixture();
        let repo = fixture.repo().unwrap();
        let view_a = ChangesetView::pending(bare_changeset("cs-a", false));
        let view_b = ChangesetView::pending(bare_changeset("cs-b", true));
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — the regression this test guards needs cs-a BEFORE
        // cs-b in the row list (an earlier row's insertion shifting a later row's index); the
        // new HeadFirst default would put cs-b (head) first instead, inverting the scenario.
        app.outline.order = OutlineOrder::BaseFirst;
        assert_eq!(
            app.current_cs(),
            1,
            "cs-b is the lib-marked current changeset"
        );

        let items_before = app.outline_items();
        let cursor_before = items_before
            .iter()
            .position(|it| matches!(it, OutlineItem::Header { cs_idx: 1, .. }))
            .expect("cs-b's header row exists before cs-a lands");
        app.outline.cursor = cursor_before;

        let diffs = crate::acquire::diff_uncommitted(repo).unwrap();
        app.apply_changeset_ready(
            app.generation(),
            0,
            Ok(crate::acquire::ChangesetDiff::Uncommitted(diffs)),
        );

        let items_after = app.outline_items();
        assert!(
            items_after.len() > items_before.len(),
            "cs-a's file rows must have been inserted ahead of cs-b's header"
        );
        assert_eq!(
            items_after[app.outline_cursor()],
            OutlineItem::Header {
                cs_idx: 1,
                label: "cs-b".to_string(),
                current: true,
                needs_restack: false,
                loading: true,
                failed: false,
            },
            "the outline cursor must still identify cs-b's header row, not whatever row now \
             sits at its old index"
        );
    }

    #[test]
    fn staged_status_column_only_populated_for_the_uncommitted_changesets_files() {
        let mut app = committed_and_uncommitted_stack();
        app.outline.mode = OutlineMode::Stack;
        let items = app.outline_items();

        let committed_file = items
            .iter()
            .find(|it| matches!(it, OutlineItem::File { path, .. } if path == "c1.txt"))
            .expect("committed changeset's file row present");
        assert_eq!(
            committed_file,
            &OutlineItem::File {
                cs_idx: 0,
                file_idx: 0,
                path: "c1.txt".to_string(),
                status: StagedStatus::None,
                change: FileStatus::Added,
                guides: Vec::new(),
            },
            "a committed changeset's file must carry no staged-ness status"
        );

        let uncommitted_file = items
            .iter()
            .find(|it| matches!(it, OutlineItem::File { path, .. } if path == "u1.txt"))
            .expect("uncommitted changeset's file row present");
        assert!(
            !matches!(uncommitted_file, OutlineItem::File { status: StagedStatus::None, .. }),
            "the untracked uncommitted file must carry a real staged-ness status, got: {uncommitted_file:?}"
        );
    }

    /// CS5: `outline_snapshot`'s `change` field is lifted from the owning `FileChange::status`,
    /// a wholly separate axis from `status` (staged-ness — see `outline::OutlineFile::change`'s
    /// doc comment). `c1.txt` is a new file introduced by the committed changeset's head commit
    /// (`Added`); `u1.txt` is an untracked worktree file (`Untracked`) — distinct FileStatus
    /// values, confirming this isn't just always defaulting to one variant.
    #[test]
    fn outline_snapshot_lifts_change_status_from_the_file_model_independent_of_staged_status() {
        let mut app = committed_and_uncommitted_stack();
        app.outline.mode = OutlineMode::Stack;
        let items = app.outline_items();

        let change_for = |path: &str| {
            items
                .iter()
                .find_map(|it| match it {
                    OutlineItem::File {
                        path: p, change, ..
                    } if p == path => Some(*change),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{path}'s file row present"))
        };
        assert_eq!(change_for("c1.txt"), FileStatus::Added);
        assert_eq!(change_for("u1.txt"), FileStatus::Untracked);
    }

    #[test]
    fn outline_move_by_on_a_file_row_jumps_the_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Flat;
        // CS3: pin BaseFirst explicitly — this test exercises `outline_move_by`'s row-crossing
        // mechanics via hardcoded Flat-mode indices, not display order.
        app.outline.order = OutlineOrder::BaseFirst;
        app.outline.cursor = 0;
        assert_eq!(app.current_cs(), 0);
        assert_eq!(app.current, 0);

        // Flat mode: a1.txt, a2.txt, b1.txt — moving to index 2 must land the diff on b1.txt in
        // cs-b.
        app.outline_move_by(2);
        assert_eq!(
            app.current_cs(),
            1,
            "the outline jump must switch changeset"
        );
        assert_eq!(app.files()[app.current].path, "b1.txt");
    }

    #[test]
    fn outline_move_by_on_a_header_row_does_not_jump_the_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — this test's hardcoded row indices assume base -> head
        // order (header, a1, a2, header, b1); the new HeadFirst default is a display-order
        // concern orthogonal to what's under test here (whether a header move jumps the diff).
        app.outline.order = OutlineOrder::BaseFirst;
        // Header rows sit at indices 0 (cs-a) and 3 (cs-b) in Stack mode (header, a1, a2,
        // header, b1). Park the diff on a2, cursor on its row.
        app.outline.cursor = 2;
        app.switch_changeset(0, 1);
        app.cursor += 1; // nudge off the open position so a hidden re-open would be visible
        let cursor_before = app.cursor;

        // A UNIT move onto the header: the header itself never jumps — and must not reset the
        // diff's cursor either (a re-`switch_changeset` to the same file would).
        app.outline_move_by(1);
        assert_eq!(
            (app.current_cs(), app.current),
            (0, 1),
            "a bare j onto a header row must not move the diff"
        );
        assert_eq!(app.cursor, cursor_before, "...nor reset the diff cursor");
    }

    #[test]
    fn coalesced_outline_burst_onto_a_header_matches_sequential_unit_moves() {
        // A multi-row delta is CS2's coalesced stand-in for N unit presses, so the two must be
        // indistinguishable — including which file the diff follows when the burst stops on a
        // header row (the LAST file crossed, exactly where unit presses leave it).
        let mut coalesced = two_committed_changesets_two_and_one_files();
        coalesced.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — the burst-vs-sequential equivalence under test doesn't
        // depend on which end of the stack displays first, and the inline comments below assume
        // base -> head row order.
        coalesced.outline.order = OutlineOrder::BaseFirst;
        coalesced.outline.cursor = 0;
        coalesced.outline_move_by(3); // header -> a1 -> a2 -> cs-b header

        let mut sequential = two_committed_changesets_two_and_one_files();
        sequential.outline.mode = OutlineMode::Stack;
        sequential.outline.order = OutlineOrder::BaseFirst;
        sequential.outline.cursor = 0;
        for _ in 0..3 {
            sequential.outline_move_by(1);
        }

        assert_eq!(coalesced.outline.cursor, sequential.outline.cursor);
        assert_eq!(
            (coalesced.current_cs(), coalesced.current),
            (sequential.current_cs(), sequential.current),
            "a summed burst stopping on a header must leave the diff on the last file \
             crossed, like the unit presses it coalesces"
        );
        assert_eq!(
            (coalesced.current_cs(), coalesced.current),
            (0, 1),
            "...which is a2 here"
        );
    }

    #[test]
    fn outline_confirm_on_a_header_row_jumps_to_its_first_file_and_returns_focus() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        // CS3: pin BaseFirst explicitly — cursor 3 is hardcoded to cs-b's header under base ->
        // head row order; the confirm mechanic under test is order-agnostic.
        app.outline.order = OutlineOrder::BaseFirst;
        app.outline.open = true;
        app.outline.focused = true;
        app.outline.cursor = 3; // cs-b's header row

        app.outline_confirm();

        assert_eq!(
            app.current_cs(),
            1,
            "Enter on a header must jump to that changeset"
        );
        assert_eq!(app.current, 0, "...landing on its FIRST file");
        assert!(
            !app.outline_focused(),
            "confirming returns focus to the diff"
        );
    }

    #[test]
    fn diff_initiated_nav_syncs_the_outline_cursor_without_stealing_focus() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        assert!(!app.outline_focused(), "diff keeps focus at construction");

        app.next_changeset();
        assert!(
            !app.outline_focused(),
            "a diff-initiated nav must never steal focus from the diff to the outline"
        );
        let items = app.outline_items();
        assert_eq!(
            items[app.outline_cursor()],
            OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "b1.txt".to_string(),
                status: StagedStatus::None,
                change: FileStatus::Added,
                guides: Vec::new(),
            },
            "the outline cursor must follow the diff's new position"
        );
    }

    #[test]
    fn diff_initiated_nav_syncs_the_outline_cursor_in_tree_mode() {
        // CS4: Tree mode's rows still carry the same cs_idx/file_idx a File row always has, so
        // `sync_outline_to_current`'s match-by-those-fields logic needs no tree-specific branch —
        // this pins that it actually still lands correctly once the row also carries `guides`.
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Tree;

        app.next_changeset();
        assert!(
            !app.outline_focused(),
            "a diff-initiated nav must never steal focus from the diff to the outline"
        );
        let items = app.outline_items();
        assert_eq!(
            items[app.outline_cursor()],
            OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "b1.txt".to_string(),
                status: StagedStatus::None,
                change: FileStatus::Added,
                guides: vec![true],
            },
            "the outline cursor must follow the diff's new position, landing on b1.txt's row \
             even though Tree mode reshuffles the row order alphabetically"
        );
    }

    #[test]
    fn outline_cycle_mode_reaches_tree_and_stack_tree() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;

        app.outline_cycle_mode();
        assert_eq!(app.outline_mode(), OutlineMode::Tree);

        app.outline_cycle_mode();
        assert_eq!(app.outline_mode(), OutlineMode::StackTree);
    }

    /// A single committed changeset touching two files under `src/`, for the Dir-row no-op
    /// tests — the two-and-one-files fixture above is deliberately flat and never produces a
    /// [`OutlineItem::Dir`] row in Tree mode.
    fn single_changeset_with_nested_paths() -> App {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("src/a.txt", "a\n")
            .file("src/b.txt", "b\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: "cs".to_string(),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        app
    }

    #[test]
    fn outline_move_by_and_confirm_on_a_dir_row_do_not_jump_the_diff() {
        let mut app = single_changeset_with_nested_paths();
        app.outline.mode = OutlineMode::Tree;
        let items = app.outline_items();
        let dir_idx = items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ dir row present in Tree mode");

        let before_cs = app.current_cs();
        let before_file = app.current;

        app.outline.cursor = dir_idx;
        app.outline_move_by(0);
        assert_eq!(app.current_cs(), before_cs);
        assert_eq!(
            app.current, before_file,
            "moving onto a Dir row must not jump the diff"
        );

        app.outline.cursor = dir_idx;
        app.outline.focused = true;
        app.outline_confirm();
        assert_eq!(app.current_cs(), before_cs);
        assert_eq!(
            app.current, before_file,
            "confirming a Dir row must not jump the diff"
        );
        assert!(
            !app.outline_focused(),
            "confirm still returns focus to the diff, even as a no-op"
        );
    }

    #[test]
    fn closing_the_outline_restores_full_width_diff_rendering() {
        // A render-level assertion belongs in render.rs's own tests; this just pins the state
        // contract `render::render` reads (`outline_open`), so a regression there is caught at
        // the state layer too.
        let mut app = two_committed_changesets_two_and_one_files();
        // Default state is open+unfocused (locked design); the pure toggle closes it regardless
        // of focus.
        assert!(app.outline_open() && !app.outline_focused());
        app.toggle_outline();
        assert!(!app.outline_open());
    }

    // ── CS2: outline scrolloff viewport + g/G jumps ─────────────────────────────

    /// Four committed changesets of three files each — Stack mode (the default) yields 16 rows
    /// (header + 3 files, ×4), long enough to exercise [`App::derive_outline_scroll`]'s margin
    /// behavior against a small `outline_height`, unlike the 5-row
    /// [`two_committed_changesets_two_and_one_files`] fixture used elsewhere in this module.
    fn four_committed_changesets_three_files_each() -> App {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut base = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mut changesets = Vec::new();
        for cs_num in 0..4 {
            let head = fixture
                .commit("main")
                .file(&format!("cs{cs_num}_a.txt"), "a\n")
                .file(&format!("cs{cs_num}_b.txt"), "b\n")
                .file(&format!("cs{cs_num}_c.txt"), "c\n")
                .create(&format!("cs{cs_num}"))
                .unwrap();
            changesets.push(Changeset {
                name: format!("cs-{cs_num}"),
                span: ChangesetSpan::Committed { base, head },
                title: None,
                current: cs_num == 0,
                needs_restack: false,
            });
            base = head;
        }
        let repo = fixture.repo().unwrap();
        let views = changesets
            .into_iter()
            .map(|cs| {
                let diff = crate::acquire::diff_changeset(repo, &cs).unwrap();
                ChangesetView::from_changeset_diff(cs, diff)
            })
            .collect();

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, views);
        app.open_current();
        app.outline.mode = OutlineMode::Stack;
        assert_eq!(app.outline_items().len(), 16, "4 x (1 header + 3 files)");
        app
    }

    #[test]
    fn outline_move_by_keeps_cursor_within_the_scrolloff_margin() {
        let mut app = four_committed_changesets_three_files_each();
        app.outline_height = 5; // bottom_margin = 5 - 1 - SCROLLOFF(2) = 2
        app.outline.cursor = 0;
        app.derive_outline_scroll(app.outline_items().len());
        assert_eq!(app.outline_scroll(), 0);

        // Walk down one row at a time; the scroll must follow to keep the cursor within
        // `[scroll, scroll + bottom_margin]`, never snapping straight to the cursor.
        for _ in 0..8 {
            app.outline_move_by(1);
            let scroll = app.outline_scroll();
            let cursor = app.outline_cursor();
            assert!(
                cursor >= scroll && cursor <= scroll + 2,
                "cursor {cursor} must stay within the scrolloff-margined viewport at scroll {scroll}"
            );
        }
        assert!(
            app.outline_scroll() > 0,
            "scrolling down must have moved the viewport"
        );

        // Walking back up must scroll up minimally, not snap to zero.
        let scroll_at_bottom = app.outline_scroll();
        app.outline_move_by(-1);
        assert!(
            app.outline_scroll() <= scroll_at_bottom,
            "moving up must not increase scroll"
        );
        assert!(
            app.outline_scroll() > 0,
            "a single step up from deep in the list must not snap scroll to zero"
        );
    }

    #[test]
    fn outline_scroll_clamps_at_both_ends() {
        let mut app = four_committed_changesets_three_files_each();
        app.outline_height = 5;

        app.outline.cursor = 0;
        app.derive_outline_scroll(app.outline_items().len());
        assert_eq!(
            app.outline_scroll(),
            0,
            "top row 0 must be visible at start"
        );

        let last = app.outline_items().len() - 1;
        app.outline.cursor = last;
        app.derive_outline_scroll(app.outline_items().len());
        let scroll = app.outline_scroll();
        assert!(
            last >= scroll && last < scroll + app.outline_height,
            "the last row must be visible once the cursor reaches it"
        );
        assert!(
            scroll <= app.outline_items().len().saturating_sub(app.outline_height),
            "scroll must never run past the point where the last row leaves the viewport"
        );
    }

    #[test]
    fn outline_top_lands_cursor_zero_and_does_not_jump_a_header() {
        // CS3: the outline's default order is now HeadFirst, so Stack mode's row 0 is cs-b's
        // (the head changeset's) header, not cs-a's — see
        // `stack_mode_head_first_shows_last_changesets_header_first_with_true_cs_idx` in
        // outline.rs for the row-order pin. `outline_top`'s own contract (row 0, no diff jump)
        // is order-agnostic, so only the "which header" framing below changes.
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        app.outline_height = 3;
        app.next_changeset(); // move the diff off its start so a stray jump would be observable
        let (cs_before, file_before) = (app.current_cs(), app.current);

        app.outline.cursor = 4; // a2.txt's row under head-first order (cs-a's last file)
        app.outline_top();

        assert_eq!(app.outline_cursor(), 0, "g lands on row 0");
        assert!(
            matches!(app.outline_items()[0], OutlineItem::Header { .. }),
            "row 0 in Stack mode is a header (cs-b's, the head changeset, under head-first order)"
        );
        assert_eq!(
            (app.current_cs(), app.current),
            (cs_before, file_before),
            "landing on a Header must not jump the diff"
        );
    }

    #[test]
    fn outline_bottom_lands_on_the_last_row_and_jumps_a_file() {
        // CS3: under the new HeadFirst default, Stack mode's row order is cs-b's header/file(s)
        // first, then cs-a's — so the LAST row is cs-a's last file (a2.txt, cs_idx 0, file_idx
        // 1), not cs-b's only file as it was under the old base-first order.
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        app.outline_height = 3;
        assert_eq!(app.current_cs(), 0, "starts on cs-a");

        app.outline_bottom();

        let last = app.outline_items().len() - 1;
        assert_eq!(app.outline_cursor(), last, "G lands on the last row");
        assert!(
            matches!(app.outline_items()[last], OutlineItem::File { .. }),
            "the last row in Stack mode under head-first order is cs-a's last file, a2.txt"
        );
        assert_eq!(
            (app.current_cs(), app.current),
            (0, 1),
            "landing on a File must switch the diff there"
        );
    }

    #[test]
    fn outline_cycle_mode_and_sync_leave_scroll_consistent() {
        let mut app = four_committed_changesets_three_files_each();
        app.outline_height = 4;
        // Push the cursor (and scroll) deep into Stack mode's row list first.
        for _ in 0..10 {
            app.outline_move_by(1);
        }
        assert!(
            app.outline_scroll() > 0,
            "precondition: scrolled away from the top"
        );

        app.outline_cycle_mode(); // -> Tree
        let cursor = app.outline_cursor();
        let scroll = app.outline_scroll();
        assert!(
            cursor >= scroll && cursor < scroll + app.outline_height,
            "outline_cycle_mode must leave the cursor visible within the new mode's scroll"
        );

        app.next_changeset(); // diff-initiated nav -> sync_outline_to_current
        let cursor = app.outline_cursor();
        let scroll = app.outline_scroll();
        assert!(
            cursor >= scroll && cursor < scroll + app.outline_height,
            "sync_outline_to_current must leave the cursor visible within scroll"
        );
    }

    // ── CS7: view-config (`apply_view_config`) ─────────────────────────────────

    #[test]
    fn unset_view_config_keeps_current_defaults() {
        let fixture = FixtureBuilder::new().build().unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.outline_width(), DEFAULT_OUTLINE_WIDTH);
        assert_eq!(app.outline_mode(), OutlineMode::default());
        assert_eq!(app.outline_order(), OutlineOrder::default());
        assert_eq!(app.icon_mode(), IconMode::default());
        assert_eq!(app.layout, Layout::default());
        assert_eq!(app.zoom, Zoom::default());
    }

    #[test]
    fn outline_width_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.width", "40")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.outline_width(), 40);
    }

    #[test]
    fn outline_width_out_of_range_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.width", "9999")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.outline_width(), DEFAULT_OUTLINE_WIDTH);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("outline.width"));
    }

    #[test]
    fn outline_mode_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.mode", "tree")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.outline_mode(), OutlineMode::Tree);
    }

    #[test]
    fn outline_mode_invalid_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.mode", "bogus")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.outline_mode(), OutlineMode::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("outline.mode"));
    }

    #[test]
    fn outline_order_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.order", "base-first")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.outline_order(), OutlineOrder::BaseFirst);
    }

    #[test]
    fn outline_order_invalid_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.order", "bogus")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.outline_order(), OutlineOrder::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("outline.order"));
    }

    #[test]
    fn icon_mode_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.icons", "nerd")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.icon_mode(), IconMode::Nerd);
    }

    #[test]
    fn icon_mode_invalid_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.icons", "bogus")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.icon_mode(), IconMode::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("workon.review.icons"));
    }

    #[test]
    fn diff_layout_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.layout", "inline")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.layout, Layout::Inline);
    }

    #[test]
    fn diff_layout_invalid_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.layout", "bogus")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.layout, Layout::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("diff.layout"));
    }

    #[test]
    fn diff_zoom_overrides_default_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.zoom", "staged")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert!(warnings.is_empty());
        assert_eq!(app.zoom, Zoom::Staged);
    }

    #[test]
    fn diff_zoom_invalid_falls_back_to_default_with_warning() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.zoom", "bogus")
            .build()
            .unwrap();
        let config = ReviewConfig::new(fixture.repo().unwrap()).view_config();
        let mut app = app_from_fixture(&fixture);

        let warnings = app.apply_view_config(&config);

        assert_eq!(app.zoom, Zoom::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("diff.zoom"));
    }

    // ── CS4: summary panel ───────────────────────────────────────────────────────

    /// Force the outline open+focused with `mode` and `cursor`, matching the state
    /// `summary_target` requires — the individual state-transition tests below build off this
    /// instead of repeating the three-field setup. Pins `order` to `BaseFirst` so a fixture's
    /// base -> head file/changeset indices line up with display order (the default `HeadFirst`
    /// reverses the header row sequence — irrelevant to what's under test here, see CS3).
    fn open_focused_outline(app: &mut App, mode: OutlineMode, cursor: usize) {
        app.outline.open = true;
        app.outline.focused = true;
        app.outline.mode = mode;
        app.outline.cursor = cursor;
        app.outline.order = OutlineOrder::BaseFirst;
    }

    #[test]
    fn summary_target_is_none_when_the_outline_is_closed() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.open = false;
        app.outline.focused = false;
        assert_eq!(app.summary_target(), None);
    }

    #[test]
    fn summary_target_is_none_when_the_outline_is_open_but_unfocused() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.open = true;
        app.outline.focused = false;
        app.outline.mode = OutlineMode::Stack;
        app.outline.cursor = 0; // a Header row
        assert_eq!(
            app.summary_target(),
            None,
            "an unfocused open outline must never override the diff area (locked design)"
        );
    }

    #[test]
    fn summary_target_is_none_when_the_cursor_is_on_a_file_row() {
        let mut app = two_committed_changesets_two_and_one_files();
        open_focused_outline(&mut app, OutlineMode::Stack, 1); // cs-a's first file row
        let items = app.outline_items();
        assert!(matches!(items[1], OutlineItem::File { .. }));
        assert_eq!(app.summary_target(), None);
    }

    #[test]
    fn summary_target_is_some_changeset_on_a_header_row() {
        let mut app = two_committed_changesets_two_and_one_files();
        open_focused_outline(&mut app, OutlineMode::Stack, 0); // cs-a's header row
        let items = app.outline_items();
        assert!(matches!(items[0], OutlineItem::Header { cs_idx: 0, .. }));
        assert_eq!(app.summary_target(), Some(SummaryTarget::Changeset(0)));
    }

    #[test]
    fn summary_target_is_some_dir_with_cs_idx_none_in_tree_mode() {
        let mut app = single_changeset_with_nested_paths();
        let items = {
            app.outline.mode = OutlineMode::Tree;
            app.outline_items()
        };
        let dir_idx = items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ dir row present in Tree mode");
        open_focused_outline(&mut app, OutlineMode::Tree, dir_idx);
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Dir {
                cs_idx: None,
                path: "src".to_string(),
            }),
            "Tree mode's single cross-stack trie has no owning changeset"
        );
    }

    #[test]
    fn summary_target_is_some_dir_with_cs_idx_some_in_stack_tree_mode() {
        let mut app = single_changeset_with_nested_paths();
        let items = {
            app.outline.mode = OutlineMode::StackTree;
            app.outline_items()
        };
        let dir_idx = items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ dir row present in StackTree mode");
        open_focused_outline(&mut app, OutlineMode::StackTree, dir_idx);
        assert_eq!(
            app.summary_target(),
            Some(SummaryTarget::Dir {
                cs_idx: Some(0),
                path: "src".to_string(),
            }),
            "StackTree mode's dir row belongs to the single changeset in this fixture"
        );
    }

    #[test]
    fn summary_target_returns_none_again_after_focus_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        open_focused_outline(&mut app, OutlineMode::Stack, 0);
        assert!(app.summary_target().is_some());
        app.focus_diff();
        assert_eq!(
            app.summary_target(),
            None,
            "losing outline focus must immediately fall back to the diff body"
        );
    }

    #[test]
    fn summary_for_changeset_reflects_the_changesets_flags_and_files() {
        let mut app = two_committed_changesets_two_and_one_files();
        open_focused_outline(&mut app, OutlineMode::Stack, 0);
        let target = app.summary_target().unwrap();
        let Summary::Changeset(summary) = app.summary_for(target) else {
            panic!("expected a Changeset summary for a Header target");
        };
        assert!(summary.current, "cs-a is the current changeset");
        assert!(!summary.needs_restack);
        assert!(!summary.loading);
        assert!(!summary.failed);
        assert_eq!(summary.files.len(), 2, "cs-a touches a1.txt and a2.txt");
        assert!(summary.total_adds + summary.total_dels > 0);
    }

    #[test]
    fn summary_for_dir_in_tree_mode_aggregates_the_deduped_cross_stack_set() {
        let mut app = single_changeset_with_nested_paths();
        app.outline.mode = OutlineMode::Tree;
        let items = app.outline_items();
        let dir_idx = items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .unwrap();
        open_focused_outline(&mut app, OutlineMode::Tree, dir_idx);
        let target = app.summary_target().unwrap();
        let Summary::Dir(summary) = app.summary_for(target) else {
            panic!("expected a Dir summary for a Dir target");
        };
        assert_eq!(summary.path, "src");
        let paths: Vec<&str> = summary.files.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.txt", "src/b.txt"]);
    }

    // ── CS7: stage/unstage/discard from outline rows ─────────────────────────────

    /// Find the [`OutlineItem::File`] row index whose full path is `path` (in the CURRENT outline
    /// mode/order) — the CS7 tests' stand-in for "click the row named X", since a row's raw index
    /// shifts with mode/order and none of these tests want to hardcode it.
    fn outline_file_row(app: &App, path: &str) -> usize {
        app.outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::File { path: p, .. } if p == path))
            .unwrap_or_else(|| panic!("no outline File row for {path:?}"))
    }

    #[test]
    fn outline_stage_on_an_unstaged_file_row_stages_it_and_keeps_the_cursor_there() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let idx = outline_file_row(&app, "a.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_stage();

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_staged_file("a.txt"));
        assert!(
            app.outline_focused(),
            "outline must keep focus across the op"
        );
        match &app.outline_items()[app.outline_cursor()] {
            OutlineItem::File { path, status, .. } => {
                assert_eq!(path, "a.txt");
                assert_eq!(
                    *status,
                    StagedStatus::Staged,
                    "row now shows the staged glyph"
                );
            }
            other => panic!("expected the cursor to stay on a.txt's File row, got {other:?}"),
        }
    }

    #[test]
    fn outline_stage_on_a_staged_file_row_unstages_it() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("new.txt", "hello\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let idx = outline_file_row(&app, "new.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_stage();

        assert!(
            app.notice.is_none(),
            "unstage must succeed: {:?}",
            app.notice
        );
        let repo = fixture.repo().unwrap();
        // An Added file has no HEAD entry, so unstaging it lands as untracked — same outcome
        // `stage_file_in_staged_pane_unstages_whole_file` pins for the diff-pane path.
        repo.assert(predicate::repo::has_untracked_file("new.txt"));
    }

    #[test]
    fn outline_stage_on_a_dir_row_stages_every_unstaged_file_under_it() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("src/a.txt", "a\n", "a\nCHANGED\n")
            .unstaged_file("src/b.txt", "b\n", "b\nCHANGED\n")
            .unstaged_file("top.txt", "t\n", "t\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.outline.mode = OutlineMode::StackTree;
        let dir_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { path, .. } if path == "src"))
            .expect("src/ dir row present in StackTree mode");
        open_focused_outline(&mut app, OutlineMode::StackTree, dir_idx);

        app.outline_stage();

        assert!(
            app.notice.is_none(),
            "dir stage must succeed: {:?}",
            app.notice
        );
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_staged_file("src/a.txt"));
        repo.assert(predicate::repo::has_staged_file("src/b.txt"));
        // The file outside `src/` must be left alone.
        repo.assert(predicate::repo::has_unstaged_file("top.txt"));
    }

    #[test]
    fn outline_stage_on_a_dir_row_applies_each_files_own_verb_under_mixed_status() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("src/a.txt", "a\n", "a\nCHANGED\n")
            .staged_file("src/b.txt", "b\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.outline.mode = OutlineMode::StackTree;
        let dir_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { path, .. } if path == "src"))
            .expect("src/ dir row present in StackTree mode");
        open_focused_outline(&mut app, OutlineMode::StackTree, dir_idx);

        app.outline_stage();

        assert!(
            app.notice.is_none(),
            "mixed-status dir stage must succeed: {:?}",
            app.notice
        );
        let repo = fixture.repo().unwrap();
        // The unstaged file stages...
        repo.assert(predicate::repo::has_staged_file("src/a.txt"));
        // ...and the already-staged (Added, no HEAD entry) file unstages to untracked — each
        // file's own verb, not a single direction applied to the whole directory.
        repo.assert(predicate::repo::has_untracked_file("src/b.txt"));
    }

    #[test]
    fn outline_stage_on_the_header_row_refuses_without_touching_the_index() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        // Index 0 in Stack mode is always the changeset Header row.
        open_focused_outline(&mut app, OutlineMode::Stack, 0);
        assert!(matches!(app.outline_items()[0], OutlineItem::Header { .. }));

        app.outline_stage();

        let notice = app
            .notice
            .as_ref()
            .expect("staging a Header row must refuse");
        assert_eq!(notice.severity, Severity::Error);
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::has_unstaged_file("a.txt"));
    }

    #[test]
    fn outline_stage_on_a_committed_changesets_file_row_refuses_with_committed_wording() {
        let mut app = committed_and_uncommitted_stack();
        // `BaseFirst` order + Stack mode: Header(committed) 0, File(committed/c1.txt) 1,
        // Header(uncommitted) 2, File(uncommitted/u1.txt) 3.
        open_focused_outline(&mut app, OutlineMode::Stack, 1);
        assert!(matches!(
            &app.outline_items()[1],
            OutlineItem::File { cs_idx, path, .. } if *cs_idx == 0 && path == "c1.txt"
        ));

        app.outline_stage();

        let notice = app
            .notice
            .as_ref()
            .expect("staging a committed changeset's row must refuse");
        assert_eq!(notice.severity, Severity::Error);
        assert!(
            notice.text.contains("already committed"),
            "got: {:?}",
            notice.text
        );
    }

    #[test]
    fn outline_discard_on_a_file_row_requests_confirm_then_y_reverts_the_worktree() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "ONE\ntwo\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let idx = outline_file_row(&app, "a.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_discard();

        let confirm = app
            .pending_confirm
            .as_ref()
            .expect("discard must request a confirm");
        assert!(
            confirm.prompt.contains("a.txt"),
            "got: {:?}",
            confirm.prompt
        );
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("a.txt", "ONE\ntwo\n"));

        app.resolve_confirm(true);

        assert!(app.pending_confirm.is_none(), "y must clear the confirm");
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("a.txt", "one\ntwo\n"));
    }

    #[test]
    fn outline_discard_survives_an_intervening_refresh_that_shifts_file_indices() {
        // The confirm modal doesn't stop the tick beat: an external index change can trigger a
        // full refresh between `d` and `y`, shifting every (cs_idx, file_idx). The pending op
        // stores (changeset name, path) pairs and re-resolves at answer time, so the discard
        // must still hit the file it was requested on — not whatever now sits at its old index.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("b.txt", "one\ntwo\n", "ONE\ntwo\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let idx = outline_file_row(&app, "b.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_discard();
        assert!(app.pending_confirm.is_some());

        // A new modified file that sorts BEFORE b.txt enters the diff while the confirm is up,
        // then a refresh rebuilds the file lists — b.txt's file_idx shifts by one.
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join("a.txt"), "NEW\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.write().unwrap();
        std::fs::write(workdir.join("a.txt"), "NEW\nCHANGED\n").unwrap();
        app.refresh();
        assert!(
            app.pending_confirm.is_some(),
            "the refresh must not consume the pending confirm"
        );

        app.resolve_confirm(true);

        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("b.txt", "one\ntwo\n"));
        repo.assert(predicate::repo::workdir_file_equals(
            "a.txt",
            "NEW\nCHANGED\n",
        ));
    }

    #[test]
    fn outline_discard_confirm_n_cancels_and_leaves_the_worktree_unchanged() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "ONE\ntwo\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let idx = outline_file_row(&app, "a.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_discard();
        app.resolve_confirm(false);

        assert!(app.pending_confirm.is_none(), "n must clear the confirm");
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("a.txt", "ONE\ntwo\n"));
    }

    #[test]
    fn outline_discard_on_a_dir_row_names_the_scope_then_y_discards_every_file_under_it() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("src/a.txt", "a\n", "A\n")
            .unstaged_file("src/b.txt", "b\n", "B\n")
            .unstaged_file("top.txt", "t\n", "T\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.outline.mode = OutlineMode::StackTree;
        let dir_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { path, .. } if path == "src"))
            .expect("src/ dir row present in StackTree mode");
        open_focused_outline(&mut app, OutlineMode::StackTree, dir_idx);

        app.outline_discard();

        let confirm = app
            .pending_confirm
            .as_ref()
            .expect("dir discard must request a confirm");
        assert!(
            confirm.prompt.contains('2') && confirm.prompt.contains("src"),
            "prompt must name the file count and the scoped path, got: {:?}",
            confirm.prompt
        );

        app.resolve_confirm(true);

        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("src/a.txt", "a\n"));
        repo.assert(predicate::repo::workdir_file_equals("src/b.txt", "b\n"));
        // The file outside `src/` must be left untouched.
        repo.assert(predicate::repo::workdir_file_equals("top.txt", "T\n"));
    }

    #[test]
    fn outline_stage_in_a_multi_file_outline_keeps_the_cursor_on_the_acted_on_row_not_the_diffs_current_file(
    ) {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "a\n", "a\nCHANGED\n")
            .unstaged_file("b.txt", "b\n", "b\nCHANGED\n")
            .unstaged_file("c.txt", "c\n", "c\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(
            app.files()[app.current].path,
            "a.txt",
            "the diff opens on the first file, a.txt — never touched by this test"
        );
        let idx = outline_file_row(&app, "b.txt");
        open_focused_outline(&mut app, OutlineMode::Stack, idx);

        app.outline_stage();

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        match &app.outline_items()[app.outline_cursor()] {
            OutlineItem::File { path, status, .. } => {
                assert_eq!(
                    path, "b.txt",
                    "the cursor must stay on the acted-on row, not drift to the diff's own \
                     current file (a.txt, via sync_outline_to_current inside coordinated_refresh)"
                );
                assert_eq!(*status, StagedStatus::Staged);
            }
            other => panic!("expected the cursor on b.txt's File row, got {other:?}"),
        }
    }

    // ── CS8: progressive gap expansion ──────────────────────────────────────

    /// A single-file fixture with two hunks separated by a wide (40-line) unchanged run — wide
    /// enough that even a full 10/10 [`App::expand_gap_at_cursor`] press still leaves a
    /// surviving [`DisplayRow::Gap`] (`40 - 2*3 - 2*10 = 14` rows still hidden), unlike
    /// [`two_hunk_fixture`]'s much narrower gap.
    fn two_hunks_with_a_wide_gap_fixture() -> Fixture {
        let mut committed = String::from("OLD_HUNK_A\n");
        let mut modified = String::from("NEW_HUNK_A\n");
        for i in 1..=40 {
            committed.push_str(&format!("ctx{i}\n"));
            modified.push_str(&format!("ctx{i}\n"));
        }
        committed.push_str("OLD_HUNK_B\n");
        modified.push_str("NEW_HUNK_B\n");

        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.txt", &committed, &modified)
            .build()
            .unwrap()
    }

    /// The display-row index of the current file's ONLY gap row — the fixture shape every CS8
    /// expansion test below relies on.
    fn only_gap_row(app: &App) -> usize {
        app.current_view_ref()
            .expect("loaded view")
            .display
            .iter()
            .position(|r| matches!(r, DisplayRow::Gap { .. }))
            .expect("expected exactly one gap row")
    }

    #[test]
    fn expand_gap_cancels_an_active_selection_but_a_non_gap_press_leaves_it_alone() {
        // An expansion reshapes the focused pane's row space, so `selection_anchor`'s invariant
        // (cancel, never translate) applies — a selection made before the expand would silently
        // cover different lines after it. The non-gap no-op path must NOT cancel: nothing
        // reshaped.
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        app.start_selection();
        assert!(app.selection_anchor.is_some(), "selection must start");
        app.expand_gap_at_cursor(false); // cursor sits on the first hunk, not a gap: no-op
        assert!(
            app.selection_anchor.is_some(),
            "a no-op press on a non-gap row must leave the selection alone"
        );

        app.cursor = only_gap_row(&app);
        app.expand_gap_at_cursor(false);
        assert!(
            app.selection_anchor.is_none(),
            "an actual expansion reshapes the row space and must cancel the selection"
        );
    }

    #[test]
    fn expand_gap_at_cursor_on_a_gap_row_reveals_more_rows() {
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        let before_len = app.current_view_ref().unwrap().display.len();
        app.cursor = gap_row;

        app.expand_gap_at_cursor(false);

        let view = app.current_view_ref().unwrap();
        assert!(
            view.display.len() > before_len,
            "expanding must reveal more rows: {before_len} -> {}",
            view.display.len()
        );
        assert!(
            app.cursor < view.display.len(),
            "cursor must stay in bounds"
        );
        assert!(
            matches!(view.display[app.cursor], DisplayRow::Row(_)),
            "the cursor's old index (the gap's leading edge) must now hold a revealed row, not \
             the gap marker: {:?}",
            view.display[app.cursor]
        );
        // The gap is wide enough (40 hidden rows) that a single 10/10 press doesn't consume it.
        assert!(
            view.display
                .iter()
                .any(|r| matches!(r, DisplayRow::Gap { .. })),
            "a partial expansion of this fixture must still leave a gap row"
        );
    }

    #[test]
    fn expand_gap_at_cursor_on_a_non_gap_row_is_a_no_op() {
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // cursor lands on hunk A's row, not the gap

        let before_len = app.current_view_ref().unwrap().display.len();
        let before_cursor = app.cursor;
        assert!(
            !matches!(
                app.current_view_ref().unwrap().display[before_cursor],
                DisplayRow::Gap { .. }
            ),
            "precondition: cursor starts on hunk A, not the gap"
        );

        app.expand_gap_at_cursor(false);

        assert_eq!(app.cursor, before_cursor, "no-op must not move the cursor");
        assert_eq!(
            app.current_view_ref().unwrap().display.len(),
            before_len,
            "no-op must not change the row count"
        );
        assert!(app.notice.is_none(), "a no-op must not raise a notice");
    }

    #[test]
    fn stage_hunk_after_expanding_a_gap_stages_the_intended_hunk() {
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;
        app.expand_gap_at_cursor(false);

        // Move to hunk B (the LATER hunk) through the freshly rebuilt `display`/`display_hunk` —
        // this is the coordinate-space desync CS8 must not introduce: `display_hunk` is
        // recomputed by `rebuild_rows` from the SAME `aligned`/`hunks` every time, so the row
        // under the cursor must still resolve to the right hunk index after an expansion.
        app.next_hunk_row();
        app.stage_hunk();

        assert!(app.notice.is_none(), "stage must succeed: {:?}", app.notice);
        let repo = fixture.repo().unwrap();
        let mut expected_index = String::from("OLD_HUNK_A\n");
        let mut expected_workdir = String::from("NEW_HUNK_A\n");
        for i in 1..=40 {
            expected_index.push_str(&format!("ctx{i}\n"));
            expected_workdir.push_str(&format!("ctx{i}\n"));
        }
        expected_index.push_str("NEW_HUNK_B\n");
        expected_workdir.push_str("NEW_HUNK_B\n");
        // The index picks up ONLY hunk B; hunk A must stay unstaged.
        repo.assert(predicate::repo::index_blob_equals(
            "f.txt",
            expected_index.as_str(),
        ));
        repo.assert(predicate::repo::workdir_file_equals(
            "f.txt",
            expected_workdir.as_str(),
        ));
    }

    #[test]
    fn expanding_a_gap_clears_the_row_keyed_word_span_cache() {
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // cursor on hunk A's row — a word-diff pair

        let hunk_a_row = app.cursor;
        app.current_view().unwrap().word_spans_for_row(hunk_a_row);
        assert!(
            !app.current_view_ref().unwrap().word_spans.is_empty(),
            "precondition: the cache must be populated before expanding"
        );

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;
        app.expand_gap_at_cursor(false);

        assert!(
            app.current_view_ref().unwrap().word_spans.is_empty(),
            "rebuild_rows must clear the row-keyed word-span cache — a stale entry would \
             mismatch the row it renders under post-expansion"
        );
        // The cache is still USABLE post-clear, not just permanently empty — re-populating it
        // must not panic and must produce a non-empty span for the still-word-diffable row.
        let (old_spans, new_spans) = app.current_view().unwrap().word_spans_for_row(hunk_a_row);
        assert!(
            !old_spans.is_empty() || !new_spans.is_empty(),
            "hunk A is still a word-diff pair after the rebuild"
        );
    }

    #[test]
    fn refresh_resets_a_files_gap_expansions() {
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;
        app.expand_gap_at_cursor(false);
        let expanded_len = app.current_view_ref().unwrap().display.len();

        app.refresh(); // ends with its own `open_current`, same as every other refresh path

        let view = app.current_view_ref().expect("view survives refresh");
        assert!(
            view.display.len() < expanded_len,
            "a fresh view must re-collapse to the base gap window, not carry over the prior \
             expansion: expanded {expanded_len}, post-refresh {}",
            view.display.len()
        );
        assert!(
            view.display
                .iter()
                .any(|r| matches!(r, DisplayRow::Gap { .. })),
            "the gap must be back in its base (still-collapsed) form"
        );
    }

    // ── CS9: reveal gaps to the enclosing tree-sitter scope ─────────────────

    /// A `.rs` fixture where both edits sit inside the SAME long function, with a 40-line
    /// unchanged run between them wide enough that even a +10/+10 press would still leave a
    /// gap (mirrors [`two_hunks_with_a_wide_gap_fixture`]'s width) — but because the whole
    /// hidden run lies inside `long_function`'s body, a scope-reveal press should uncover it
    /// ENTIRELY (the function encloses the whole gap), unlike +10/+10.
    fn function_with_a_wide_internal_gap_fixture() -> Fixture {
        let mut committed = String::from("fn long_function() {\n    let a = OLD_A;\n");
        let mut modified = String::from("fn long_function() {\n    let a = NEW_A;\n");
        for i in 1..=40 {
            committed.push_str(&format!("    ctx{i}();\n"));
            modified.push_str(&format!("    ctx{i}();\n"));
        }
        committed.push_str("    let b = OLD_B;\n}\n");
        modified.push_str("    let b = NEW_B;\n}\n");

        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.rs", &committed, &modified)
            .build()
            .unwrap()
    }

    /// A `.rs` fixture where both edits sit at the TOP LEVEL (no enclosing function/impl/etc —
    /// only comment lines separate them), so [`crate::scope::enclosing_scope_lines`] finds no
    /// allowlisted ancestor around the anchor and a press must fall back to +10/+10 exactly like
    /// a grammar-less file.
    fn top_level_edits_with_a_wide_gap_fixture() -> Fixture {
        let mut committed = String::from("static A: i32 = OLD_A;\n");
        let mut modified = String::from("static A: i32 = NEW_A;\n");
        for i in 1..=40 {
            committed.push_str(&format!("// ctx{i}\n"));
            modified.push_str(&format!("// ctx{i}\n"));
        }
        committed.push_str("static B: i32 = OLD_B;\n");
        modified.push_str("static B: i32 = NEW_B;\n");

        FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("f.rs", &committed, &modified)
            .build()
            .unwrap()
    }

    /// The `skipped` count of the current file's only [`DisplayRow::Gap`], found by scanning
    /// `display` (NOT via `app.cursor` — expanding the gap's leading edge shifts the gap marker
    /// to a later index, same as [`only_gap_row`] re-finds it after an expansion in the CS8
    /// tests above). Panics if there isn't exactly one gap row.
    fn gap_skipped(app: &App) -> usize {
        let row = only_gap_row(app);
        match app.current_view_ref().expect("loaded view").display[row] {
            DisplayRow::Gap { skipped, .. } => skipped,
            other => panic!("expected a Gap row, got {other:?}"),
        }
    }

    #[test]
    fn scope_reveal_uncovers_the_whole_gap_when_the_enclosing_function_covers_it() {
        let fixture = function_with_a_wide_internal_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;

        app.expand_gap_at_cursor(false);

        let view = app.current_view_ref().unwrap();
        assert!(
            !view
                .display
                .iter()
                .any(|r| matches!(r, DisplayRow::Gap { .. })),
            "the enclosing function covers the ENTIRE hidden run, so a single scope-reveal press \
             must consume the gap completely — unlike a flat +10/+10 press, which would still \
             leave one on this fixture's 40-row gap: {:?}",
            view.display
        );
    }

    #[test]
    fn a_grammarless_file_falls_back_to_the_flat_plus_ten_reveal() {
        // Reuse CS8's `.txt` fixture (no bundled grammar for that extension) — the scope-reveal
        // path must find no lang key and fall straight through to +10/+10, same as before CS9.
        let fixture = two_hunks_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;
        let skipped_before = gap_skipped(&app);

        app.expand_gap_at_cursor(false);

        let skipped_after = gap_skipped(&app);
        assert_eq!(
            skipped_before - skipped_after,
            20,
            "no grammar for .txt: exactly the flat 10-before/10-after reveal, same as CS8"
        );
    }

    #[test]
    fn a_scope_with_nothing_new_falls_back_to_the_flat_plus_ten_reveal() {
        // Both edits are top-level `static`s with no enclosing function/impl/etc — the anchor
        // line has no allowlisted ancestor, so scope-reveal finds nothing and must fall back to
        // +10/+10 exactly like the grammarless case, even though this file DOES have a grammar.
        let fixture = top_level_edits_with_a_wide_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;
        let skipped_before = gap_skipped(&app);

        app.expand_gap_at_cursor(false);

        let skipped_after = gap_skipped(&app);
        assert_eq!(
            skipped_before - skipped_after,
            20,
            "no enclosing scope at the top level: falls back to the flat 10-before/10-after reveal"
        );
    }

    #[test]
    fn full_expand_ignores_scope_reveal_regardless_of_grammar() {
        // `E` (full=true) must stay pure CS8 behavior even on a file with a grammar and a scope
        // that would otherwise apply — scope-reveal is an `Enter`-only (CS9) refinement.
        let fixture = function_with_a_wide_internal_gap_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let gap_row = only_gap_row(&app);
        app.cursor = gap_row;

        app.expand_gap_at_cursor(true);

        let view = app.current_view_ref().unwrap();
        assert!(
            !view
                .display
                .iter()
                .any(|r| matches!(r, DisplayRow::Gap { .. })),
            "E must fully expand the gap: {:?}",
            view.display
        );
    }

    // ── CS10: mouse (click-to-focus, wheel scrolling) ────────────────────────────

    #[test]
    fn click_on_an_outline_file_row_focuses_selects_and_jumps_the_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        app.outline.order = OutlineOrder::BaseFirst;
        // BaseFirst Stack order: header(cs-a)=0, a1.txt=1, a2.txt=2, header(cs-b)=3, b1.txt=4.
        app.outline_height = 10;
        app.derive_outline_scroll(app.outline_items().len());
        app.hit_regions.outline = Some(Region {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        });
        assert!(!app.outline_focused(), "starts unfocused (locked default)");

        // Row 2 (a2.txt) at the outline's top-of-viewport (scroll 0) is screen row 2.
        app.handle_click(5, 2);

        assert!(app.outline_focused(), "a click on the outline focuses it");
        assert_eq!(app.outline_cursor(), 2);
        assert_eq!(app.current_cs(), 0);
        assert_eq!(
            app.files()[app.current].path,
            "a2.txt",
            "a File row's click must jump the diff there, like outline_move_to"
        );
    }

    #[test]
    fn click_on_an_outline_header_row_selects_without_jumping_the_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
        app.outline.order = OutlineOrder::BaseFirst;
        app.outline_height = 10;
        app.derive_outline_scroll(app.outline_items().len());
        app.hit_regions.outline = Some(Region {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        });
        let before_cs = app.current_cs();
        let before_file = app.current;

        // Row 3 is cs-b's header.
        app.handle_click(5, 3);

        assert!(app.outline_focused());
        assert_eq!(app.outline_cursor(), 3);
        assert_eq!(
            (app.current_cs(), app.current),
            (before_cs, before_file),
            "a Header row's click must not jump the diff"
        );
        assert!(
            app.summary_target().is_some(),
            "selecting a Header row (outline open + focused) must surface the summary panel"
        );
    }

    #[test]
    fn click_in_the_single_diff_pane_focuses_it_and_moves_the_cursor_to_the_clicked_row() {
        // 40 single-line rows (mirrors `derive_scroll_keeps_scrolloff_margin_and_slides_minimally`
        // above) — long enough that clicking row 4 lands there without clamping against a tiny
        // real diff.
        let lines: String = (1..=40).map(|n| format!("l{n}\n")).collect();
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("big.txt", &lines)
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.focus_outline();
        assert!(app.outline_focused());
        app.pane_height = 10;
        app.cursor = 0;
        app.scroll = 0;
        app.hit_regions.single = Some(Region {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        });

        app.handle_click(10, 4);

        assert!(
            !app.outline_focused(),
            "a click in the diff pane must return focus to the diff"
        );
        assert_eq!(
            app.cursor, 4,
            "the cursor must land on the clicked row (scroll 0 + offset 4)"
        );
    }

    #[test]
    fn click_in_the_unfocused_split_pane_flips_split_focus_and_moves_its_cursor() {
        let fixture = partial_fixture();
        let mut app = app_from_fixture(&fixture);
        app.open_current(); // Split; focused pane defaults to Unstaged
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert_eq!(app.split_focus_role(), Role::Unstaged);

        app.pane_height = 5;
        app.alt_height = 5;
        app.derive_scroll();
        app.derive_alt_scroll();
        app.hit_regions.unstaged = Some(Region {
            x: 0,
            y: 1,
            w: 40,
            h: 5,
        });
        app.hit_regions.staged = Some(Region {
            x: 0,
            y: 7,
            w: 40,
            h: 5,
        });

        // Row 1 inside the staged region (y=7, height 5) — the currently UNFOCUSED pane. `f.txt`
        // is a 3-line file (alpha/beta/gamma), so offset 1 stays within its row count either way.
        app.handle_click(3, 8);

        assert_eq!(
            app.split_focus_role(),
            Role::Staged,
            "a click in the unfocused pane must flip split_focus onto it"
        );
        let (_, cursor) = app.pane_render_state(Role::Staged);
        assert_eq!(
            cursor,
            Some(1),
            "the newly-focused pane's cursor must land on the clicked row (offset 1 into the region)"
        );
    }

    #[test]
    fn wheel_over_the_outline_focuses_it_and_moves_the_cursor_by_delta_with_scrolloff() {
        let mut app = four_committed_changesets_three_files_each();
        app.outline_height = 5; // bottom_margin = 5 - 1 - SCROLLOFF(2) = 2
        app.outline.cursor = 0;
        app.derive_outline_scroll(app.outline_items().len());
        app.hit_regions.outline = Some(Region {
            x: 0,
            y: 0,
            w: 20,
            h: 5,
        });
        assert!(!app.outline_focused());

        app.handle_wheel(5, 2, 3);

        assert!(app.outline_focused(), "a wheel event focuses its pane");
        assert_eq!(app.outline_cursor(), 3);
        let scroll = app.outline_scroll();
        assert!(
            app.outline_cursor() >= scroll && app.outline_cursor() <= scroll + 2,
            "the outline scroll must follow the wheel-moved cursor via the normal scrolloff derive"
        );
    }

    #[test]
    fn wheel_over_the_focused_diff_pane_moves_the_cursor_by_delta() {
        // Same 40-line fixture as the click test above — enough rows that a ±3 wheel move never
        // clamps against a tiny real diff.
        let lines: String = (1..=40).map(|n| format!("l{n}\n")).collect();
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("big.txt", &lines)
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.pane_height = 10;
        app.cursor = 5;
        app.scroll = 0;
        app.hit_regions.single = Some(Region {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        });
        let cursor_before = app.cursor;

        app.handle_wheel(10, 3, 3);
        assert_eq!(app.cursor, cursor_before + 3);

        app.handle_wheel(10, 3, -3);
        assert_eq!(app.cursor, cursor_before);
    }

    #[test]
    fn click_outside_every_hit_region_is_a_no_op() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline_height = 10;
        app.pane_height = 10;
        app.hit_regions = HitRegions {
            outline: Some(Region {
                x: 0,
                y: 0,
                w: 20,
                h: 10,
            }),
            single: Some(Region {
                x: 21,
                y: 0,
                w: 40,
                h: 10,
            }),
            unstaged: None,
            staged: None,
        };
        let outline_focused_before = app.outline_focused();
        let cursor_before = app.cursor;
        let outline_cursor_before = app.outline_cursor();
        let current_before = (app.current_cs(), app.current);

        // Row 0 sits above both content regions (a header row at y=0 in either would collide —
        // pick a column between the two panes' widths, on the divider itself).
        app.handle_click(20, 0);

        assert_eq!(app.outline_focused(), outline_focused_before);
        assert_eq!(app.cursor, cursor_before);
        assert_eq!(app.outline_cursor(), outline_cursor_before);
        assert_eq!((app.current_cs(), app.current), current_before);
    }
}
