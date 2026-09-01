//! The outline side pane's pure item model: given a snapshot of every reviewed changeset (label,
//! current/needs-restack flags, and per-file staged-ness), build the flat row list the pane
//! renders and the outline cursor indexes — no [`crate::app::App`]/[`crate::app::ChangesetView`]
//! dependency, same posture as [`crate::align`]'s pure row-alignment module consumed by
//! `app`/`render`.
//!
//! The outline side pane (flat and stack modes) shipped two of the four modes
//! ([`OutlineMode::Flat`]/[`OutlineMode::Stack`]); the outline's path-trie tree modes added
//! the two path-trie modes ([`OutlineMode::Tree`]/[`OutlineMode::StackTree`]) via the private
//! [`TrieNode`] builder below. File-status letters and opt-in nerd icons adds each file row's
//! [`crate::model::FileStatus`] (the `M`/
//! `A`/`D`/... change-status letter — see [`OutlineFile::change`]/[`OutlineItem::File::change`]'s
//! doc comments for why that's a wholly separate field from [`StagedStatus`], which tracks
//! index/worktree staged-ness, not the underlying change kind). Pulling in
//! `crate::model::FileStatus` keeps this module's pure-data posture intact: `model.rs` is itself
//! a pure data module (no `App`/`ChangesetView` dependency), so importing its plain enum doesn't
//! reintroduce the `App` coupling this module was factored out to avoid.
//!
//! Outline collapse/expand (fold) (`outline-fold`) also adds a second stage layered on top of
//! [`build_items`]: collapse/expand. [`build_items`] itself stays wholly unaware of fold state
//! (its extensive mode/dedup/guide tests below are untouched by it) — [`apply_fold`] takes its output and a per-row
//! collapsed predicate and returns the filtered row list plus the two extra pieces of data render/
//! cursor logic needs (a collapsed row's hidden-file count, and a full-list -> filtered-list index
//! map for re-finding a fold-hidden target). [`fold_outline`] is the two steps composed —
//! `App::outline_items`'s single entry point (see that method's doc comment for why every
//! cursor/staging/render consumer funnels through the SAME filtered list).

use std::collections::HashMap;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::model::FileStatus;

/// Which of the outline's row-building strategies is active — cycled by `i` (only while the
/// outline pane has focus; see `App::outline_cycle_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutlineMode {
    /// Every changed path across the whole stack, once each, no changeset headers.
    Flat,
    /// A changeset header row per changeset, followed by that changeset's file rows — the
    /// default (locked choice for the outline side pane (flat and stack modes): this is the mode
    /// that actually shows the stack structure the stack-and-outline work exists to surface).
    #[default]
    Stack,
    /// [`Self::Flat`]'s de-duped path set, rendered as a directory trie (dir rows + file leaves)
    /// instead of a bare list.
    Tree,
    /// [`Self::Stack`]'s per-changeset grouping, but each changeset's files are rendered as their
    /// own nested trie instead of a bare list.
    StackTree,
}

impl OutlineMode {
    /// `i`'s cycle order: `Stack -> StackTree -> Flat -> Tree -> Stack` (`outline-mode-cycle`) — the default
    /// [`Self::Stack`] leads, its trie sibling [`Self::StackTree`] follows immediately, then the
    /// non-grouped pair [`Self::Flat`]/[`Self::Tree`] closes the loop.
    pub fn cycle(self) -> Self {
        match self {
            OutlineMode::Stack => OutlineMode::StackTree,
            OutlineMode::StackTree => OutlineMode::Flat,
            OutlineMode::Flat => OutlineMode::Tree,
            OutlineMode::Tree => OutlineMode::Stack,
        }
    }

    /// The kebab-cased display name (`outline-mode-cycle`) — used by the footer's `i
    /// →<next>` hint and mirrors `OUTLINE_MODE_OPTIONS`'s config strings (`app.rs`), so the
    /// two never drift apart.
    pub fn label(self) -> &'static str {
        match self {
            OutlineMode::Stack => "stack",
            OutlineMode::StackTree => "stack-tree",
            OutlineMode::Flat => "flat",
            OutlineMode::Tree => "tree",
        }
    }
}

/// Which end of the stack the outline's stack-shaped modes ([`OutlineMode::Stack`]/
/// [`OutlineMode::StackTree`]) display first — outline-side-pane (flat and stack modes)
/// dogfooding feedback #2. Purely a display
/// order: [`OutlineItem`]'s `cs_idx`/`file_idx` always stay TRUE indices into `App::changesets`
/// regardless of which way the rows are painted (see [`build_items`]'s doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutlineOrder {
    /// The most recently created (head) changeset's header renders first — the outline-side-pane
    /// (flat and stack modes) default.
    #[default]
    HeadFirst,
    /// The stack's base changeset renders first, matching `App::changesets`' own base -> head
    /// storage order (today's pre-outline-side-pane behavior).
    BaseFirst,
}

/// Enumerate `changesets` in `order`'s display scan — the shared preamble of every stack-shaped
/// builder below. Indices are always TRUE base -> head indices into the slice regardless of scan
/// direction (enumerate happens BEFORE any reversal), which is the invariant `cs_idx`/`file_idx`
/// consumers like `App::switch_changeset` rely on.
fn scan_order(
    changesets: &[OutlineChangeset],
    order: OutlineOrder,
) -> Vec<(usize, &OutlineChangeset)> {
    let mut entries: Vec<(usize, &OutlineChangeset)> = changesets.iter().enumerate().collect();
    if order == OutlineOrder::HeadFirst {
        entries.reverse();
    }
    entries
}

/// A file's staged-ness for the outline's status column — the data model `render.rs` derives its
/// git-porcelain-style X/Y two-column status matrix from (`outline-status-xy`). Only
/// meaningful for the uncommitted changeset's files; a committed changeset's files always
/// resolve to `None` because their `unstaged_idx`/`staged_idx` maps are always-empty (see
/// `DiffState::from_committed`) — the same "derive, don't special-case" collapse
/// `effective_zoom` already relies on, so no committed-specific branch is needed here either.
/// `render::build_outline_line`'s File arm reads `None` as "render a committed single letter +
/// pad column" and `Unstaged`/`Staged`/`Partial` as "render the X/Y matrix" — see that fn's doc
/// comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StagedStatus {
    /// No staged/unstaged sub-diff info for this file (a committed changeset's file, or an
    /// uncommitted file that — impossibly — has a whole-role change but neither sub-change).
    #[default]
    None,
    /// The file has an unstaged (index ↔ worktree) change but no staged one.
    Unstaged,
    /// The file has a staged (`HEAD` ↔ index) change but no unstaged one.
    Staged,
    /// The file has both — partially staged.
    Partial,
}

impl StagedStatus {
    /// Resolve from the two membership flags `App`/`ChangesetView` already compute (does this
    /// file have an entry in the unstaged/staged sub-`DiffModel`) — the same two booleans
    /// [`crate::app::effective_zoom`] gates on.
    pub fn from_flags(has_unstaged: bool, has_staged: bool) -> Self {
        match (has_unstaged, has_staged) {
            (true, true) => StagedStatus::Partial,
            (true, false) => StagedStatus::Unstaged,
            (false, true) => StagedStatus::Staged,
            (false, false) => StagedStatus::None,
        }
    }
}

/// One file's outline-relevant data, as extracted from its owning changeset by
/// `App::outline_items` — the input [`build_items`] consumes.
#[derive(Debug, Clone)]
pub struct OutlineFile {
    pub path: String,
    /// Index/worktree staged-ness — [`StagedStatus::None`] for a committed changeset's files.
    /// NOT the same axis as [`Self::change`]: a file can be `Staged` (this field) while its
    /// underlying change is `Deleted` (that one) — they answer different questions ("is it
    /// staged" vs. "what kind of change is it") and must stay two distinct fields.
    pub status: StagedStatus,
    /// File-status letters and opt-in nerd icons: the underlying change kind
    /// (Modified/Added/Deleted/...), lifted from the owning
    /// [`crate::model::FileChange::status`] — drives the outline's `M`/`A`/`D`/`R`/`C`/`?`/`U`
    /// letter (`render::build_outline_line`), independent of [`Self::status`] above.
    pub change: FileStatus,
}

/// One changeset's outline-relevant data — a snapshot, not a borrow, so this module never needs
/// to know about [`crate::app::ChangesetView`] or `workon::Changeset` at all.
#[derive(Debug, Clone)]
pub struct OutlineChangeset {
    /// The changeset's display label (`crate::app::display_label` — title falling back to name,
    /// with the uncommitted layer rendered as "Uncommitted changes"), the same rule the winbar
    /// and summary panel use.
    pub label: String,
    /// Mirrors `workon::Changeset::current` — drives the outline's green current marker.
    pub current: bool,
    /// Mirrors `workon::Changeset::needs_restack` — drives the outline's amber warning glyph.
    pub needs_restack: bool,
    /// ADR-037: the changeset's diff hasn't been acquired yet — the header shows a loading
    /// indication in place of the (currently absent, since `files` is empty for a `Pending`
    /// slot) file rows.
    pub loading: bool,
    /// ADR-037: the acquisition attempt for this changeset errored — the header marks it.
    pub failed: bool,
    pub files: Vec<OutlineFile>,
}

