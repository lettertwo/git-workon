//! The outline side pane's pure item model: given a snapshot of every reviewed changeset (label,
//! current/needs-restack flags, and per-file staged-ness), build the flat row list the pane
//! renders and the outline cursor indexes — no [`crate::app::App`]/[`crate::app::ChangesetView`]
//! dependency, mirroring how [`crate::attribute`] stays a pure module consumed by `app`/`render`.
//!
//! CS3 shipped two of the four modes ([`OutlineMode::Flat`]/[`OutlineMode::Stack`]); CS4 (this
//! revision) adds the two path-trie modes ([`OutlineMode::Tree`]/[`OutlineMode::StackTree`]) via
//! the private [`TrieNode`] builder below.

use std::collections::HashMap;

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
}

/// One file's outline-relevant data, as extracted from its owning changeset by
/// `App::outline_items` — the input [`build_items`] consumes.
#[derive(Debug, Clone)]
pub struct OutlineFile {
    pub path: String,
    pub status: StagedStatus,
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
/// `guides` (on [`Self::Dir`]/[`Self::File`]) is the tree-guide vector CS4 adds: one bool per
/// nesting level from the shallowest ancestor down to the row itself, `true` meaning "this
/// level is its parent's last child". Rendering uses every-element-but-the-last to decide
/// whether to draw a continuing `│` or blank space at that column, and the last element to draw
/// `└─`/`├─` for the row's own connector. [`OutlineMode::Flat`]/[`OutlineMode::Stack`] rows carry
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
        /// ADR-037: this changeset hasn't been diffed yet — rendered as a loading indication.
        loading: bool,
        /// ADR-037: this changeset's acquisition attempt errored — rendered as a marker.
        failed: bool,
    },
    /// A directory row — only emitted in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`]. Not a
    /// jump target: it carries no `cs_idx`/`file_idx`, so `App::outline_move_by` no-ops on it
    /// (same as [`Self::Header`]) and `App::outline_confirm` also no-ops on it (CS4 decision —
    /// there's no expand/collapse state to toggle, so Enter on a directory row does nothing but
    /// still returns focus to the diff, matching every other confirm outcome).
    Dir { name: String, guides: Vec<bool> },
    /// A file row — the target of every outline->diff jump. `path` is the FULL path in
    /// [`OutlineMode::Flat`]/[`OutlineMode::Stack`] (unchanged from CS3), but is just the leaf
    /// segment in [`OutlineMode::Tree`]/[`OutlineMode::StackTree`] — the ancestor directory rows
    /// already carry the rest of the path, so re-printing it on every leaf would be redundant.
    File {
        cs_idx: usize,
        file_idx: usize,
        path: String,
        status: StagedStatus,
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

/// Build the outline's row list for `mode` from every reviewed changeset, in the same base ->
/// head order `App::changesets` holds them.
pub fn build_items(changesets: &[OutlineChangeset], mode: OutlineMode) -> Vec<OutlineItem> {
    match mode {
        OutlineMode::Flat => build_flat(changesets),
        OutlineMode::Stack => build_stack(changesets),
        OutlineMode::Tree => build_tree(changesets),
        OutlineMode::StackTree => build_stack_tree(changesets),
    }
}

/// [`OutlineMode::Stack`]: a header per changeset, then its files in order — no de-duplication,
/// every changeset's own copy of a path (if touched more than once across the stack) gets its
/// own row under its own header.
fn build_stack(changesets: &[OutlineChangeset]) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for (cs_idx, cs) in changesets.iter().enumerate() {
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
                guides: Vec::new(),
            });
        }
    }
    items
}

