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
use crate::align::{align_file, collapse_gaps, inline_rows, CellKind, DisplayRow, InlineRow, Row};
use crate::apply::{Git2Applier, StageVerb};
use crate::config::RawViewConfig;
use crate::highlight::{FgSpan, TsHighlighter};
use crate::model::{DiffModel, FileChange, FileStatus, Hunk, LineKind};
use crate::ops;
use crate::outline::{self, OutlineChangeset, OutlineFile, OutlineItem, OutlineMode};
use crate::queue::{OpOutcome, StagingOp, StagingQueue};
use crate::refresh::{IndexSignature, RefreshCoordinator};
use crate::source::{resolve_source, Source};
use crate::stage_op::{FileStagingOp, LineSelectionOp};
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
        let inline = inline_rows(&display);
        let first_inline_hunk_row = inline
            .iter()
            .position(is_inline_hunk_content_row)
            .unwrap_or(0);

        let display_hunk = display
            .iter()
            .map(|row| {
                let (old, new) = display_row_linenos(row);
                hunk_for_linenos(&file.hunks, old, new)
            })
            .collect();
        let inline_hunk = inline
            .iter()
            .map(|row| {
                let (old, new) = inline_row_linenos(row);
                hunk_for_linenos(&file.hunks, old, new)
            })
            .collect();

        Self {
            old_text,
            new_text,
            old_lines,
            new_lines,
            display,
            first_hunk_row,
            first_inline_hunk_row,
            old_hl,
            new_hl,
            word_spans: HashMap::new(),
            inline,
            inline_word_spans: HashMap::new(),
            display_hunk,
            inline_hunk,
        }
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
        ChangesetSpan::Committed { head, .. } => repo.find_commit(head).and_then(|c| c.tree()).ok(),
        ChangesetSpan::Uncommitted => None,
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
        }
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
        let outline = OutlineState {
            open: changesets.len() > 1,
            focused: false,
            cursor: 0,
            mode: OutlineMode::default(),
            width: DEFAULT_OUTLINE_WIDTH,
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
            help_visible: false,
            review_source: None,
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

    /// Index into the reviewed stack of the active changeset — read by tests asserting the
    /// [`Self::from_changesets`]/[`Self::refresh`] "honor lib `current`" rule (locked decision
    /// #6), and by changeset-nav's own tests ([`Self::next_changeset`]/[`Self::prev_changeset`]/
    /// [`Self::goto_changeset`]).
    pub fn current_cs(&self) -> usize {
        self.current_cs
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
        matches!(self.cur().cs.span, ChangesetSpan::Committed { .. })
    }

    /// Re-run [`crate::acquire::resolve_changesets`] against the CURRENT `HEAD` branch and
    /// rebuild every [`ChangesetView`] from scratch — the operation both a manual refresh (`r`)
    /// and (later) a post-staging-op/external-write refresh need. Re-assembling (not just
    /// re-diffing the active changeset) matters because a restack can change the stack's
    /// topology, not just its diffs.
    ///
    /// - Rebuilds [`Self::changesets`] and [`Self::base_label`] in place. Does NOT touch `repo`
    ///   (same handle), `highlighter` (its per-instance grammar cache would have to re-parse
    ///   every language from scratch if rebuilt), `layout`, or `zoom` (the user's current view
    ///   mode shouldn't reset just because they pressed `r`, or because a background refresh
    ///   fired).
    /// - Preserves the active changeset by NAME: if a changeset with that name still exists in
    ///   the rebuilt stack, `current_cs` follows it; otherwise it falls back to whichever
    ///   changeset the lib now reports as `current`, or index `0`.
    /// - Preserves file position by PATH within the (possibly different) active changeset, same
    ///   rule M4 used: `current` follows the path if it still exists, else clamps into the new
    ///   list (or `0` if empty).
    /// - Re-seats the (possibly changed) current file at its first hunk via [`Self::open_current`]
    ///   — the same path a file switch already uses. This does NOT try to preserve the exact
    ///   cursor row: the rows under an old cursor position may no longer correspond to the same
    ///   content once the diff is rebuilt, so jumping to the first hunk (like opening a file fresh)
    ///   is the only always-valid choice, consistent with how zoom/layout switches already treat
    ///   cursor position as non-transferable across a reshape.
    ///
    /// On any assembly/diff error, leaves all existing state untouched and sets an error
    /// [`Notice`] instead (via [`Self::notify`]) — a failed refresh must never blank the review.
    ///
    /// Dispatches on [`Self::review_source`] (M7 CS2 fix): a no-argument launch (`None`) re-runs
    /// today's auto-detect ([`crate::acquire::resolve_changesets`]); an explicit-source launch
    /// (`Some`) re-runs [`crate::source::resolve_source`] against THAT source, never auto-detect
    /// — every CS2 source variant (`Stack`, `Uncommitted`) is offline, so re-resolving on every
    /// refresh (manual `r` and the tick-driven index watcher alike) is cheap and safe. Without
    /// this, both refresh triggers would silently swap an explicit review (e.g. `uncommitted`)
    /// for the current `HEAD`'s auto-detected state.
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

        let mut views = Vec::with_capacity(changesets.len());
        for cs in changesets {
            match crate::acquire::diff_changeset(&self.repo, &cs) {
                Ok(diff) => views.push(ChangesetView::from_changeset_diff(cs, diff)),
                Err(err) => {
                    self.notify(format!("refresh failed: {err}"), Severity::Error);
                    return;
                }
            }
        }
        // `resolve_changesets` always returns at least one changeset (a lone Uncommitted entry
        // when no stack is active), but stay defensive rather than index an empty `Vec` below.
        if views.is_empty() {
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

        self.current_cs = views
            .iter()
            .position(|v| v.cs.name == prev_cs_name)
            .unwrap_or_else(|| current_cs_index(&views));
        self.base_label = base_label_for(&views[self.current_cs].cs);
        self.changesets = views;

        let n = self.cur().diff.files.len();
        self.current = current_path
            .and_then(|path| self.cur().diff.files.iter().position(|f| f.path == path))
            .unwrap_or(if n == 0 { 0 } else { self.current.min(n - 1) });

        self.open_current();
        // The rebuilt changeset list can resize/reorder the outline's row list out from under
        // its cursor — reposition it, same as every other diff-initiated nav (does NOT touch
        // `outline.open`/`focused`/`mode`, which persist across a refresh like `layout`/`zoom`).
        self.sync_outline_to_current();
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
    /// has no change for the file, or it isn't loaded yet).
    pub(crate) fn role_view_ref(&self, idx: usize, role: Role) -> Option<&FileView> {
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
            if file.is_binary {
                return;
            }
            // Build the view in a block so `head_tree` (which borrows `self.repo`) drops before
            // the `views_for_mut` reborrow — same reason the combined path below can assign a
            // direct field while `head_tree` is live but this method-call path cannot. `file` is
            // cloned out of `self.cur()` for the same reason: `FileView::load` needs `&self.repo`
            // and `&mut self.highlighter` at once, which a borrow still anchored in `self.cur()`
            // would conflict with.
            let view = {
                // Re-peeled per call, same rationale as the combined path below.
                let Ok(head_tree) = self.repo.head().and_then(|h| h.peel_to_tree()) else {
                    return;
                };
                // Non-Combined roles are uncommitted-only (committed changesets have empty
                // staged/unstaged sub-models), so the new side always stays worktree/index —
                // `None` here preserves that exactly.
                FileView::load(
                    &self.repo,
                    &head_tree,
                    None,
                    &file,
                    role,
                    &mut self.highlighter,
                )
            };
            self.views_for_mut(role)[idx] = Some(view);
            return;
        }

        // Combined role.
        // Re-peeled per call rather than cached on `App`: for the uncommitted layer `HEAD` can
        // move between file loads, and the tree is cheap to re-peel either way.
        // `self.cur().cs.span` is `Copy`, so reading it here borrows `self` only for this
        // sub-expression — `head_tree` itself ends up borrowing `self.repo` alone (via the free
        // `old_side_tree_for`), leaving `&mut self.highlighter` free below. A method tied to
        // `&self` would instead have bound the tree's lifetime to all of `self`.
        let Some(head_tree) = old_side_tree_for(&self.repo, self.cur().cs.span) else {
            return;
        };
        // New-side source mirrors the old side: `None` (worktree) for the uncommitted layer,
        // the changeset's `head` tree for a committed changeset. Same free-fn borrow dance as
        // `old_side_tree_for` — both trees borrow only `self.repo`, so `&mut self.highlighter`
        // stays free for `FileView::load`.
        let new_tree = new_side_tree_for(&self.repo, self.cur().cs.span);
        let file = self.cur().diff.files[idx].clone();
        let view = FileView::load(
            &self.repo,
            &head_tree,
            new_tree.as_ref(),
            &file,
            Role::Combined,
            &mut self.highlighter,
        );
        drop(head_tree);
        drop(new_tree);
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

    /// Load the current file's needed views and reset both panes to their first hunks.
    pub fn open_current(&mut self) {
        self.ensure_loaded(self.current);
        self.reset_panes();
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

    /// Snapshot every reviewed changeset into [`OutlineChangeset`]/[`OutlineFile`] and build the
    /// current [`OutlineMode`]'s row list — the outline cursor's index space, and the source of
    /// truth `render.rs` draws from. Rebuilt fresh on every call (cheap: a small stack times a
    /// handful of files each, no caching, same posture as [`Self::effective_zoom_for`]) rather
    /// than cached on `App`, so it's never stale across a mode toggle, a nav, or a refresh.
    pub fn outline_items(&self) -> Vec<OutlineItem> {
        let snapshot: Vec<OutlineChangeset> = self
            .changesets
            .iter()
            .map(|v| OutlineChangeset {
                label: v.cs.title.clone().unwrap_or_else(|| v.cs.name.clone()),
                current: v.cs.current,
                needs_restack: v.cs.needs_restack,
                files: v
                    .files()
                    .iter()
                    .enumerate()
                    .map(|(idx, f)| OutlineFile {
                        path: f.path.clone(),
                        status: v.staged_status(idx),
                    })
                    .collect(),
            })
            .collect();
        outline::build_items(&snapshot, self.outline.mode)
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

    /// The outline pane's column width — `workon.review.outline.width` (CS7), or
    /// [`DEFAULT_OUTLINE_WIDTH`] if never set. Read by `render.rs` in place of the old fixed
    /// const.
    pub fn outline_width(&self) -> u16 {
        self.outline.width
    }

    pub fn outline_mode(&self) -> OutlineMode {
        self.outline.mode
    }

    /// `o`: a three-state cycle — closed -> open+focused -> open+unfocused (focus back on the
    /// diff, pane stays visible) -> closed. Opening always grabs focus (per the locked design);
    /// the middle -> closed transition ("o while the outline is open but the diff has focus
    /// closes it") isn't explicitly specified in the plan but is the natural completion of the
    /// cycle, kept simple rather than adding a separate "close" key.
    pub fn toggle_outline(&mut self) {
        if !self.outline.open {
            self.outline.open = true;
            self.outline.focused = true;
            self.sync_outline_to_current();
        } else if self.outline.focused {
            self.outline.focused = false;
        } else {
            self.outline.open = false;
        }
    }

    /// `?`: toggle the help overlay (CS3). A plain flip — the overlay always renders whatever
    /// view currently has keyboard focus (see `render::render_help_overlay`), so there is no
    /// extra state to reposition here, unlike [`Self::toggle_outline`]'s three-state cycle.
    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    /// Return focus to the diff without closing the outline (`Esc` while the outline has focus —
    /// `tui::update` routes it here instead of quitting, per the locked design's "Esc must still
    /// not quit when the outline has focus").
    pub fn outline_unfocus(&mut self) {
        self.outline.focused = false;
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

    /// Move the outline's own cursor by `delta` rows (`j`/`k` while the outline has focus),
    /// clamped into the current row list. Landing on a FILE row jumps the diff there
    /// immediately (outline -> diff, per the locked design); landing on a HEADER row does NOT
    /// jump — only [`Self::outline_confirm`] (`Enter`) jumps from a header, since a header's
    /// "first file" isn't necessarily where a `j`/`k` scan through the stack should keep
    /// stopping the diff. This calls [`Self::switch_changeset`] directly (not `next_file`/
    /// `goto_changeset`), so it does NOT re-trigger [`Self::sync_outline_to_current`] — see that
    /// method's doc comment for why only the DIFF-initiated entry points do.
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
        }
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
        }
    }

    /// Enqueue `op`, drain the queue on the same beat, then act on the outcome: a failure or panic
    /// surfaces on the footer and skips the refresh (the index is now in whatever partial state
    /// the failed op left it in — the user resolves with `r`); a `Completed` drain refreshes,
    /// rebuilding the views + attribution from the new index (locked decision #5).
    ///
    /// Generic over any [`StagingOp`] — a hunk/file op ([`FileStagingOp`]) or a (possibly
    /// multi-hunk) line selection ([`LineSelectionOp`], which applies as ONE merged patch rather
    /// than enqueueing one op per hunk — see that type's docs for why splitting is wrong). Either
    /// way exactly one op is ever in flight, so the queue's trap-4 live-index staleness doesn't
    /// apply — the queue is here for its lock-retry and panic isolation.
    fn run_op(&mut self, op: impl StagingOp + 'static) {
        self.queue.enqueue(op);
        // Distinct fields (`queue` mutable, `repo`/`applier` shared) — the borrow checker permits
        // the disjoint borrows in one call, so the queue needn't be taken out and put back.
        let outcomes = self.queue.drain(&self.repo, &self.applier);
        let failure = outcomes.iter().find_map(|outcome| match outcome {
            OpOutcome::Failed(_, err) => Some(format!("staging failed: {err}")),
            OpOutcome::Panicked(_) => Some("staging operation panicked".to_string()),
            OpOutcome::Completed(_) => None,
        });
        match failure {
            Some(message) => self.notify(message, Severity::Error),
            None => self.coordinated_refresh(),
        }
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

/// [`App::base_label`] for the changeset that would become active — a committed changeset's
/// base rev (7-char short-sha), or `"HEAD"` for the uncommitted layer (worktree ↔ `HEAD`,
/// unchanged from M2–M4).
fn base_label_for(cs: &Changeset) -> String {
    match cs.span {
        ChangesetSpan::Committed { base, .. } => {
            let full = base.to_string();
            full.chars().take(7).collect()
        }
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
        find_next_hunk_row, find_prev_hunk_row, App, ChangesetView, EffectiveZoom, Layout, Role,
        Zoom, DEFAULT_OUTLINE_WIDTH,
    };
    use crate::align::{AlignedRow, CellKind, DisplayRow, InlineRow, Row};
    use crate::config::ReviewConfig;
    use crate::model::FileStatus;
    use crate::outline::{OutlineItem, OutlineMode, StagedStatus};

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
    fn toggle_outline_cycles_closed_open_focused_open_unfocused_closed() {
        let mut app = two_committed_changesets_two_and_one_files();
        // Force a known starting state regardless of the default.
        while app.outline_open() {
            app.toggle_outline();
        }
        assert!(!app.outline_open());

        app.toggle_outline();
        assert!(
            app.outline_open() && app.outline_focused(),
            "opening focuses"
        );

        app.toggle_outline();
        assert!(
            app.outline_open() && !app.outline_focused(),
            "toggling while focused returns focus to the diff without closing"
        );

        app.toggle_outline();
        assert!(
            !app.outline_open(),
            "toggling again while open-but-unfocused closes the pane"
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

        let items = app.outline_items();
        assert_eq!(
            items[0],
            OutlineItem::Header {
                cs_idx: 0,
                label: "cs-a".to_string(),
                current: false,
                needs_restack: false,
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
            }
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

    #[test]
    fn outline_move_by_on_a_file_row_jumps_the_diff() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Flat;
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
        app.outline.cursor = 0; // cs-a's header row
        let cs_before = app.current_cs();
        let file_before = app.current;

        // Header rows sit at indices 0 (cs-a) and 3 (cs-b) in Stack mode (header, a1, a2,
        // header). Move onto the cs-b header without landing on a file row in between.
        app.outline_move_by(3);
        assert_eq!(
            (app.current_cs(), app.current),
            (cs_before, file_before),
            "landing the outline cursor on a header row must not move the diff"
        );
    }

    #[test]
    fn outline_confirm_on_a_header_row_jumps_to_its_first_file_and_returns_focus() {
        let mut app = two_committed_changesets_two_and_one_files();
        app.outline.mode = OutlineMode::Stack;
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
        // Default state is open+unfocused (locked design), so a single `o` here hits the
        // "open, diff has focus" branch of the cycle, which closes the pane.
        assert!(app.outline_open() && !app.outline_focused());
        app.toggle_outline();
        assert!(!app.outline_open());
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
}
