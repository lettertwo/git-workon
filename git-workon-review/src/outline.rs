//! The outline side pane's pure item model: given a snapshot of every reviewed changeset (label,
//! current/needs-restack flags, and per-file staged-ness), build the flat row list the pane
//! renders and the outline cursor indexes — no [`crate::app::App`]/[`crate::app::ChangesetView`]
//! dependency, mirroring how [`crate::attribute`] stays a pure module consumed by `app`/`render`.
//!
//! CS3 shipped two of the four modes ([`OutlineMode::Flat`]/[`OutlineMode::Stack`]); CS4 added
//! the two path-trie modes ([`OutlineMode::Tree`]/[`OutlineMode::StackTree`]) via the private
//! [`TrieNode`] builder below. CS5 adds each file row's [`crate::model::FileStatus`] (the `M`/
//! `A`/`D`/... change-status letter — see [`OutlineFile::change`]/[`OutlineItem::File::change`]'s
//! doc comments for why that's a wholly separate field from [`StagedStatus`], which tracks
//! index/worktree staged-ness, not the underlying change kind). Pulling in
//! `crate::model::FileStatus` keeps this module's pure-data posture intact: `model.rs` is itself
//! a pure data module (no `App`/`ChangesetView` dependency), so importing its plain enum doesn't
//! reintroduce the `App` coupling this module was factored out to avoid.

use std::collections::HashMap;

use crate::model::FileStatus;

/// Which of the outline's row-building strategies is active — cycled by `i` (only while the
/// outline pane has focus; see `App::outline_cycle_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutlineMode {
    /// Every changed path across the whole stack, once each, no changeset headers.
    Flat,
    /// A changeset header row per changeset, followed by that changeset's file rows — the
    /// default (locked choice for CS3: this is the mode that actually shows the stack
    /// structure M5 exists to surface).
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
    /// `i`'s cycle order: `Flat -> Stack -> Tree -> StackTree -> Flat`. Flat/Stack (the
    /// non-trie modes) come first since they're the CS3 default pair; the trie modes follow in
    /// the same flat/grouped pairing (Tree mirrors Flat's cross-stack dedup, StackTree mirrors
    /// Stack's per-changeset grouping).
    pub fn cycle(self) -> Self {
        match self {
            OutlineMode::Flat => OutlineMode::Stack,
            OutlineMode::Stack => OutlineMode::Tree,
            OutlineMode::Tree => OutlineMode::StackTree,
            OutlineMode::StackTree => OutlineMode::Flat,
        }
    }
}

/// Which end of the stack the outline's stack-shaped modes ([`OutlineMode::Stack`]/
/// [`OutlineMode::StackTree`]) display first — CS3 dogfooding feedback #2. Purely a display
/// order: [`OutlineItem`]'s `cs_idx`/`file_idx` always stay TRUE indices into `App::changesets`
/// regardless of which way the rows are painted (see [`build_items`]'s doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutlineOrder {
    /// The most recently created (head) changeset's header renders first — the CS3 default.
    #[default]
    HeadFirst,
    /// The stack's base changeset renders first, matching `App::changesets`' own base -> head
    /// storage order (today's pre-CS3 behavior).
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