/// [`OutlineMode::Flat`]: every changed path once, in FIRST-appearance order (a stable, readable
/// order that doesn't reshuffle just because a later changeset re-touches an earlier path), but
/// pointing at its LAST (newest / closest-to-head) occurrence — "last-write-wins" per the locked
/// design: a path touched by both an earlier committed changeset and the uncommitted layer
/// should jump to (and show the staged-ness of) the uncommitted layer's copy, not the stale
/// committed one.
fn build_flat(changesets: &[OutlineChangeset]) -> Vec<OutlineItem> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, (usize, usize, StagedStatus)> = HashMap::new();
    for (cs_idx, cs) in changesets.iter().enumerate() {
        for (file_idx, file) in cs.files.iter().enumerate() {
            if !latest.contains_key(&file.path) {
                order.push(file.path.clone());
            }
            latest.insert(file.path.clone(), (cs_idx, file_idx, file.status));
        }
    }
    order
        .into_iter()
        .map(|path| {
            let (cs_idx, file_idx, status) = latest[&path];
            OutlineItem::File {
                cs_idx,
                file_idx,
                path,
                status,
                guides: Vec::new(),
            }
        })
        .collect()
}

/// De-dupe every changed path across the stack to its LAST occurrence (mirrors
/// [`build_flat`]'s last-write-wins rule), independent of iteration/insertion order — the trie
/// builders below re-sort by path segment anyway, so no stable-order bookkeeping is needed here.
fn latest_by_path(
    changesets: &[OutlineChangeset],
) -> HashMap<String, (usize, usize, StagedStatus)> {
    let mut latest = HashMap::new();
    for (cs_idx, cs) in changesets.iter().enumerate() {
        for (file_idx, file) in cs.files.iter().enumerate() {
            latest.insert(file.path.clone(), (cs_idx, file_idx, file.status));
        }
    }
    latest
}

/// A node in the path trie the tree modes build. A node with `file.is_some()` is a leaf (a
/// changed file at that exact path); otherwise it's a pure directory node. Git paths never
/// collide a file and a directory at the same path, so a node is never both.
#[derive(Debug, Default)]
struct TrieNode {
    file: Option<(usize, usize, StagedStatus)>,
    /// Insertion order is irrelevant — [`emit`] re-sorts children (dirs-after-files, alpha
    /// within group) every time it flattens a node.
    children: Vec<(String, TrieNode)>,
}

impl TrieNode {
    fn insert(&mut self, segments: &[&str], cs_idx: usize, file_idx: usize, status: StagedStatus) {
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
            child.file = Some((cs_idx, file_idx, status));
        } else {
            child.insert(rest, cs_idx, file_idx, status);
        }
    }
}

/// Flatten `node`'s children into `items`, depth-first, in "dirs after files at each level,
/// alpha within group" order (matches the `~/.config/nvim/lua/app/review/ui/outline.lua`
/// prototype's `_build_path_tree`/`_emit_tree_node`: files read before directories at a given
/// level, so a directory's own contents don't visually separate its sibling files from the
/// directory listing above them). `ancestors_last` is the growing guide vector — see
/// [`OutlineItem`]'s doc comment for how rendering consumes it.
fn emit(node: &TrieNode, ancestors_last: &[bool], items: &mut Vec<OutlineItem>) {
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
    let ordered: Vec<&(String, TrieNode)> = files.into_iter().chain(dirs).collect();
    let n = ordered.len();
    for (i, (name, child)) in ordered.into_iter().enumerate() {
        let is_last = i == n - 1;
        let mut guides = ancestors_last.to_vec();
        guides.push(is_last);
        match child.file {
            Some((cs_idx, file_idx, status)) => {
                items.push(OutlineItem::File {
                    cs_idx,
                    file_idx,
                    path: name.clone(),
                    status,
                    guides,
                });
            }
            None => {
                items.push(OutlineItem::Dir {
                    name: name.clone(),
                    guides: guides.clone(),
                });
                emit(child, &guides, items);
            }
        }
    }
}

/// [`OutlineMode::Tree`]: [`build_flat`]'s de-duped path set, rendered as a single directory
/// trie spanning the whole stack (no changeset headers).
fn build_tree(changesets: &[OutlineChangeset]) -> Vec<OutlineItem> {
    let latest = latest_by_path(changesets);
    let mut root = TrieNode::default();
    for (path, (cs_idx, file_idx, status)) in &latest {
        let segments: Vec<&str> = path.split('/').collect();
        root.insert(&segments, *cs_idx, *file_idx, *status);
    }
    let mut items = Vec::new();
    emit(&root, &[], &mut items);
    items
}

