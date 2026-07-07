//! The outline side pane's pure item model: given a snapshot of every reviewed changeset (label,
//! current/needs-restack flags, and per-file staged-ness), build the flat row list the pane
//! renders and the outline cursor indexes — no [`crate::app::App`]/[`crate::app::ChangesetView`]
//! dependency, mirroring how [`crate::attribute`] stays a pure module consumed by `app`/`render`.
//!
//! CS3 ships two of the eventual four modes ([`OutlineMode::Flat`]/[`OutlineMode::Stack`]); the
//! two path-trie modes (tree / stack-tree) are CS4's addition to this same module.

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
}

impl OutlineMode {
    /// `i`'s cycle order: `Flat -> Stack -> Flat`. CS4 will extend this to all four modes.
    pub fn cycle(self) -> Self {
        match self {
            OutlineMode::Flat => OutlineMode::Stack,
            OutlineMode::Stack => OutlineMode::Flat,
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
    pub files: Vec<OutlineFile>,
}

/// One row the outline pane renders and the outline cursor can land on. `cs_idx` is always the
/// index into `App`'s changeset list the row belongs to; `file_idx` (on [`Self::File`]) is the
/// index into THAT changeset's file list — together they're exactly what
/// `App::switch_changeset`/`App::goto_changeset` need to jump the diff there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineItem {
    /// A changeset header — only emitted in [`OutlineMode::Stack`].
    Header {
        cs_idx: usize,
        label: String,
        current: bool,
        needs_restack: bool,
    },
    /// A file row — the target of every outline->diff jump.
    File {
        cs_idx: usize,
        file_idx: usize,
        path: String,
        status: StagedStatus,
    },
}

/// Build the outline's row list for `mode` from every reviewed changeset, in the same base ->
/// head order `App::changesets` holds them.
pub fn build_items(changesets: &[OutlineChangeset], mode: OutlineMode) -> Vec<OutlineItem> {
    match mode {
        OutlineMode::Flat => build_flat(changesets),
        OutlineMode::Stack => build_stack(changesets),
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
        });
        for (file_idx, file) in cs.files.iter().enumerate() {
            items.push(OutlineItem::File {
                cs_idx,
                file_idx,
                path: file.path.clone(),
                status: file.status,
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
    let mut latest: std::collections::HashMap<String, (usize, usize, StagedStatus)> =
        std::collections::HashMap::new();
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
            }
        })
        .collect()
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
            files: files
                .iter()
                .map(|(p, s)| OutlineFile {
                    path: p.to_string(),
                    status: *s,
                })
                .collect(),
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
                },
                OutlineItem::File {
                    cs_idx: 0,
                    file_idx: 0,
                    path: "a1.txt".to_string(),
                    status: StagedStatus::None,
                },
                OutlineItem::Header {
                    cs_idx: 1,
                    label: "cs-b".to_string(),
                    current: true,
                    needs_restack: true,
                },
                OutlineItem::File {
                    cs_idx: 1,
                    file_idx: 0,
                    path: "b1.txt".to_string(),
                    status: StagedStatus::None,
                },
            ]
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
                OutlineItem::Header { .. } => unreachable!(),
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
    fn mode_cycles_between_flat_and_stack() {
        assert_eq!(OutlineMode::Flat.cycle(), OutlineMode::Stack);
        assert_eq!(OutlineMode::Stack.cycle(), OutlineMode::Flat);
    }
}