/// One row the outline pane renders and the outline cursor can land on. `cs_idx` is always the
/// index into `App`'s changeset list the row belongs to; `file_idx` (on [`Self::File`]) is the
/// index into THAT changeset's file list — together they're exactly what
/// `App::switch_changeset`/`App::goto_changeset` need to jump the diff there.
///
/// `guides` (on [`Self::Dir`]/[`Self::File`]) is the tree-guide vector the outline's
/// path-trie tree modes adds: one bool per
/// nesting level from the shallowest ancestor down to the row itself, `true` meaning "this
/// level is its parent's last child". Rendering uses every-element-but-the-last to decide
/// whether to draw a continuing `│` or blank space at that column, and the last element to draw
/// `╰─`/`├─` for the row's own connector (the outline's path-trie tree modes rounds the
/// last-child corner). [`OutlineMode::Flat`]/[`OutlineMode::Stack`] rows carry
/// an EMPTY `guides` — that's the signal to `render::build_outline_line` to fall back to the
/// flat two-space indent instead of drawing tree connectors; a non-empty `guides` of length 1
/// means "top-level tree row" (depth 0), so emptiness and depth-0 are deliberately distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineItem {
    /// A changeset header — emitted in [`OutlineMode::Stack`]/[`OutlineMode::StackTree`].
    Header {
        cs_idx: usize,
        /// Changeset count (`outline-header-polish`) — paired with `cs_idx` at render time
        /// to draw the `[i/n]` counter (`i` = `cs_idx + 1`, base=1). Always `changesets.len()` at
        /// build time, so it's the same for every `Header` row a given `build_items` call emits.
        n: usize,
        label: String,
        current: bool,
        needs_restack: bool,
        /// ADR-037: this changeset hasn't been diffed yet — rendered as a loading indication.
        loading: bool,
        /// ADR-037: this changeset's acquisition attempt errored — rendered as a marker.
        failed: bool,
    },
    /// A directory row — only emitted in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`]. Not a
    /// jump target: it carries no `file_idx`, so `App::outline_move_by` no-ops on it (same as
    /// [`Self::Header`]); `App::outline_confirm` toggles this row's fold state instead of jumping
    /// (`outline-fold`) and deliberately does NOT return focus to the diff — see that
    /// method's doc comment. Fold state itself lives on `App` (per-[`OutlineMode`] sets keyed by
    /// [`FoldKey`]), not here — this row stays a plain data snapshot either way.
    Dir {
        name: String,
        /// The FULL path from the trie root (e.g. `"src/cmd"`), unlike `name` which is just the
        /// leaf segment — the summary panel needs the whole path to filter files under this
        /// directory (see `crate::summary::dir_summary`).
        path: String,
        /// `Some(cs_idx)` when this row's trie is per-changeset ([`OutlineMode::StackTree`] —
        /// the same true index its owning [`Self::Header`] carries); `None` in the cross-stack
        /// [`OutlineMode::Tree`], whose single trie spans every changeset (so a dir row there has
        /// no single owning changeset to scope a summary to — the summary panel's
        /// `App::summary_for` instead
        /// aggregates over [`latest_by_path`]'s de-duped set for that case).
        cs_idx: Option<usize>,
        guides: Vec<bool>,
    },
    /// A file row — the target of every outline->diff jump. `path` is the FULL path in
    /// [`OutlineMode::Flat`]/[`OutlineMode::Stack`] (unchanged from the outline side pane (flat and stack modes)), but is just the leaf
    /// segment in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`] — the ancestor directory rows
    /// already carry the rest of the path, so re-printing it on every leaf would be redundant.
    File {
        cs_idx: usize,
        file_idx: usize,
        path: String,
        status: StagedStatus,
        /// File-status letters and opt-in nerd icons: the change kind (Modified/Added/Deleted/...)
        /// — see [`OutlineFile::change`]'s doc
        /// comment on why this is distinct from `status` above.
        change: FileStatus,
        guides: Vec<bool>,
    },
}

impl OutlineItem {
    /// Nesting depth for indentation math: `0` for [`Self::Header`] and any flat/stack row
    /// (empty `guides`), else `guides.len() - 1`.
    pub fn depth(&self) -> usize {
        match self {
            OutlineItem::Header { .. } => 0,
            OutlineItem::Dir { guides, .. } | OutlineItem::File { guides, .. } => {
                guides.len().saturating_sub(1)
            }
        }
    }
}

/// Build the outline's row list for `mode` from every reviewed changeset. `order` controls which
/// end of the stack displays first for the stack-shaped modes (see [`OutlineOrder`]); `cs_idx`/
/// `file_idx` on every emitted [`OutlineItem`] are always TRUE indices into `App::changesets`
/// (that array's own base -> head storage order never changes) regardless of `order` — only the
/// ROW SEQUENCE the outline paints flips. [`build_tree`]'s de-dupe is order-independent (see its
/// own doc comment), so `order` is accepted but unused there.
///
/// `pub(crate)` (`outline-fold`): this is the "unfiltered build" [`fold_outline`]'s doc comment refers to —
/// every outside-the-module consumer (i.e. `App`) goes through `fold_outline`/`apply_fold`
/// instead, so a fold is never accidentally bypassed by calling this directly.
pub(crate) fn build_items(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
) -> Vec<OutlineItem> {
    build_items_inner(changesets, mode, order, None)
}

/// [`build_items`] with the outline fuzzy filter's per-row inclusion gate (`filter`) layered on — the REVISED
/// 2026-07-24 "rebuild from the surviving file set" entry point [`fold_outline_filtered`] calls.
/// Deliberately walks the SAME, full, unpruned `changesets` slice `build_items` does (see
/// [`is_included`]'s doc comment) — every `cs_idx`/`file_idx` this emits is therefore still
/// computed the exact same positional way `build_items` always has, so the true-index invariant
/// holds for free rather than needing a new index-carrying field on [`OutlineChangeset`]/
/// [`OutlineFile`].
fn build_items_filtered(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
    filter: &QueryMatches,
) -> Vec<OutlineItem> {
    build_items_inner(changesets, mode, order, Some(filter))
}

fn build_items_inner(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
    filter: Option<&QueryMatches>,
) -> Vec<OutlineItem> {
    match mode {
        OutlineMode::Flat => build_flat(changesets, order, filter),
        OutlineMode::Stack => build_stack(changesets, order, filter),
        OutlineMode::Tree => build_tree(changesets, filter),
        OutlineMode::StackTree => build_stack_tree(changesets, order, filter),
    }
}

// ── Fold (collapse/expand), `outline-fold` ─────────────────────────────────────

/// A foldable outline row's identity — the key `App`'s per-[`OutlineMode`] fold sets store.
/// [`OutlineItem::Header`] is keyed by its changeset's label PLUS its `cs_idx`; [`OutlineItem::Dir`]
/// by its full path plus, in [`OutlineMode::StackTree`], its owning changeset's `cs_idx` (`None`
/// in [`OutlineMode::Tree`], mirroring [`OutlineItem::Dir::cs_idx`]'s own `Option` — that mode's
/// single trie has no one owning changeset to key by).
///
/// `cs_idx` is load-bearing here, not just belt-and-suspenders: a changeset's own `label` is NOT
/// guaranteed unique across a single snapshot in general (e.g. two changesets could otherwise
/// share a title), so keying by label alone would risk folding unrelated rows together the
/// moment either was toggled. `cs_idx` is still the true index into `App::changesets` (stable
/// across an ordinary refresh — only a structural stack change, e.g. a changeset added/removed,
/// shifts it), matching the same "identity survives refresh via `cs_idx`" precedent
/// [`crate::app::OutlineRowIdentity`] already relies on for staging-verb restore.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldKey {
    Header { label: String, cs_idx: usize },
    Dir { path: String, owner: Option<usize> },
}

impl FoldKey {
    /// `item`'s [`FoldKey`], or `None` for a [`OutlineItem::File`] row (never foldable — it
    /// carries no fold state of its own). Reads only fields the item already carries on itself
    /// (`cs_idx`, `label`/`path`) — no external lookup needed. `pub(crate)`: also
    /// `App::outline_toggle_fold`'s way of turning "the row under the cursor" into the key its
    /// fold set is keyed by, without duplicating this match.
    pub(crate) fn for_item(item: &OutlineItem) -> Option<Self> {
        match item {
            OutlineItem::Header { cs_idx, label, .. } => Some(FoldKey::Header {
                label: label.clone(),
                cs_idx: *cs_idx,
            }),
            OutlineItem::Dir { path, cs_idx, .. } => Some(FoldKey::Dir {
                path: path.clone(),
                owner: *cs_idx,
            }),
            OutlineItem::File { .. } => None,
        }
    }
}

/// The outline's row list after `outline-fold`'s fold filtering is layered on top of
/// [`build_items`]'s raw
/// build — see [`apply_fold`]/[`fold_outline`]'s doc comments for how it's derived, and
/// `App::outline_items`'s doc comment for why this is the SINGLE choke point every cursor/
/// staging/render consumer reads through.
#[derive(Debug, Clone)]
pub(crate) struct FoldedOutline {
    /// The visible rows, in order — a subsequence of [`build_items`]'s full (unfiltered) output.
    pub items: Vec<OutlineItem>,
    /// Parallel to `items`: the count of hidden FILE rows (not dirs — `outline-fold`'s locked "N = hidden
    /// FILE rows only" rule) under a collapsed Header/Dir row. `0` for every other row, including
    /// an EXPANDED Header/Dir — render reads `0` as "no marker", so an expanded row never draws
    /// the trailing ` ▸ N` chevron.
    pub hidden_counts: Vec<usize>,
    /// Parallel to the FULL (unfiltered) [`build_items`] output, NOT to `items`: for original row
    /// `i`, the index into `items`/`hidden_counts` a cursor targeting that row should land on —
    /// its own filtered position if it survived filtering, or its nearest VISIBLE ancestor's if a
    /// fold hides it (`outline-fold`'s "lands on the collapsed ancestor without auto-expanding" rule). Used
    /// by `App::sync_outline_to_current` to re-target a diff-initiated jump onto a folded row's
    /// row instead of leaving the outline cursor on an arbitrary clamp.
    pub visible_index: Vec<usize>,
}