/// A file's staged-ness for the outline's status column — a minimal indicator (locked CS3
/// scope: NOT the prototype's X/Y two-column git-status matrix). Only meaningful for the
/// uncommitted changeset's files; a committed changeset's files always resolve to `None`
/// because their `unstaged_idx`/`staged_idx` maps are always-empty (see
/// `DiffState::from_committed`) — the same "derive, don't special-case" collapse
/// `effective_zoom` already relies on, so no committed-specific branch is needed here either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StagedStatus {
    /// No staged/unstaged sub-diff info for this file (a committed changeset's file, or an
    /// uncommitted file that — impossibly — has a combined change but neither sub-change).
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

    /// The single-character glyph the outline renders in the status column, or a blank space
    /// for [`StagedStatus::None`] (keeps every file row's path starting at the same column
    /// regardless of whether it carries a status).
    pub fn glyph(self) -> char {
        match self {
            StagedStatus::None => ' ',
            StagedStatus::Unstaged => '+',
            StagedStatus::Staged => '\u{2713}',  // ✓
            StagedStatus::Partial => '\u{25D0}', // ◐
        }
    }

    /// The nerd-font equivalent of [`StagedStatus::glyph`] (CS3, `workon.review.outline.icons =
    /// nerd`) — picked from the classic BMP `fa` set for wider font compatibility (see
    /// `icons.rs`'s module doc). [`StagedStatus::None`] stays a blank space, same as
    /// [`StagedStatus::glyph`], since there's no status to convey.
    pub fn nerd_glyph(self) -> char {
        match self {
            StagedStatus::None => ' ',
            StagedStatus::Unstaged => '\u{f067}', // nf-fa-plus
            StagedStatus::Staged => '\u{f00c}',   // nf-fa-check
            StagedStatus::Partial => '\u{f042}',  // nf-fa-adjust
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
    /// CS5: the underlying change kind (Modified/Added/Deleted/...), lifted from the owning
    /// [`crate::model::FileChange::status`] — drives the outline's `M`/`A`/`D`/`R`/`C`/`?`/`U`
    /// letter (`render::build_outline_line`), independent of [`Self::status`] above.
    pub change: FileStatus,
}

/// One changeset's outline-relevant data — a snapshot, not a borrow, so this module never needs
/// to know about [`crate::app::ChangesetView`] or `workon::Changeset` at all.
#[derive(Debug, Clone)]
pub struct OutlineChangeset {
    /// The changeset's title, falling back to its name — same rule the winbar (render.rs)
    /// already uses.
    pub label: String,
    /// Mirrors `workon::Changeset::current` — drives the outline's green current marker.
    pub current: bool,
    /// Mirrors `workon::Changeset::needs_restack` — drives the outline's amber warning glyph.
    pub needs_restack: bool,
    /// ADR-031: the changeset's diff hasn't been acquired yet — the header shows a loading
    /// indication in place of the (currently absent, since `files` is empty for a `Pending`
    /// slot) file rows.
    pub loading: bool,
    /// ADR-031: the acquisition attempt for this changeset errored — the header marks it.
    pub failed: bool,
    pub files: Vec<OutlineFile>,
}

/// One row the outline pane renders and the outline cursor can land on. `cs_idx` is always the
/// index into `App`'s changeset list the row belongs to; `file_idx` (on [`Self::File`]) is the
/// index into THAT changeset's file list — together they're exactly what
/// `App::switch_changeset`/`App::goto_changeset` need to jump the diff there.
///
/// `guides` (on [`Self::Dir`]/[`Self::File`]) is the tree-guide vector CS4 adds: one bool per
/// nesting level from the shallowest ancestor down to the row itself, `true` meaning "this
/// level is its parent's last child". Rendering uses every-element-but-the-last to decide
/// whether to draw a continuing `│` or blank space at that column, and the last element to draw
/// `╰─`/`├─` for the row's own connector (CS4 rounds the last-child corner). [`OutlineMode::Flat`]/[`OutlineMode::Stack`] rows carry
/// an EMPTY `guides` — that's the signal to `render::build_outline_line` to fall back to the
/// flat two-space indent instead of drawing tree connectors; a non-empty `guides` of length 1
/// means "top-level tree row" (depth 0), so emptiness and depth-0 are deliberately distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineItem {
    /// A changeset header — emitted in [`OutlineMode::Stack`]/[`OutlineMode::StackTree`].
    Header {
        cs_idx: usize,
        label: String,
        current: bool,
        needs_restack: bool,
        /// ADR-031: this changeset hasn't been diffed yet — rendered as a loading indication.
        loading: bool,
        /// ADR-031: this changeset's acquisition attempt errored — rendered as a marker.
        failed: bool,
    },
    /// A directory row — only emitted in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`]. Not a
    /// jump target: it carries no `file_idx`, so `App::outline_move_by` no-ops on it (same as
    /// [`Self::Header`]) and `App::outline_confirm` also no-ops on it (CS4 decision — there's no
    /// expand/collapse state to toggle, so Enter on a directory row does nothing but still
    /// returns focus to the diff, matching every other confirm outcome).
    Dir {
        name: String,
        /// The FULL path from the trie root (e.g. `"src/cmd"`), unlike `name` which is just the
        /// leaf segment — CS4's summary panel needs the whole path to filter files under this
        /// directory (see `crate::summary::dir_summary`).
        path: String,
        /// `Some(cs_idx)` when this row's trie is per-changeset ([`OutlineMode::StackTree`] —
        /// the same true index its owning [`Self::Header`] carries); `None` in the cross-stack
        /// [`OutlineMode::Tree`], whose single trie spans every changeset (so a dir row there has
        /// no single owning changeset to scope a summary to — CS4's `App::summary_for` instead
        /// aggregates over [`latest_by_path`]'s de-duped set for that case).
        cs_idx: Option<usize>,
        guides: Vec<bool>,
    },
    /// A file row — the target of every outline->diff jump. `path` is the FULL path in
    /// [`OutlineMode::Flat`]/[`OutlineMode::Stack`] (unchanged from CS3), but is just the leaf
    /// segment in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`] — the ancestor directory rows
    /// already carry the rest of the path, so re-printing it on every leaf would be redundant.
    File {
        cs_idx: usize,
        file_idx: usize,
        path: String,
        status: StagedStatus,
        /// CS5: the change kind (Modified/Added/Deleted/...) — see [`OutlineFile::change`]'s doc
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
pub fn build_items(
    changesets: &[OutlineChangeset],
    mode: OutlineMode,
    order: OutlineOrder,
) -> Vec<OutlineItem> {
    match mode {
        OutlineMode::Flat => build_flat(changesets, order),
        OutlineMode::Stack => build_stack(changesets, order),
        OutlineMode::Tree => build_tree(changesets),
        OutlineMode::StackTree => build_stack_tree(changesets, order),
    }
}

/// [`OutlineMode::Stack`]: a header per changeset, then its files in order — no de-duplication,
/// every changeset's own copy of a path (if touched more than once across the stack) gets its
/// own row under its own header. `order` picks which end of the stack paints first; `cs_idx`/
/// `file_idx` are computed from the ORIGINAL (base -> head) enumeration before any reversal, so
/// they stay true indices into `App::changesets` either way.
fn build_stack(changesets: &[OutlineChangeset], order: OutlineOrder) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for (cs_idx, cs) in scan_order(changesets, order) {
        items.push(OutlineItem::Header {
            cs_idx,
            label: cs.label.clone(),
            current: cs.current,
            needs_restack: cs.needs_restack,
            loading: cs.loading,
            failed: cs.failed,
        });
        for (file_idx, file) in cs.files.iter().enumerate() {
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
fn build_flat(changesets: &[OutlineChangeset], order: OutlineOrder) -> Vec<OutlineItem> {
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
        .map(|path| {
            let occ = latest[&path];
            OutlineItem::File {
                cs_idx: occ.cs_idx,
                file_idx: occ.file_idx,
                path,
                status: occ.status,
                change: occ.change,
                guides: Vec::new(),
            }
        })
        .collect()
}

/// De-dupe every changed path across the stack to its LAST occurrence (mirrors
/// [`build_flat`]'s last-write-wins rule), independent of iteration/insertion order — the trie
/// builders below re-sort by path segment anyway, so no stable-order bookkeeping is needed here.
///
/// `pub(crate)`: CS4's `App::summary_for` reuses this directly to aggregate a
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
/// ([`StagedStatus`], CS3) and change kind ([`FileStatus`], CS5). Named because the bare 4-tuple
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
/// alpha within group" order (CS3 dogfooding feedback #7: directories read before files at a
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
fn build_tree(changesets: &[OutlineChangeset]) -> Vec<OutlineItem> {
    let latest = latest_by_path(changesets);
    let mut root = TrieNode::default();
    for (path, occ) in &latest {
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
fn build_stack_tree(changesets: &[OutlineChangeset], order: OutlineOrder) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for (cs_idx, cs) in scan_order(changesets, order) {
        items.push(OutlineItem::Header {
            cs_idx,
            label: cs.label.clone(),
            current: cs.current,
            needs_restack: cs.needs_restack,
            loading: cs.loading,
            failed: cs.failed,
        });
        let mut root = TrieNode::default();
        for (file_idx, file) in cs.files.iter().enumerate() {
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

    /// [`cs`] variant that lets a test pin each file's [`FileStatus`] explicitly (CS5).
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

    /// [`cs`] variant for ADR-031's slot tests — builds a `Pending`/`Failed` outline changeset
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

    /// ADR-031: a `Pending`/`Failed` changeset (no files) still emits a Stack-mode header row,
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
                    label: "cs-pending".to_string(),
                    current: false,
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

    /// CS3: [`OutlineOrder::HeadFirst`] flips [`build_flat`]'s DISPLAY scan (first-appearance
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
        assert_eq!(OutlineMode::Flat.cycle(), OutlineMode::Stack);
        assert_eq!(OutlineMode::Stack.cycle(), OutlineMode::Tree);
        assert_eq!(OutlineMode::Tree.cycle(), OutlineMode::StackTree);
        assert_eq!(OutlineMode::StackTree.cycle(), OutlineMode::Flat);
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

    /// CS3: [`OutlineOrder::HeadFirst`] (the new default) shows the LAST changeset's ([`cs-c`],
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

    /// CS3: [`OutlineOrder::BaseFirst`] restores the pre-CS3 base -> head header order.
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
}