/// [`OutlineMode::StackTree`]: [`build_stack`]'s per-changeset header grouping, but each
/// changeset's own files are flattened into their own nested trie (no cross-changeset dedup —
/// each changeset trie is built from just that changeset's files, matching `build_stack`'s "every
/// changeset's own copy gets its own row" rule).
fn build_stack_tree(changesets: &[OutlineChangeset]) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for (cs_idx, cs) in changesets.iter().enumerate() {
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
            root.insert(&segments, cs_idx, file_idx, file.status);
        }
        emit(&root, &[], &mut items);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(
        label: &str,
        current: bool,
        needs_restack: bool,
        files: &[(&str, StagedStatus)],
    ) -> OutlineChangeset {
        OutlineChangeset {
            label: label.to_string(),
            current,
            needs_restack,
            loading: false,
            failed: false,
            files: files
                .iter()
                .map(|(p, s)| OutlineFile {
                    path: p.to_string(),
                    status: *s,
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
        let items = build_items(&changesets, OutlineMode::Stack);
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
        let items = build_items(&changesets, OutlineMode::Stack);
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
        let items = build_items(&changesets, OutlineMode::Flat);
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
        let items = build_items(&changesets, OutlineMode::Flat);
        assert_eq!(items.len(), 1, "the shared path must appear exactly once");
        assert_eq!(
            items[0],
            OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "shared.txt".to_string(),
                status: StagedStatus::Unstaged,
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
        let items = build_items(&changesets, OutlineMode::Flat);
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
    /// depth > 1 and the dirs-after-files/alpha-within-group ordering at every level.
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
    fn tree_mode_builds_dirs_after_files_alpha_within_group_with_correct_depth_and_guides() {
        let changesets = vec![deep_path_changeset("cs-a", true, false)];
        let items = build_items(&changesets, OutlineMode::Tree);
        assert_eq!(
            items,
            vec![
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "top.rs".to_string(),
                    status: StagedStatus::None,
                    guides: vec![false],
                },
                OutlineItem::Dir {
                    name: "src".to_string(),
                    guides: vec![true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 3,
                    path: "d.rs".to_string(),
                    status: StagedStatus::None,
                    guides: vec![true, false],
                },
                OutlineItem::Dir {
                    name: "a".to_string(),
                    guides: vec![true, true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 1,
                    path: "b.rs".to_string(),
                    status: StagedStatus::None,
                    guides: vec![true, true, false],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 2,
                    path: "c.rs".to_string(),
                    status: StagedStatus::None,
                    guides: vec![true, true, true],
                },
            ],
            "root: top.rs (file) then src/ (dir); under src/: d.rs (file) then a/ (dir); \
             under src/a/: b.rs then c.rs — files-before-dirs, alpha within each group"
        );
        assert_eq!(items[0].depth(), 0, "top.rs is a root-level row");
        assert_eq!(items[1].depth(), 0, "src/ is a root-level row");
        assert_eq!(items[2].depth(), 1, "src/d.rs is one level deep");
        assert_eq!(items[4].depth(), 2, "src/a/b.rs is two levels deep");
    }

    #[test]
    fn tree_mode_dedupes_a_shared_path_to_the_newest_changesets_occurrence() {
        let changesets = vec![
            cs("cs-a", false, false, &[("shared.txt", StagedStatus::None)]),
            cs("cs-b", true, false, &[("shared.txt", StagedStatus::Staged)]),
        ];
        let items = build_items(&changesets, OutlineMode::Tree);
        assert_eq!(
            items,
            vec![OutlineItem::File {
                cs_idx: 1,
                file_idx: 0,
                path: "shared.txt".to_string(),
                status: StagedStatus::Staged,
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
        let items = build_items(&changesets, OutlineMode::StackTree);
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
                    guides: vec![true],
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "y.txt".to_string(),
                    status: StagedStatus::None,
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
                    guides: vec![true],
                },
            ],
            "each changeset's files form their own trie nested under that changeset's header, \
             with no cross-changeset dedup"
        );
    }
}