/// Filter `items` (a fresh [`build_items`] call's output) down to the rows `is_folded`'s per-mode
/// fold set leaves visible, computing each collapsed row's hidden-file marker and the full-list ->
/// filtered-list index map described on [`FoldedOutline::visible_index`]. Needs no `changesets`
/// snapshot of its own — [`FoldKey::for_item`] reads only what each item already carries on
/// itself (see that fn's doc comment on why `cs_idx`, not a label lookup, is what disambiguates).
///
/// One linear pass with an explicit stack of "open ancestor" frames, mirroring [`emit`]'s own
/// depth-first row order: a [`OutlineItem::Header`] frame's scope is "everything up to the next
/// Header" (depth `-1`, a sentinel shallower than every real tree depth); a [`OutlineItem::Dir`]
/// frame's scope is "everything with a deeper tree `guides` prefix than its own" (its
/// [`OutlineItem::depth`]). Both close the same way: popping frames whose recorded depth is `>=`
/// the current row's depth, since a shallower-or-equal row can't be that frame's descendant. A row
/// hidden by ANY currently-open ancestor being folded is dropped from the output entirely, but a
/// hidden File row still bumps every open ancestor's running hidden-file count (even an unfolded
/// one — that count is simply never read unless the frame turns out to be folded when it's
/// popped), so a doubly-nested fold's OUTER marker still counts files hidden two levels down.
pub(crate) fn apply_fold(
    items: &[OutlineItem],
    is_folded: impl Fn(&FoldKey) -> bool,
) -> FoldedOutline {
    struct Frame {
        depth: isize,
        folded: bool,
        hidden: usize,
        /// Index into the output `items`/`hidden_counts` this frame's OWN row landed at — `None`
        /// if the frame's own row was itself hidden by a still-further-out fold (a doubly-nested
        /// collapse), in which case it never got a marker to write into.
        out_idx: Option<usize>,
    }

    /// Write a popped frame's final hidden-file count into its own row's marker slot — only if
    /// the frame is folded (an expanded frame's count is dead data, never read) and was itself
    /// visible (`out_idx: Some`; a hidden frame has no marker slot to write into at all).
    fn finalize(frame: Frame, hidden_counts: &mut [usize]) {
        if frame.folded {
            if let Some(idx) = frame.out_idx {
                hidden_counts[idx] = frame.hidden;
            }
        }
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut out_items: Vec<OutlineItem> = Vec::new();
    let mut hidden_counts: Vec<usize> = Vec::new();
    let mut visible_index: Vec<usize> = Vec::with_capacity(items.len());

    for item in items {
        let depth: isize = match item {
            OutlineItem::Header { .. } => -1,
            OutlineItem::Dir { .. } | OutlineItem::File { .. } => item.depth() as isize,
        };
        while stack.last().is_some_and(|f| f.depth >= depth) {
            finalize(
                stack.pop().expect("just checked the stack is non-empty"),
                &mut hidden_counts,
            );
        }

        let hidden = stack.iter().any(|f| f.folded);
        if hidden && matches!(item, OutlineItem::File { .. }) {
            for f in &mut stack {
                f.hidden += 1;
            }
        }

        let out_idx =
            if hidden {
                stack.iter().rev().find_map(|f| f.out_idx).expect(
                    "row 0 of any build is always visible, so some open ancestor must be too",
                )
            } else {
                let idx = out_items.len();
                out_items.push(item.clone());
                hidden_counts.push(0);
                idx
            };
        visible_index.push(out_idx);

        if let OutlineItem::Header { .. } | OutlineItem::Dir { .. } = item {
            let key = FoldKey::for_item(item)
                .expect("just matched Header/Dir, both of which always resolve a FoldKey");
            stack.push(Frame {
                depth,
                folded: is_folded(&key),
                hidden: 0,
                out_idx: if hidden { None } else { Some(out_idx) },
            });
        }
    }
    while let Some(frame) = stack.pop() {
        finalize(frame, &mut hidden_counts);
    }

    FoldedOutline {
        items: out_items,
        hidden_counts,
        visible_index,
    }
}

/// [`build_items`] + [`apply_fold`] composed — `App::outline_items`'s (and its private
/// `App::outline_folded` helper's) single entry point, so `app.rs` never has to import both
/// functions and remember to always pair them.
pub(crate) fn fold_outline(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
    is_folded: impl Fn(&FoldKey) -> bool,
) -> FoldedOutline {
    let items = build_items(changesets, mode, order);
    apply_fold(&items, is_folded)
}

// ── Fuzzy filter (`outline-filter`, REVISED 2026-07-24: filter-then-rebuild) ───────

/// One row's fuzzy-match result against the SOURCE text it was scored on (a changeset's title, or
/// a file's FULL repo-relative path — never a dir segment or a tree leaf; REVISED 2026-07-24 drops
/// those as independent match targets). `score` is only meaningful compared against another
/// [`FilterMatch`] from the SAME query. `indices` are CHAR indices into that source text, not yet
/// remapped onto whatever text the eventual row displays — [`attach_filter_marks`] does that.
/// `matched_len` is that source text's own char count, which the tree-mode leaf remap needs to
/// compute the offset into a row that only displays the path's trailing segment.
#[derive(Debug, Clone)]
struct FilterMatch {
    score: i64,
    indices: Vec<usize>,
    matched_len: usize,
}

/// [`score_changesets`]'s output: every changeset/file's own [`FilterMatch`] (if it has one),
/// keyed by TRUE `cs_idx`/`file_idx` — never by array position, since the rebuild step below still
/// walks the FULL, unpruned `changesets` slice (see [`is_included`]'s doc comment for why nothing
/// here ever needs its own `cs_idx` field to stay correct). `matched_cs` is the "does this
/// changeset survive AT ALL" set (title match OR at least one file match) the stack-shaped
/// builders gate their header emission on.
struct QueryMatches {
    header_matches: HashMap<usize, FilterMatch>,
    file_matches: HashMap<(usize, usize), FilterMatch>,
    matched_cs: std::collections::HashSet<usize>,
}

/// Score every changeset in `changesets` against `query` at the SOURCE, per REVISED 2026-07-24's
/// "match at the source, not the built rows" rule — in two tiers:
///
/// 1. **Files first.** Score each file's FULL path individually; every match is recorded under
///    `file_matches` and the changeset enters `matched_cs`.
/// 2. **Titles only as a fallback**, when NO file anywhere in the snapshot matched. A title match
///    then keeps the WHOLE changeset (every file, unscored) via `header_matches`.
///
/// The fallback tier exists because titles are prose: a fuzzy subsequence like `"an"` matches
/// "Uncommitted ch·an·ges" (and half the titles in a real stack), so letting a title match
/// compete with file matches would routinely pull entire changesets into a query meant to narrow
/// to one file. Demoting titles to the no-file-results case keeps both intents predictable:
/// file-ish queries always narrow to files; a query that matches nothing BUT a title (typing a
/// changeset's name) still surfaces that changeset with all its files.
///
/// A changeset with neither tier's match never enters `matched_cs`, which is what causes
/// [`build_stack`]/[`build_stack_tree`] to drop its header (and, transitively,
/// [`build_flat`]/[`build_tree`] to drop every one of its files) entirely.
fn score_changesets(changesets: &[OutlineChangeset], query: &str) -> QueryMatches {
    let matcher = SkimMatcherV2::default();
    let mut out = QueryMatches {
        header_matches: HashMap::new(),
        file_matches: HashMap::new(),
        matched_cs: std::collections::HashSet::new(),
    };
    for (cs_idx, cs) in changesets.iter().enumerate() {
        for (file_idx, file) in cs.files.iter().enumerate() {
            if let Some((score, indices)) = matcher.fuzzy_indices(&file.path, query) {
                out.file_matches.insert(
                    (cs_idx, file_idx),
                    FilterMatch {
                        score,
                        indices,
                        matched_len: file.path.chars().count(),
                    },
                );
                out.matched_cs.insert(cs_idx);
            }
        }
    }
    if out.file_matches.is_empty() {
        for (cs_idx, cs) in changesets.iter().enumerate() {
            if let Some((score, indices)) = matcher.fuzzy_indices(&cs.label, query) {
                out.header_matches.insert(
                    cs_idx,
                    FilterMatch {
                        score,
                        indices,
                        matched_len: cs.label.chars().count(),
                    },
                );
                out.matched_cs.insert(cs_idx);
            }
        }
    }
    out
}

/// Whether `(cs_idx, file_idx)` survives filtering: unconditionally `true` when `filter` is
/// `None` (the ordinary, unfiltered build every pre-outline-filter test exercises), else `true` when either
/// the file's OWN path matched, or its changeset's TITLE matched (a title match "keeps the WHOLE
/// changeset, all files" — see [`score_changesets`]'s doc comment).
fn is_included(filter: Option<&QueryMatches>, cs_idx: usize, file_idx: usize) -> bool {
    match filter {
        None => true,
        Some(f) => {
            f.header_matches.contains_key(&cs_idx)
                || f.file_matches.contains_key(&(cs_idx, file_idx))
        }
    }
}

/// [`fold_outline_filtered`]'s per-row output, parallel to its [`FoldedOutline::items`]: each
/// row's fuzzy-match char indices REMAPPED onto whatever text that row itself displays (empty if
/// the row isn't itself a match — an ancestor `Dir` kept only because a descendant survived, or a
/// `Header` kept only because a file survived), and each row's own score (`None` for the same
/// "not itself a match" rows) — [`Self::best_index`] is [`App::outline_filter_reflow`]'s
/// cursor-park source.
#[derive(Debug, Clone, Default)]
pub(crate) struct FilterMarks {
    pub match_indices: Vec<Vec<usize>>,
    pub scores: Vec<Option<i64>>,
}

impl FilterMarks {
    fn empty_for(len: usize) -> Self {
        FilterMarks {
            match_indices: vec![Vec::new(); len],
            scores: vec![None; len],
        }
    }

    /// The FIRST row (by rendered position) carrying the HIGHEST score, or `None` if no row in
    /// this build has a score at all (no filter active, or — impossible in practice, since a
    /// changeset only ever survives via its own or a file's match — every survivor is an
    /// unscored ancestor). Ties keep the earlier row: the fold only replaces the running best on
    /// a STRICTLY greater score.
    pub(crate) fn best_index(&self) -> Option<usize> {
        self.scores
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|score| (i, score)))
            .fold(None, |best: Option<(usize, i64)>, (i, score)| match best {
                Some((_, best_score)) if best_score >= score => best,
                _ => Some((i, score)),
            })
            .map(|(i, _)| i)
    }
}

