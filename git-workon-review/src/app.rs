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

use crate::acquire::WorktreeDiffs;
use crate::align::{align_file, collapse_gaps, inline_rows, CellKind, DisplayRow, InlineRow, Row};
use crate::highlight::{FgSpan, TsHighlighter};
use crate::model::{DiffModel, FileChange, FileStatus};
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
}

impl FileView {
    /// `role` decides where each side's text comes from — the hunks (which rows are changes) are
    /// already role-correct because `file` is that role's own [`FileChange`], but the surrounding
    /// text must match the same two revisions the hunks were diffed against, or context lines
    /// render one revision on one side and a different one on the other:
    /// - old side: [`Role::Combined`]/[`Role::Staged`] read the `HEAD` blob; [`Role::Unstaged`]
    ///   reads the INDEX blob (unstaged is index ↔ worktree).
    /// - new side: [`Role::Combined`]/[`Role::Unstaged`] read the worktree file;
    ///   [`Role::Staged`] reads the INDEX blob (staged is `HEAD` ↔ index).
    fn load(
        repo: &Repository,
        head_tree: &git2::Tree<'_>,
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
                Role::Combined | Role::Unstaged => read_workdir_file(repo, &file.path),
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

/// Review session state: the combined diff's file list, per-file lazily loaded views, and
/// navigation/scroll state. One long-lived [`TsHighlighter`] lives here (not per file) — its
/// language-config cache is keyed per-instance, so a fresh highlighter per file would rebuild
/// every grammar config on every navigation.
pub struct App {
    repo: Repository,
    /// The combined diff's files. git2 enumerates these in path order (verified in
    /// `tests`), so "current file index" is a stable alphabetical position, not an
    /// arrival/discovery order that could reshuffle under the user. The file LIST stays
    /// combined-driven even in split/zoom modes — only the rendered rows change per role.
    pub files: Vec<FileChange>,
    /// The unstaged (index ↔ worktree) sub-diff, and, parallel to [`Self::files`],
    /// [`Self::unstaged_idx`] mapping each combined file to its index here (or `None` when that
    /// file has no unstaged change). The staged pair mirrors it.
    unstaged_model: DiffModel,
    staged_model: DiffModel,
    unstaged_idx: Vec<Option<usize>>,
    staged_idx: Vec<Option<usize>>,
    /// Per-file, per-role lazily built views (parallel to [`Self::files`]). A slot stays `None`
    /// until first access; a role slot ALSO stays `None` forever when that file has no change in
    /// that role (see [`Self::ensure_role_loaded`]).
    views_combined: Vec<Option<FileView>>,
    views_unstaged: Vec<Option<FileView>>,
    views_staged: Vec<Option<FileView>>,
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
}

impl App {
    pub fn new(repo: Repository, diffs: WorktreeDiffs) -> Self {
        let WorktreeDiffs {
            staged,
            unstaged,
            combined,
        } = diffs;
        let files = combined.files;
        let n = files.len();
        let unstaged_idx = files
            .iter()
            .map(|f| find_role_change(&unstaged, f))
            .collect();
        let staged_idx = files.iter().map(|f| find_role_change(&staged, f)).collect();
        Self {
            repo,
            files,
            unstaged_model: unstaged,
            staged_model: staged,
            unstaged_idx,
            staged_idx,
            views_combined: (0..n).map(|_| None).collect(),
            views_unstaged: (0..n).map(|_| None).collect(),
            views_staged: (0..n).map(|_| None).collect(),
            current: 0,
            cursor: 0,
            scroll: 0,
            pane_height: 20,
            alt: PaneState::default(),
            alt_height: 20,
            base_label: "HEAD".to_string(),
            highlighter: TsHighlighter::new(),
            layout: Layout::default(),
            zoom: Zoom::default(),
            split_focus: SplitPane::Unstaged,
        }
    }

    /// Resolve the [`EffectiveZoom`] for file `idx` this frame: the requested [`Self::zoom`] gated
    /// against that file's available sub-diffs and stageability. Cheap (three lookups + the pure
    /// [`effective_zoom`]) — re-evaluated per file per frame, no caching (locked decision #3).
    pub(crate) fn effective_zoom_for(&self, idx: usize) -> EffectiveZoom {
        let can_stage = self.files.get(idx).map(|f| !f.is_binary).unwrap_or(false);
        let has_unstaged = self.unstaged_idx.get(idx).copied().flatten().is_some();
        let has_staged = self.staged_idx.get(idx).copied().flatten().is_some();
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
        match role {
            Role::Combined => &self.views_combined,
            Role::Unstaged => &self.views_unstaged,
            Role::Staged => &self.views_staged,
        }
    }

    fn views_for_mut(&mut self, role: Role) -> &mut [Option<FileView>] {
        match role {
            Role::Combined => &mut self.views_combined,
            Role::Unstaged => &mut self.views_unstaged,
            Role::Staged => &mut self.views_staged,
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
                let Some(file) = self.files.get(idx) else {
                    return;
                };
                if file.is_binary {
                    return;
                }
                if self.views_combined.get(idx).map(Option::is_some) != Some(false) {
                    return;
                }
                None
            }
            Role::Unstaged => self.unstaged_idx.get(idx).copied().flatten(),
            Role::Staged => self.staged_idx.get(idx).copied().flatten(),
        };

        if role != Role::Combined {
            let Some(mi) = model_idx else {
                return; // no change in this role for this file
            };
            if self.views_for(role).get(idx).map(Option::is_some) != Some(false) {
                return; // already loaded (or slot absent)
            }
            let file = match role {
                Role::Unstaged => &self.unstaged_model.files[mi],
                Role::Staged => &self.staged_model.files[mi],
                Role::Combined => unreachable!(),
            };
            if file.is_binary {
                return;
            }
            // Build the view in a block so `head_tree` (which borrows `self.repo`) drops before
            // the `views_for_mut` reborrow — same reason the combined path below can assign a
            // direct field while `head_tree` is live but this method-call path cannot.
            let view = {
                // Re-peeled per call, same rationale as the combined path below.
                let Ok(head_tree) = self.repo.head().and_then(|h| h.peel_to_tree()) else {
                    return;
                };
                FileView::load(&self.repo, &head_tree, file, role, &mut self.highlighter)
            };
            self.views_for_mut(role)[idx] = Some(view);
            return;
        }

        // Combined role.
        // Re-peeled per call rather than cached on `App`: HEAD can move between file loads and the
        // tree is cheap to re-peel.
        let Ok(head_tree) = self.repo.head().and_then(|h| h.peel_to_tree()) else {
            return;
        };
        let view = FileView::load(
            &self.repo,
            &head_tree,
            &self.files[idx],
            Role::Combined,
            &mut self.highlighter,
        );
        self.views_combined[idx] = Some(view);
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
        self.zoom = match self.zoom {
            Zoom::Split => Zoom::Combined,
            Zoom::Combined => Zoom::Unstaged,
            Zoom::Unstaged => Zoom::Staged,
            Zoom::Staged => Zoom::Split,
        };
        self.open_current();
    }

    /// Swap focus between the two split panes (`w`) — swaps `cursor`/`scroll`/`pane_height` with
    /// the stashed unfocused pane so the existing cursor methods keep driving the focused pane, and
    /// re-derives the newly focused pane's scroll against its own (just-swapped-in) height. A no-op
    /// outside a split.
    pub fn toggle_split_focus(&mut self) {
        if self.effective_zoom_for(self.current) != EffectiveZoom::Split {
            return;
        }
        std::mem::swap(&mut self.cursor, &mut self.alt.cursor);
        std::mem::swap(&mut self.scroll, &mut self.alt.scroll);
        std::mem::swap(&mut self.pane_height, &mut self.alt_height);
        self.split_focus = match self.split_focus {
            SplitPane::Unstaged => SplitPane::Staged,
            SplitPane::Staged => SplitPane::Unstaged,
        };
        self.derive_scroll();
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
    use git_workon_fixture::prelude::*;

    use super::test_support::app_from_fixture;
    use super::{find_next_hunk_row, find_prev_hunk_row};
    use crate::align::{AlignedRow, CellKind, DisplayRow, InlineRow, Row};
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
        assert!(app.files.is_empty(), "fixture must have no dirty files");

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
}