/// [`fold_outline_filtered`]'s final step: remap [`score_changesets`]'s SOURCE-text match indices
/// onto whatever text each rebuilt+folded row actually displays, and carry each row's own score
/// alongside.
///
/// - [`OutlineItem::Header`]: the row's own `label` IS the text that was scored (a title match),
///   so its indices need no remap.
/// - [`OutlineItem::File`]: `file_matches` indices address the FULL path that was scored. In
///   [`OutlineMode::Flat`]/[`OutlineMode::Stack`] (empty `guides`) the row's own `path` field IS
///   that full path — no remap. In [`OutlineMode::Tree`]/[`OutlineMode::StackTree`] (non-empty
///   `guides`) the row displays only the LEAF segment (the ancestor [`OutlineItem::Dir`] rows
///   already carry the rest — see [`OutlineItem::File`]'s own doc comment). A trie leaf is always
///   the full path's own TRAILING segment, so shifting every index left by `matched_len -
///   leaf_len` lands it in the leaf's own char range; an index that shifts negative addressed a
///   character in an ancestor directory segment this row doesn't render, so it's dropped.
/// - [`OutlineItem::Dir`]: deliberately left UNHIGHLIGHTED. REVISED 2026-07-24 drops dir rows as
///   independent match targets, and a surviving dir can have several children whose match spans
///   disagree — picking one arbitrarily (or unioning spans from unrelated files) would misrepresent
///   what actually matched. Leaving dir rows plain is the simple, honest choice; render still
///   dims them via the existing tree-guide styling, so they read as quiet structure either way.
fn attach_filter_marks(
    items: &[OutlineItem],
    header_matches: &HashMap<usize, FilterMatch>,
    file_matches: &HashMap<(usize, usize), FilterMatch>,
) -> FilterMarks {
    let mut marks = FilterMarks::empty_for(items.len());
    for (i, item) in items.iter().enumerate() {
        match item {
            OutlineItem::Header { cs_idx, .. } => {
                if let Some(m) = header_matches.get(cs_idx) {
                    marks.match_indices[i] = m.indices.clone();
                    marks.scores[i] = Some(m.score);
                }
            }
            OutlineItem::File {
                cs_idx,
                file_idx,
                path,
                guides,
                ..
            } => {
                if let Some(m) = file_matches.get(&(*cs_idx, *file_idx)) {
                    marks.scores[i] = Some(m.score);
                    marks.match_indices[i] = if guides.is_empty() {
                        m.indices.clone()
                    } else {
                        let leaf_len = path.chars().count();
                        let offset = m.matched_len.saturating_sub(leaf_len);
                        m.indices
                            .iter()
                            .filter_map(|&idx| idx.checked_sub(offset))
                            .filter(|&shifted| shifted < leaf_len)
                            .collect()
                    };
                }
            }
            OutlineItem::Dir { .. } => {}
        }
    }
    marks
}

/// [`score_changesets`] + rebuild ([`build_items_filtered`]) + [`apply_fold`] +
/// [`attach_filter_marks`] composed — `App::outline_filtered`'s single entry point (mirrors how
/// [`fold_outline`] composes the fold-only case). `query.is_empty()` short-circuits straight to a
/// plain [`fold_outline`] call with every mark empty/`None` — the "zero regression when the
/// filter is unused" rule, now enforced HERE rather than duplicated by every caller.
///
/// REVISED 2026-07-24's "rebuild, don't post-filter" rule: the surviving changesets/files are fed
/// back through the SAME [`build_items`]/[`apply_fold`] machinery every other build uses (via
/// [`build_items_filtered`]'s inclusion gate), so headers, dir rows, tree guides, fold behavior,
/// and hidden-count markers all come out structurally correct — no flattening, no guide reset.
/// Ordering is therefore the outline's ordinary structural order, never score-descending; the
/// score is used ONLY to pick [`App::outline_filter_reflow`]'s cursor-park target via
/// [`FilterMarks::best_index`].
pub(crate) fn fold_outline_filtered(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
    is_folded: impl Fn(&FoldKey) -> bool,
    query: &str,
) -> (FoldedOutline, FilterMarks) {
    if query.is_empty() {
        let folded = fold_outline(changesets, mode, order, is_folded);
        let marks = FilterMarks::empty_for(folded.items.len());
        return (folded, marks);
    }
    let matches = score_changesets(changesets, query);
    let items = build_items_filtered(changesets, mode, order, &matches);
    let folded = apply_fold(&items, is_folded);
    let marks = attach_filter_marks(
        &folded.items,
        &matches.header_matches,
        &matches.file_matches,
    );
    (folded, marks)
}

/// [`OutlineMode::Stack`]: a header per changeset, then its files in order — no de-duplication,
/// every changeset's own copy of a path (if touched more than once across the stack) gets its
/// own row under its own header. `order` picks which end of the stack paints first; `cs_idx`/
/// `file_idx` are computed from the ORIGINAL (base -> head) enumeration before any reversal, so
/// they stay true indices into `App::changesets` either way.
///
/// `filter` (REVISED 2026-07-24): `None` builds every row, exactly as before this changeset —
/// `Some` skips a changeset's header (and every one of its files) entirely when it isn't in
/// `matched_cs`, and skips an individual surviving changeset's own non-matching files via
/// [`is_included`]. Never renumbers: `cs_idx`/`file_idx` are still read straight off the SAME
/// positional scan, so a filtered build's indices are exactly as true as an unfiltered one's.
fn build_stack(
    changesets: &[OutlineChangeset],
    order: OutlineOrder,
    filter: Option<&QueryMatches>,
) -> Vec<OutlineItem> {
    let n = changesets.len();
    let mut items = Vec::new();
    for (cs_idx, cs) in scan_order(changesets, order) {
        if filter.is_some_and(|f| !f.matched_cs.contains(&cs_idx)) {
            continue;
        }
        items.push(OutlineItem::Header {
            cs_idx,
            n,
            label: cs.label.clone(),
            current: cs.current,
            needs_restack: cs.needs_restack,
            loading: cs.loading,
            failed: cs.failed,
        });
        for (file_idx, file) in cs.files.iter().enumerate() {
            if !is_included(filter, cs_idx, file_idx) {
                continue;
            }
            items.push(OutlineItem::File {
                cs_idx,
                file_idx,
                path: file.path.clone(),
                status: file.status,
                change: file.change,
                guides: Vec::new(),
            });
        }
    }
    items
}

/// [`OutlineMode::Flat`]: every changed path once, in FIRST-appearance order UNDER `order`'s
/// display scan (a stable, readable order that doesn't reshuffle just because a later-scanned
/// changeset re-touches an earlier path), but pointing at its closest-to-head occurrence —
/// "last-write-wins" per the locked design: a path touched by both an earlier committed
/// changeset and the uncommitted layer should jump to (and show the staged-ness of) the
/// uncommitted layer's copy, not the stale committed one. This head-wins target resolution is
/// independent of `order` — [`latest_by_path`] always scans base -> head regardless of which way
/// the row list is displayed, so the resolution below reuses it rather than re-deriving from the
/// (possibly reversed) `order` scan used for display order.
///
/// `filter` (REVISED 2026-07-24): the de-dupe target resolution (which occurrence a shared path
/// jumps to) is untouched by filtering — it's derived from the FULL, unpruned `changesets`, same
/// as always. Only the FINAL emit step changes: a path is dropped when its resolved (closest-to-
/// head) occurrence itself doesn't survive [`is_included`] — even if some OLDER, non-displayed
/// occurrence of the same path would have matched, since Flat mode never shows that older copy
/// anyway (unfiltered or not).
fn build_flat(
    changesets: &[OutlineChangeset],
    order: OutlineOrder,
    filter: Option<&QueryMatches>,
) -> Vec<OutlineItem> {
    let latest = latest_by_path(changesets);
    let mut order_list: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (_, cs) in scan_order(changesets, order) {
        for file in &cs.files {
            if seen.insert(file.path.as_str()) {
                order_list.push(file.path.clone());
            }
        }
    }
    order_list
        .into_iter()
        .filter_map(|path| {
            let occ = latest[&path];
            if !is_included(filter, occ.cs_idx, occ.file_idx) {
                return None;
            }
            Some(OutlineItem::File {
                cs_idx: occ.cs_idx,
                file_idx: occ.file_idx,
                path,
                status: occ.status,
                change: occ.change,
                guides: Vec::new(),
            })
        })
        .collect()
}

/// De-dupe every changed path across the stack to its LAST occurrence (mirrors
/// [`build_flat`]'s last-write-wins rule), independent of iteration/insertion order — the trie
/// builders below re-sort by path segment anyway, so no stable-order bookkeeping is needed here.
///
/// `pub(crate)`: the summary panel's `App::summary_for` reuses this directly to aggregate a
/// [`OutlineMode::Tree`] directory's files (`cs_idx: None` on that mode's [`OutlineItem::Dir`]
/// rows) over the same last-write-wins de-duped set the Tree outline itself displays, rather than
/// re-deriving the dedup logic in `app.rs`.
pub(crate) fn latest_by_path(changesets: &[OutlineChangeset]) -> HashMap<String, FileOccurrence> {
    let mut latest = HashMap::new();
    for (cs_idx, cs) in changesets.iter().enumerate() {
        for (file_idx, file) in cs.files.iter().enumerate() {
            latest.insert(
                file.path.clone(),
                FileOccurrence {
                    cs_idx,
                    file_idx,
                    status: file.status,
                    change: file.change,
                },
            );
        }
    }
    latest
}

/// The file a de-duped path (or a trie leaf) resolves to: its true `(cs_idx, file_idx)` address
/// into `App::changesets` plus the two per-file status axes the outline renders — staged-ness
/// ([`StagedStatus`], `outline-status-xy`) and change kind ([`FileStatus`], file-status
/// letters and opt-in nerd icons). Named because the bare 4-tuple
/// it replaced had to be re-explained (and type-annotated) at every use site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileOccurrence {
    pub cs_idx: usize,
    pub file_idx: usize,
    pub status: StagedStatus,
    pub change: FileStatus,
}

/// A node in the path trie the tree modes build. A node with `file.is_some()` is a leaf (a
/// changed file at that exact path); otherwise it's a pure directory node. Git paths never
/// collide a file and a directory at the same path, so a node is never both.
#[derive(Debug, Default)]
struct TrieNode {
    file: Option<FileOccurrence>,
    /// Insertion order is irrelevant — [`emit`] re-sorts children (dirs-before-files, alpha
    /// within group) every time it flattens a node.
    children: Vec<(String, TrieNode)>,
}

impl TrieNode {
    fn insert(&mut self, segments: &[&str], occ: FileOccurrence) {
        let (head, rest) = segments
            .split_first()
            .expect("insert is never called with an empty segment list");
        let idx = match self.children.iter().position(|(name, _)| name == head) {
            Some(idx) => idx,
            None => {
                self.children.push((head.to_string(), TrieNode::default()));
                self.children.len() - 1
            }
        };
        let child = &mut self.children[idx].1;
        if rest.is_empty() {
            // Last-write-wins: a later insert of the same full path overwrites the leaf data.
            child.file = Some(occ);
        } else {
            child.insert(rest, occ);
        }
    }
}

/// Flatten `node`'s children into `items`, depth-first, in "dirs before files at each level,
/// alpha within group" order (the outline side pane (flat and stack modes) dogfooding
/// feedback #7: directories read before files at a
/// given level, matching the conventional file-tree convention of grouping folders above
/// loose files). `ancestors_last` is the growing guide vector — see [`OutlineItem`]'s doc
/// comment for how rendering consumes it.
fn emit(
    node: &TrieNode,
    ancestors_last: &[bool],
    path_prefix: &str,
    dir_cs_idx: Option<usize>,
    items: &mut Vec<OutlineItem>,
) {
    let mut files: Vec<&(String, TrieNode)> = node
        .children
        .iter()
        .filter(|(_, n)| n.file.is_some())
        .collect();
    let mut dirs: Vec<&(String, TrieNode)> = node
        .children
        .iter()
        .filter(|(_, n)| n.file.is_none())
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    let ordered: Vec<&(String, TrieNode)> = dirs.into_iter().chain(files).collect();
    let n = ordered.len();
    for (i, (name, child)) in ordered.into_iter().enumerate() {
        let is_last = i == n - 1;
        let mut guides = ancestors_last.to_vec();
        guides.push(is_last);
        match child.file {
            Some(occ) => {
                items.push(OutlineItem::File {
                    cs_idx: occ.cs_idx,
                    file_idx: occ.file_idx,
                    path: name.clone(),
                    status: occ.status,
                    change: occ.change,
                    guides,
                });
            }
            None => {
                let full_path = if path_prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{path_prefix}/{name}")
                };
                items.push(OutlineItem::Dir {
                    name: name.clone(),
                    path: full_path.clone(),
                    cs_idx: dir_cs_idx,
                    guides: guides.clone(),
                });
                emit(child, &guides, &full_path, dir_cs_idx, items);
            }
        }
    }
}

/// [`OutlineMode::Tree`]: [`build_flat`]'s de-duped path set, rendered as a single directory
/// trie spanning the whole stack (no changeset headers). Alpha-sorted by path segment at every
/// level ([`emit`]), not by stack position, and [`latest_by_path`]'s de-dupe always resolves to
/// the closest-to-head occurrence regardless of scan order — so [`OutlineOrder`] has nothing to
/// affect here, and unlike the stack-shaped builders this one takes no `order` parameter.
///
/// `filter` (REVISED 2026-07-24): same de-dupe-then-gate story as [`build_flat`] — an excluded
/// occurrence's path is simply never inserted into the trie, so its ancestor `Dir` rows vanish
/// too whenever it was their only surviving child (a dir with zero inserted descendants never
/// gets emitted at all — see [`emit`]).
fn build_tree(changesets: &[OutlineChangeset], filter: Option<&QueryMatches>) -> Vec<OutlineItem> {
    let latest = latest_by_path(changesets);
    let mut root = TrieNode::default();
    for (path, occ) in &latest {
        if !is_included(filter, occ.cs_idx, occ.file_idx) {
            continue;
        }
        let segments: Vec<&str> = path.split('/').collect();
        root.insert(&segments, *occ);
    }
    let mut items = Vec::new();
    emit(&root, &[], "", None, &mut items);
    items
}

/// [`OutlineMode::StackTree`]: [`build_stack`]'s per-changeset header grouping, but each
/// changeset's own files are flattened into their own nested trie (no cross-changeset dedup —
/// each changeset trie is built from just that changeset's files, matching `build_stack`'s "every
/// changeset's own copy gets its own row" rule). `order` picks which end of the stack paints
/// first, same as [`build_stack`]; `cs_idx`/`file_idx` stay true indices regardless.
///
/// `filter` (REVISED 2026-07-24): same header-skip gate as [`build_stack`], plus the same
/// never-insert-an-excluded-file gate [`build_tree`] uses for its own per-changeset trie.
fn build_stack_tree(
    changesets: &[OutlineChangeset],
    order: OutlineOrder,
    filter: Option<&QueryMatches>,
) -> Vec<OutlineItem> {
    let n = changesets.len();
    let mut items = Vec::new();
    for (cs_idx, cs) in scan_order(changesets, order) {
        if filter.is_some_and(|f| !f.matched_cs.contains(&cs_idx)) {
            continue;
        }
        items.push(OutlineItem::Header {
            cs_idx,
            n,
            label: cs.label.clone(),
            current: cs.current,
            needs_restack: cs.needs_restack,
            loading: cs.loading,
            failed: cs.failed,
        });
        let mut root = TrieNode::default();
        for (file_idx, file) in cs.files.iter().enumerate() {
            if !is_included(filter, cs_idx, file_idx) {
                continue;
            }
            let segments: Vec<&str> = file.path.split('/').collect();
            root.insert(
                &segments,
                FileOccurrence {
                    cs_idx,
                    file_idx,
                    status: file.status,
                    change: file.change,
                },
            );
        }
        emit(&root, &[], "", Some(cs_idx), &mut items);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `change` defaults to [`FileStatus::Modified`] for every file — the ordinary case, and
    /// irrelevant to the order/dedup/depth semantics these tests exercise. Tests that care about
    /// a SPECIFIC change status (dedup target resolution) use [`cs_with_change`] instead.
    fn cs(
        label: &str,
        current: bool,
        needs_restack: bool,
        files: &[(&str, StagedStatus)],
    ) -> OutlineChangeset {
        cs_with_change(
            label,
            current,
            needs_restack,
            &files
                .iter()
                .map(|(p, s)| (*p, *s, FileStatus::Modified))
                .collect::<Vec<_>>(),
        )
    }

    /// [`cs`] variant that lets a test pin each file's [`FileStatus`] explicitly (file-status
    /// letters and opt-in nerd icons).
    fn cs_with_change(
        label: &str,
        current: bool,
        needs_restack: bool,
        files: &[(&str, StagedStatus, FileStatus)],
    ) -> OutlineChangeset {
        OutlineChangeset {
            label: label.to_string(),
            current,
            needs_restack,
            loading: false,
            failed: false,
            files: files
                .iter()
                .map(|(p, s, c)| OutlineFile {
                    path: p.to_string(),
                    status: *s,
                    change: *c,
                })
                .collect(),
        }
    }

    /// [`cs`] variant for ADR-037's slot tests — builds a `Pending`/`Failed` outline changeset
    /// (no files, since a non-`Ready` [`crate::app::ChangesetView`] never has any).
    fn cs_slot(label: &str, loading: bool, failed: bool) -> OutlineChangeset {
        OutlineChangeset {
            label: label.to_string(),
            current: false,
            needs_restack: false,
            loading,
            failed,
            files: Vec::new(),
        }
    }

    #[test]
    fn stack_mode_emits_a_header_before_each_changesets_files() {
        let changesets = vec![
            cs("cs-a", false, false, &[("a1.txt", StagedStatus::None)]),
            cs("cs-b", true, true, &[("b1.txt", StagedStatus::None)]),
        ];
        // BaseFirst pins the base -> head structural rule (header-then-files per changeset)
        // independent of display order; head-first order coverage lives in the dedicated
        // `stack_mode_*_order` tests below.
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        assert_eq!(
            items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 2,
                    label: "cs-a".to_string(),
                    current: false,
                    needs_restack: false,
                    loading: false,
                    failed: false,
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "a1.txt".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: Vec::new(),
                },
                OutlineItem::Header {
                    cs_idx: 1,
                    n: 2,
                    label: "cs-b".to_string(),
                    current: true,
                    needs_restack: true,
                    loading: false,
                    failed: false,
                },
                OutlineItem::File {
                    cs_idx: 1,
                    file_idx: 0,
                    path: "b1.txt".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: Vec::new(),
                },
            ]
        );
    }

    /// ADR-037: a `Pending`/`Failed` changeset (no files) still emits a Stack-mode header row,
    /// carrying the loading/failed marker instead of any file rows.
    #[test]
    fn stack_mode_marks_pending_and_failed_headers_with_no_file_rows() {
        let changesets = vec![
            cs_slot("cs-pending", true, false),
            cs_slot("cs-failed", false, true),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        assert_eq!(
            items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 2,
                    label: "cs-pending".to_string(),
                    current: false,
                    needs_restack: false,
                    loading: true,
                    failed: false,
                },
                OutlineItem::Header {
                    cs_idx: 1,
                    n: 2,
                    label: "cs-failed".to_string(),
                    current: false,
                    needs_restack: false,
                    loading: false,
                    failed: true,
                },
            ],
            "a Pending/Failed changeset carries no file rows (its files list is empty), only its \
             own marked header"
        );
    }

    #[test]
    fn flat_mode_has_no_headers() {
        let changesets = vec![cs(
            "cs-a",
            true,
            false,
            &[
                ("a1.txt", StagedStatus::None),
                ("a2.txt", StagedStatus::None),
            ],
        )];
        let items = build_items(&changesets, OutlineMode::Flat, OutlineOrder::HeadFirst);
        assert!(items
            .iter()
            .all(|it| matches!(it, OutlineItem::File { .. })));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn flat_mode_dedupes_a_shared_path_to_the_newest_changesets_occurrence() {
        let changesets = vec![
            cs("cs-a", false, false, &[("shared.txt", StagedStatus::None)]),
            cs(
                "cs-b",
                true,
                false,
                &[("shared.txt", StagedStatus::Unstaged)],
            ),
        ];
        let items = build_items(&changesets, OutlineMode::Flat, OutlineOrder::HeadFirst);
        assert_eq!(items.len(), 1, "the shared path must appear exactly once");
        assert_eq!(
            items[0],
            OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "shared.txt".to_string(),
                status: StagedStatus::Unstaged,
                change: FileStatus::Modified,
                guides: Vec::new(),
            },
            "must point at cs-b (the LATER/newer changeset), not cs-a"
        );
    }

    #[test]
    fn flat_mode_preserves_first_appearance_order_despite_last_write_wins_target() {
        let changesets = vec![
            cs(
                "cs-a",
                false,
                false,
                &[
                    ("first.txt", StagedStatus::None),
                    ("shared.txt", StagedStatus::None),
                ],
            ),
            cs("cs-b", true, false, &[("shared.txt", StagedStatus::Staged)]),
        ];
        // BaseFirst scans cs-a before cs-b, so "first appearance" here means base -> head scan
        // order; the head-first display-order variant lives in
        // `flat_mode_head_first_scans_head_to_base_but_keeps_head_wins_target` below.
        let items = build_items(&changesets, OutlineMode::Flat, OutlineOrder::BaseFirst);
        let paths: Vec<&str> = items
            .iter()
            .map(|it| match it {
                OutlineItem::File { path, .. } => path.as_str(),
                OutlineItem::Header { .. } | OutlineItem::Dir { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            paths,
            vec!["first.txt", "shared.txt"],
            "display order follows first appearance, not the retargeted changeset"
        );
    }

    /// The outline side pane (flat and stack modes): [`OutlineOrder::HeadFirst`] flips
    /// [`build_flat`]'s DISPLAY scan (first-appearance
    /// order now reads head -> base), but [`latest_by_path`]'s "closest-to-head wins" TARGET
    /// resolution never changes — a path touched by two changesets must resolve to the head-most
    /// one under BOTH orders.
    #[test]
    fn flat_mode_head_first_scans_head_to_base_but_keeps_head_wins_target() {
        let changesets = vec![
            cs(
                "cs-a",
                false,
                false,
                &[
                    ("first.txt", StagedStatus::None),
                    ("shared.txt", StagedStatus::None),
                ],
            ),
            cs("cs-b", true, false, &[("shared.txt", StagedStatus::Staged)]),
        ];
        let items = build_items(&changesets, OutlineMode::Flat, OutlineOrder::HeadFirst);
        let paths: Vec<&str> = items
            .iter()
            .map(|it| match it {
                OutlineItem::File { path, .. } => path.as_str(),
                OutlineItem::Header { .. } | OutlineItem::Dir { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            paths,
            vec!["shared.txt", "first.txt"],
            "head-first display scans cs-b (head) before cs-a (base), so shared.txt is seen \
             first"
        );
        assert_eq!(
            items
                .iter()
                .find(|it| matches!(it, OutlineItem::File { path, .. } if path == "shared.txt")),
            Some(&OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "shared.txt".to_string(),
                status: StagedStatus::Staged,
                change: FileStatus::Modified,
                guides: Vec::new(),
            }),
            "target resolution stays head-wins (cs-b) regardless of display order"
        );
    }

    #[test]
    fn staged_status_from_flags_covers_the_truth_table() {
        assert_eq!(StagedStatus::from_flags(false, false), StagedStatus::None);
        assert_eq!(
            StagedStatus::from_flags(true, false),
            StagedStatus::Unstaged
        );
        assert_eq!(StagedStatus::from_flags(false, true), StagedStatus::Staged);
        assert_eq!(StagedStatus::from_flags(true, true), StagedStatus::Partial);
    }

    #[test]
    fn mode_cycle_round_trips_all_four_modes() {
        assert_eq!(OutlineMode::Stack.cycle(), OutlineMode::StackTree);
        assert_eq!(OutlineMode::StackTree.cycle(), OutlineMode::Flat);
        assert_eq!(OutlineMode::Flat.cycle(), OutlineMode::Tree);
        assert_eq!(OutlineMode::Tree.cycle(), OutlineMode::Stack);
    }

    /// Deep-path fixture used by the tree-mode tests: a top-level file, a top-level directory
    /// with both its own file and a nested subdirectory of two more files — enough to exercise
    /// depth > 1 and the dirs-before-files/alpha-within-group ordering at every level.
    fn deep_path_changeset(label: &str, current: bool, needs_restack: bool) -> OutlineChangeset {
        cs(
            label,
            current,
            needs_restack,
            &[
                ("top.rs", StagedStatus::None),
                ("src/a/b.rs", StagedStatus::None),
                ("src/a/c.rs", StagedStatus::None),
                ("src/d.rs", StagedStatus::None),
            ],
        )
    }

    #[test]
    fn tree_mode_builds_dirs_before_files_alpha_within_group_with_correct_depth_and_guides() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let items = build_items(&changesets, OutlineMode::Tree, OutlineOrder::HeadFirst);
        assert_eq!(
            items,
            vec![
                OutlineItem::Dir {
                    name: "src".to_string(),
                    path: "src".to_string(),
                    cs_idx: None,
                    guides: vec![false],
                },
                OutlineItem::Dir {
                    name: "a".to_string(),
                    path: "src/a".to_string(),
                    cs_idx: None,
                    guides: vec![false, false],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 1,
                    path: "b.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![false, false, false],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 2,
                    path: "c.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![false, false, true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 3,
                    path: "d.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![false, true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "top.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![true],
                },
            ],
            "root: src/ (dir) then top.rs (file); under src/: a/ (dir) then d.rs (file); \
             under src/a/: b.rs then c.rs — dirs-before-files, alpha within each group"
        );
        assert_eq!(items[0].depth(), 0, "src/ is a root-level row");
        assert_eq!(items[1].depth(), 1, "src/a/ is one level deep");
        assert_eq!(items[2].depth(), 2, "src/a/b.rs is two levels deep");
        assert_eq!(items[5].depth(), 0, "top.rs is a root-level row");
    }

    #[test]
    fn tree_mode_dedupes_a_shared_path_to_the_newest_changesets_occurrence() {
        let changesets = vec![
            cs("cs-a", false, false, &[("shared.txt", StagedStatus::None)]),
            cs("cs-b", true, false, &[("shared.txt", StagedStatus::Staged)]),
        ];
        let items = build_items(&changesets, OutlineMode::Tree, OutlineOrder::HeadFirst);
        assert_eq!(
            items,
            vec![OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "shared.txt".to_string(),
                status: StagedStatus::Staged,
                change: FileStatus::Modified,
                guides: vec![true],
            }],
            "the shared path must appear exactly once, pointing at the newer changeset"
        );
    }

    #[test]
    fn stack_tree_mode_nests_each_changesets_files_under_its_own_header() {
        let changesets = vec![
            cs("cs-a", false, false, &[("x/y.txt", StagedStatus::None)]),
            cs("cs-b", true, true, &[("z.txt", StagedStatus::Unstaged)]),
        ];
        let items = build_items(&changesets, OutlineMode::StackTree, OutlineOrder::BaseFirst);
        assert_eq!(
            items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 2,
                    label: "cs-a".to_string(),
                    current: false,
                    needs_restack: false,
                    loading: false,
                    failed: false,
                },
                OutlineItem::Dir {
                    name: "x".to_string(),
                    path: "x".to_string(),
                    cs_idx: Some(0),
                    guides: vec![true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "y.txt".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![true, true],
                },
                OutlineItem::Header {
                    cs_idx: 1,
                    n: 2,
                    label: "cs-b".to_string(),
                    current: true,
                    needs_restack: true,
                    loading: false,
                    failed: false,
                },
                OutlineItem::File {
                    cs_idx: 1,
                    file_idx: 0,
                    path: "z.txt".to_string(),
                    status: StagedStatus::Unstaged,
                    change: FileStatus::Modified,
                    guides: vec![true],
                },
            ],
            "each changeset's files form their own trie nested under that changeset's header, \
             with no cross-changeset dedup"
        );
    }

    /// The outline side pane (flat and stack modes): [`OutlineOrder::HeadFirst`] (the new
    /// default) shows the LAST changeset's ([`cs-c`],
    /// index 2 — the true, base-> head `App::changesets` index) header FIRST, while its `cs_idx`
    /// still equals its true index into `changesets` (2), never a display-order index (0).
    #[test]
    fn stack_mode_head_first_shows_last_changesets_header_first_with_true_cs_idx() {
        let changesets = vec![
            cs("cs-a", false, false, &[("a1.txt", StagedStatus::None)]),
            cs("cs-b", false, false, &[("b1.txt", StagedStatus::None)]),
            cs("cs-c", true, false, &[("c1.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::HeadFirst);
        assert_eq!(
            items[0],
            OutlineItem::Header {
                cs_idx: 2,
                n: 3,
                label: "cs-c".to_string(),
                current: true,
                needs_restack: false,
                loading: false,
                failed: false,
            },
            "head-first: the LAST (head) changeset's header renders first, carrying its TRUE \
             index (2) into `changesets`, not a display-order index"
        );
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|it| match it {
                OutlineItem::Header { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec!["cs-c", "cs-b", "cs-a"],
            "head-first header order is exactly the reverse of `changesets`' base -> head order"
        );
    }

    /// The outline side pane (flat and stack modes): [`OutlineOrder::BaseFirst`] restores the
    /// pre-outline-side-pane base -> head header order.
    #[test]
    fn stack_mode_base_first_restores_base_to_head_header_order() {
        let changesets = vec![
            cs("cs-a", false, false, &[("a1.txt", StagedStatus::None)]),
            cs("cs-b", true, false, &[("b1.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|it| match it {
                OutlineItem::Header { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["cs-a", "cs-b"]);
    }

    /// [`OutlineMode::StackTree`] analog of
    /// `stack_mode_head_first_shows_last_changesets_header_first_with_true_cs_idx`.
    #[test]
    fn stack_tree_mode_head_first_shows_last_changesets_header_first_with_true_cs_idx() {
        let changesets = vec![
            cs("cs-a", false, false, &[("x/y.txt", StagedStatus::None)]),
            cs("cs-b", true, false, &[("z.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::StackTree, OutlineOrder::HeadFirst);
        assert_eq!(
            items[0],
            OutlineItem::Header {
                cs_idx: 1,
                n: 2,
                label: "cs-b".to_string(),
                current: true,
                needs_restack: false,
                loading: false,
                failed: false,
            },
            "head-first: cs-b's header renders first, carrying its true index (1)"
        );
        assert_eq!(
            items[1],
            OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "z.txt".to_string(),
                status: StagedStatus::None,
                change: FileStatus::Modified,
                guides: vec![true],
            },
            "cs-b's own file follows immediately under its head-first header"
        );
    }

    // ── Fold (collapse/expand), `outline-fold` ─────────────────────────────────

    #[test]
    fn apply_fold_with_nothing_folded_leaves_every_row_visible_with_zero_markers() {
        let changesets = vec![
            cs("cs-a", false, false, &[("a1.txt", StagedStatus::None)]),
            cs("cs-b", true, true, &[("b1.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        let folded = apply_fold(&items, |_| false);
        assert_eq!(
            folded.items, items,
            "nothing folded, so nothing is filtered"
        );
        assert!(
            folded.hidden_counts.iter().all(|&n| n == 0),
            "no collapsed row, so no marker anywhere"
        );
        assert_eq!(
            folded.visible_index,
            (0..items.len()).collect::<Vec<_>>(),
            "every row maps onto its own (only) position"
        );
    }

    #[test]
    fn apply_fold_hides_a_collapsed_headers_files_and_marks_the_hidden_count() {
        let changesets = vec![
            cs(
                "cs-a",
                false,
                false,
                &[
                    ("a1.txt", StagedStatus::None),
                    ("a2.txt", StagedStatus::None),
                ],
            ),
            cs("cs-b", true, false, &[("b1.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        let folded = apply_fold(&items, |key| {
            *key == FoldKey::Header {
                label: "cs-a".to_string(),
                cs_idx: 0,
            }
        });
        assert_eq!(
            folded.items.len(),
            3,
            "cs-a's header survives (its 2 files hidden); cs-b's header AND its own file both \
             survive (cs-b isn't folded)"
        );
        assert!(matches!(
            folded.items[0],
            OutlineItem::Header { ref label, .. } if label == "cs-a"
        ));
        assert_eq!(
            folded.hidden_counts[0], 2,
            "cs-a's collapsed header marks its 2 hidden files"
        );
        assert!(matches!(
            folded.items[1],
            OutlineItem::Header { ref label, .. } if label == "cs-b"
        ));
        assert_eq!(folded.hidden_counts[1], 0, "cs-b is not collapsed");
        assert!(matches!(
            folded.items[2],
            OutlineItem::File { ref path, .. } if path == "b1.txt"
        ));
    }

    #[test]
    fn apply_fold_leaves_a_sibling_headers_files_untouched() {
        let changesets = vec![
            cs("cs-a", false, false, &[("a1.txt", StagedStatus::None)]),
            cs("cs-b", true, false, &[("b1.txt", StagedStatus::None)]),
        ];
        let items = build_items(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst);
        let folded = apply_fold(&items, |key| {
            *key == FoldKey::Header {
                label: "cs-a".to_string(),
                cs_idx: 0,
            }
        });
        let paths: Vec<&str> = folded
            .items
            .iter()
            .filter_map(|it| match it {
                OutlineItem::File { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec!["b1.txt"],
            "cs-b's own file stays visible; only cs-a's collapsed section is hidden"
        );
    }

    #[test]
    fn apply_fold_collapsing_a_dir_hides_its_nested_files_and_subdirs_but_counts_only_files() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let items = build_items(&changesets, OutlineMode::Tree, OutlineOrder::HeadFirst);
        // `src` (depth 0) contains `src/a` (a nested dir, depth 1) and `src/d.rs`, and `src/a`
        // itself contains `src/a/b.rs` + `src/a/c.rs` — collapsing `src` should hide all 3 files
        // (b.rs, c.rs, d.rs) it contains at any depth, but the marker counts files only, not the
        // nested `src/a` dir row itself.
        let folded = apply_fold(&items, |key| {
            *key == FoldKey::Dir {
                path: "src".to_string(),
                owner: None,
            }
        });
        assert_eq!(
            folded.items.len(),
            2,
            "src/ (collapsed) and top.rs (an unrelated sibling) survive"
        );
        let src_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ row survives collapsed");
        assert_eq!(
            folded.hidden_counts[src_idx], 3,
            "b.rs, c.rs, and d.rs are all hidden under collapsed src/ — src/a/ itself doesn't count"
        );
    }

    #[test]
    fn apply_fold_doubly_nested_collapse_still_counts_toward_the_outer_markers_hidden_files() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let items = build_items(&changesets, OutlineMode::Tree, OutlineOrder::HeadFirst);
        // Collapse BOTH `src` and its nested `src/a` — `src/a`'s own row is hidden (nested inside
        // the already-collapsed `src`), but its 2 files must still count toward `src`'s own
        // marker, even though `src/a`'s marker is never written (it has no visible row to write
        // into).
        let folded = apply_fold(&items, |key| {
            matches!(
                key,
                FoldKey::Dir { path, owner: None } if path == "src" || path == "src/a"
            )
        });
        assert_eq!(
            folded.items.len(),
            2,
            "only src/ (collapsed) and top.rs survive; src/a/ is hidden under src/'s own fold"
        );
        let src_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ row survives collapsed");
        assert_eq!(
            folded.hidden_counts[src_idx], 3,
            "src/'s marker still counts all 3 descendant files, even the 2 nested two levels down \
             under the also-collapsed (and therefore invisible) src/a/"
        );
    }

    #[test]
    fn apply_fold_visible_index_maps_a_hidden_files_full_list_position_to_its_visible_ancestor() {
        // `deep_path_changeset`'s full (unfolded) Tree-mode row order is exactly:
        // [src/ (0), src/a/ (1), src/a/b.rs (2), src/a/c.rs (3), src/d.rs (4), top.rs (5)] — see
        // `tree_mode_builds_dirs_before_files_alpha_within_group_with_correct_depth_and_guides`
        // above, which pins this same order. Collapsing `src/` hides everything at indices 1..=4
        // (all nested under it, regardless of their own depth); index 5 (`top.rs`) is a sibling,
        // untouched.
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let items = build_items(&changesets, OutlineMode::Tree, OutlineOrder::HeadFirst);
        let folded = apply_fold(&items, |key| {
            *key == FoldKey::Dir {
                path: "src".to_string(),
                owner: None,
            }
        });
        let src_visible_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { name, .. } if name == "src"))
            .expect("src/ survives collapsed");

        assert_eq!(
            folded.visible_index[0], src_visible_idx,
            "src/'s own row maps onto itself"
        );
        for (full_idx, item) in items.iter().enumerate().take(5).skip(1) {
            assert_eq!(
                folded.visible_index[full_idx], src_visible_idx,
                "row {full_idx} ({item:?}) is hidden under collapsed src/, so it must map onto \
                 src/'s own visible row"
            );
        }
        let top_rs_visible_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::File { path, .. } if path == "top.rs"))
            .expect("top.rs survives, unaffected by src/'s fold");
        assert_eq!(
            folded.visible_index[5], top_rs_visible_idx,
            "top.rs (a sibling of src/, not nested under it) maps onto its own visible row"
        );
    }

    #[test]
    fn fold_outline_composes_build_items_and_apply_fold() {
        let changesets = vec![cs(
            "cs-a",
            true,
            false,
            &[
                ("a1.txt", StagedStatus::None),
                ("a2.txt", StagedStatus::None),
            ],
        )];
        let folded = fold_outline(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::HeadFirst,
            |key| {
                *key == FoldKey::Header {
                    label: "cs-a".to_string(),
                    cs_idx: 0,
                }
            },
        );
        assert_eq!(
            folded.items,
            vec![OutlineItem::Header {
                cs_idx: 0,
                n: 1,
                label: "cs-a".to_string(),
                current: true,
                needs_restack: false,
                loading: false,
                failed: false,
            }],
            "the header survives collapsed; both files are hidden"
        );
        assert_eq!(folded.hidden_counts, vec![2]);
    }

    // ── Fuzzy filter (`outline-filter`, REVISED 2026-07-24: filter-then-rebuild) ───────

    /// `fold_outline_filtered` with an always-visible fold (no key folded) — the shape most of
    /// these tests want; a couple below pass their own predicate to check fold interaction.
    fn filtered(
        changesets: &[OutlineChangeset],
        mode: OutlineMode,
        order: OutlineOrder,
        query: &str,
    ) -> (FoldedOutline, FilterMarks) {
        fold_outline_filtered(changesets, mode, order, |_| false, query)
    }

    #[test]
    fn fold_outline_filtered_drops_non_matches_and_keeps_true_indices_on_survivors() {
        let changesets = vec![cs(
            "cs-a",
            true,
            false,
            &[
                ("src/app.rs", StagedStatus::None),
                ("README.md", StagedStatus::None),
            ],
        )];
        let (folded, _) = filtered(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::BaseFirst,
            "app",
        );
        assert_eq!(
            folded.items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 1,
                    label: "cs-a".to_string(),
                    current: true,
                    needs_restack: false,
                    loading: false,
                    failed: false,
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "src/app.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: Vec::new(),
                },
            ],
            "the header rebuilds structurally (unlike the pre-REVISED flat list), and \
             src/app.rs keeps its TRUE cs_idx/file_idx (0, 0) — README.md (file_idx 1) is \
             dropped, it never matches 'app'"
        );
    }

    #[test]
    fn fold_outline_filtered_preserves_structural_order_not_score_order() {
        // Same two strings/query the pre-REVISED `apply_filter` score-ordering test used to prove
        // "src/x/app_helper.rs" (a long, scattered match) scores LOWER than "app.rs" (an exact,
        // unbroken substring match) — but cs-a (the lower-scoring file's changeset) renders
        // FIRST here, because REVISED 2026-07-24 drops the old score-descending sort entirely in
        // favor of the outline's ordinary base -> head structural order.
        let changesets = vec![
            cs(
                "cs-a",
                false,
                false,
                &[("src/x/app_helper.rs", StagedStatus::None)],
            ),
            cs("cs-b", true, false, &[("app.rs", StagedStatus::None)]),
        ];
        let (folded, marks) = filtered(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::BaseFirst,
            "app.rs",
        );
        let paths: Vec<&str> = folded
            .items
            .iter()
            .filter_map(|it| match it {
                OutlineItem::File { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec!["src/x/app_helper.rs", "app.rs"],
            "cs-a's (lower-scoring) file still renders before cs-b's (higher-scoring) one — \
             base -> head structural order, not score order"
        );
        let scattered_idx = folded
            .items
            .iter()
            .position(
                |it| matches!(it, OutlineItem::File { path, .. } if path == "src/x/app_helper.rs"),
            )
            .unwrap();
        let exact_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::File { path, .. } if path == "app.rs"))
            .unwrap();
        assert!(
            marks.scores[exact_idx].unwrap() > marks.scores[scattered_idx].unwrap(),
            "app.rs's exact match must still score higher than the scattered one, even though \
             it renders SECOND"
        );
    }

    #[test]
    fn fold_outline_filtered_title_match_keeps_the_whole_changesets_files_unscored() {
        let changesets = vec![cs(
            "release-widget",
            true,
            false,
            &[
                ("one.rs", StagedStatus::None),
                ("two.rs", StagedStatus::None),
            ],
        )];
        let (folded, marks) = filtered(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::BaseFirst,
            "release",
        );
        assert_eq!(
            folded.items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 1,
                    label: "release-widget".to_string(),
                    current: true,
                    needs_restack: false,
                    loading: false,
                    failed: false,
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "one.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: Vec::new(),
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 1,
                    path: "two.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: Vec::new(),
                },
            ],
            "a title match on 'release-widget' keeps EVERY file, even though neither one.rs nor \
             two.rs matches 'release' on its own"
        );
        assert!(
            marks.scores[0].is_some(),
            "the header itself is the match, so it carries a score"
        );
        assert_eq!(
            marks.scores[1..].to_vec(),
            vec![None, None],
            "the files were never individually scored — kept only because their changeset's \
             title matched"
        );
    }

    #[test]
    fn a_file_match_anywhere_suppresses_the_title_fallback_tier() {
        // "an" fuzzy-matches the prose title "refactor changes" (subsequence: ch·an·ges) AND the
        // file banana.txt. Titles are a FALLBACK tier only: because a file matched somewhere, the
        // title-matched changeset must NOT survive — otherwise short file queries would pull in
        // whole changesets through their prose titles (the regression: "an" vs the ever-present
        // "Uncommitted changes" label).
        let changesets = vec![
            cs(
                "refactor changes",
                false,
                false,
                &[("zzz.qqq", StagedStatus::None)],
            ),
            cs("other", true, false, &[("banana.txt", StagedStatus::None)]),
        ];
        let (folded, _) = filtered(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::BaseFirst,
            "an",
        );
        assert!(
            folded
                .items
                .iter()
                .all(|it| !matches!(it, OutlineItem::Header { cs_idx: 0, .. })),
            "the title-matched changeset must be suppressed by the file match, got {:?}",
            folded.items
        );
        assert!(
            folded.items.iter().any(
                |it| matches!(it, OutlineItem::File { cs_idx: 1, path, .. } if path == "banana.txt")
            ),
            "banana.txt (the file-tier match) survives with its true indices, got {:?}",
            folded.items
        );
    }

    #[test]
    fn fold_outline_filtered_keeps_dir_ancestors_for_a_deep_tree_match() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        // 'b.rs' matches only src/a/b.rs (none of c.rs/d.rs/top.rs contain a 'b').
        let (folded, marks) = filtered(
            &changesets,
            OutlineMode::Tree,
            OutlineOrder::HeadFirst,
            "b.rs",
        );
        assert_eq!(
            folded.items,
            vec![
                OutlineItem::Dir {
                    name: "src".to_string(),
                    path: "src".to_string(),
                    cs_idx: None,
                    // Every guide is `true` here (unlike the UNFILTERED build's [false]/
                    // [false,false]/[false,false,true]): once c.rs/d.rs/top.rs are filtered out,
                    // src/ and src/a/ each become their PARENT's only (and therefore last)
                    // child, and b.rs becomes src/a/'s only child too.
                    guides: vec![true],
                },
                OutlineItem::Dir {
                    name: "a".to_string(),
                    path: "src/a".to_string(),
                    cs_idx: None,
                    guides: vec![true, true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 1,
                    path: "b.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![true, true, true],
                },
            ],
            "a deep match rebuilds its ancestor src/ and src/a/ Dir rows, with CORRECT (re-derived,\
             not stale) tree guides — no other row (c.rs, d.rs, top.rs) survives"
        );
        assert!(
            marks.match_indices[0].is_empty() && marks.match_indices[1].is_empty(),
            "the ancestor Dir rows are left unhighlighted (REVISED 2026-07-24: dir rows are no \
             longer independent match targets, and picking one child's span to show on a \
             multi-child dir would be misleading — see attach_filter_marks's doc comment)"
        );
        assert_eq!(
            marks.match_indices[2],
            vec![0, 1, 2, 3],
            "'b.rs' matches the leaf's own full text; since the leaf IS the whole matched \
             suffix here, the remap is a no-op shift of 0"
        );
    }

    #[test]
    fn fold_outline_filtered_remaps_a_tree_leafs_match_indices_off_the_ancestor_prefix() {
        // 'a/b' scores against the FULL path "src/a/b.rs", matching the 'a', '/', 'b' run that
        // straddles the src/a/ ancestor prefix and the b.rs leaf's own first char — only the
        // leaf-local portion should survive the remap onto the rendered leaf text "b.rs".
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let (folded, marks) = filtered(
            &changesets,
            OutlineMode::Tree,
            OutlineOrder::HeadFirst,
            "a/b",
        );
        let leaf_idx = folded
            .items
            .iter()
            .position(|it| matches!(it, OutlineItem::File { path, .. } if path == "b.rs"))
            .expect("b.rs survives the 'a/b' query");
        // "src/a/b.rs": indices of 'a' (4), '/' (5), 'b' (6) — the leaf "b.rs" starts at char 6
        // (full length 10, leaf length 4, offset 6). Only the 'b' at index 6 shifts into the
        // leaf's own [0, 4) range (shifted to 0); 'a' and the preceding '/' shift negative and
        // are dropped.
        assert_eq!(
            marks.match_indices[leaf_idx],
            vec![0],
            "only the leaf-local 'b' survives the remap; the ancestor-segment 'a' and '/' \
             matches are dropped, not misrendered onto the wrong chars of 'b.rs'"
        );
    }

    #[test]
    fn fold_outline_filtered_stack_tree_mode_keeps_dir_ancestors_too() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let (folded, _) = filtered(
            &changesets,
            OutlineMode::StackTree,
            OutlineOrder::HeadFirst,
            "b.rs",
        );
        assert_eq!(
            folded.items,
            vec![
                OutlineItem::Header {
                    cs_idx: 0,
                    n: 1,
                    label: "cs-a".to_string(),
                    current: true,
                    needs_restack: false,
                    loading: false,
                    failed: false,
                },
                OutlineItem::Dir {
                    name: "src".to_string(),
                    path: "src".to_string(),
                    cs_idx: Some(0),
                    guides: vec![true],
                },
                OutlineItem::Dir {
                    name: "a".to_string(),
                    path: "src/a".to_string(),
                    cs_idx: Some(0),
                    guides: vec![true, true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 1,
                    path: "b.rs".to_string(),
                    status: StagedStatus::None,
                    change: FileStatus::Modified,
                    guides: vec![true, true, true],
                },
            ],
            "StackTree mode also rebuilds a deep match's ancestor Dir rows under its header, \
             with correct guides"
        );
    }

    #[test]
    fn fold_outline_filtered_empty_query_is_a_zero_regression_no_op() {
        let changesets = vec![cs(
            "cs-a",
            true,
            false,
            &[
                ("a1.txt", StagedStatus::None),
                ("a2.txt", StagedStatus::None),
            ],
        )];
        let plain = fold_outline(
            &changesets,
            OutlineMode::Stack,
            OutlineOrder::BaseFirst,
            |_| false,
        );
        let (folded, marks) =
            filtered(&changesets, OutlineMode::Stack, OutlineOrder::BaseFirst, "");
        assert_eq!(
            folded.items, plain.items,
            "an empty query must reproduce the plain fold_outline build exactly"
        );
        assert_eq!(folded.hidden_counts, plain.hidden_counts);
        assert_eq!(folded.visible_index, plain.visible_index);
        assert!(
            marks.scores.iter().all(Option::is_none)
                && marks.match_indices.iter().all(Vec::is_empty),
            "no filter active means no row carries a score or a highlight"
        );
    }

    #[test]
    fn fold_outline_filtered_folds_the_rebuilt_tree_same_as_an_unfiltered_build() {
        // The fold applies to the REBUILT (post-filter) row list, not the pre-filter one — a
        // collapsed src/ should still hide its filtered-in descendant.
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let (folded, _) = fold_outline_filtered(
            &changesets,
            OutlineMode::Tree,
            OutlineOrder::HeadFirst,
            |key| {
                *key == FoldKey::Dir {
                    path: "src".to_string(),
                    owner: None,
                }
            },
            "b.rs",
        );
        assert_eq!(
            folded.items,
            vec![OutlineItem::Dir {
                name: "src".to_string(),
                path: "src".to_string(),
                cs_idx: None,
                // `true`, not `false`: with c.rs/d.rs/top.rs filtered out, src/ is root's only
                // (and therefore last) surviving child — see the sibling test above.
                guides: vec![true],
            }],
            "src/ survives collapsed, but its own (filtered-in) b.rs descendant stays hidden"
        );
        assert_eq!(
            folded.hidden_counts,
            vec![1],
            "src/'s marker counts its one hidden (but filter-surviving) file"
        );
    }

    #[test]
    fn filter_marks_best_index_picks_the_first_strictly_highest_score() {
        let marks = FilterMarks {
            match_indices: vec![Vec::new(); 4],
            scores: vec![Some(1), Some(5), Some(5), None],
        };
        assert_eq!(
            marks.best_index(),
            Some(1),
            "the first of the two tied-highest scores (index 1) wins, not the later one"
        );
        assert_eq!(FilterMarks::empty_for(3).best_index(), None);
    }
}
