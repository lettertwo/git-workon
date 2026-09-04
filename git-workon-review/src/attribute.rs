//! Staged-ness attribution for the **combined** (`HEAD` ↔ worktree) view — locked decision #7 in
//! the M4 plan. Pure: given a file's unstaged/staged sub-[`FileChange`]s (already looked up by
//! `App` via its `unstaged_idx`/`staged_idx` mapping), produces two membership sets keyed by the
//! exact line numbers the combined view's `AlignedRow`s already carry. No content matching, no
//! reconstruction — our rows carry real `old_lnum`/`new_lnum`, unlike the
//! `review-tui-spike` prototype's renderer, which had to reconstruct them and so needed a
//! del-run anchor heuristic to stay in sync with its picker. That heuristic has no analog here.
//!
//! ## The asymmetry (forced by coordinate alignment, not a stylistic choice)
//!
//! The combined view's OLD side is `HEAD` — the same "old" reference as the **staged** diff
//! (`HEAD` ↔ index). So a combined **deletion** at `old_lnum` M is "already staged" exactly when
//! the staged diff also deletes `old_lnum` M.
//!
//! The combined view's NEW side is the worktree — the same "new" reference as the **unstaged**
//! diff (index ↔ worktree). So a combined **addition** at `new_lnum` N is "not yet staged"
//! exactly when the unstaged diff also adds `new_lnum` N.
//!
//! These are two different sub-diffs keyed on two different sides — do not "fix" this to be
//! symmetric; the asymmetry is what makes the lookup correct.

use std::collections::HashSet;

use crate::model::{FileChange, LineKind};

/// Per-file membership sets built fresh each frame (see `render`'s combined-role render path) —
/// never cached on `App`, since the index can move out from under a stale cache (the M4 index
/// watcher refreshes the index independently of the render loop).
#[derive(Debug, Clone, Default)]
pub struct Attribution {
    /// `new_lnum`s of every addition in the file's UNSTAGED (index ↔ worktree) sub-diff. An
    /// combined Add cell at one of these lines is NOT YET staged (renders bright).
    pub unstaged_adds: HashSet<u32>,
    /// `old_lnum`s of every deletion in the file's STAGED (`HEAD` ↔ index) sub-diff. A combined
    /// Del cell at one of these lines IS already staged (renders dim).
    pub staged_dels: HashSet<u32>,
}

impl Attribution {
    /// Build from the current file's unstaged/staged sub-`FileChange`s, either of which may be
    /// absent (the file has no change in that role) — an absent role contributes an empty set on
    /// its axis rather than an error, so a file with no staged sub-diff renders every Del bright,
    /// and a file with no unstaged sub-diff renders every Add dim.
    pub fn build(unstaged: Option<&FileChange>, staged: Option<&FileChange>) -> Self {
        let mut unstaged_adds = HashSet::new();
        if let Some(file) = unstaged {
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    if line.kind == LineKind::Addition {
                        if let Some(n) = line.new_lnum {
                            unstaged_adds.insert(n);
                        }
                    }
                }
            }
        }

        let mut staged_dels = HashSet::new();
        if let Some(file) = staged {
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    if line.kind == LineKind::Deletion {
                        if let Some(n) = line.old_lnum {
                            staged_dels.insert(n);
                        }
                    }
                }
            }
        }

        Self {
            unstaged_adds,
            staged_dels,
        }
    }

    /// True when a combined Add cell at `new_lnum` is NOT YET staged (renders bright).
    pub fn add_is_unstaged(&self, new_lnum: u32) -> bool {
        self.unstaged_adds.contains(&new_lnum)
    }

    /// True when a combined Del cell at `old_lnum` IS already staged (renders dim).
    pub fn del_is_staged(&self, old_lnum: u32) -> bool {
        self.staged_dels.contains(&old_lnum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hunk, HunkLine};

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> HunkLine {
        HunkLine {
            kind,
            content: Vec::new(),
            old_lnum: old,
            new_lnum: new,
            missing_newline: false,
        }
    }

    fn file_with_hunks(hunks: Vec<Hunk>) -> FileChange {
        FileChange {
            path: "f.txt".to_string(),
            old_path: None,
            status: crate::model::FileStatus::Modified,
            is_binary: false,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks,
        }
    }

    fn hunk(lines: Vec<HunkLine>) -> Hunk {
        Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            header: Vec::new(),
            lines,
        }
    }

    #[test]
    fn absent_staged_sub_diff_yields_empty_staged_dels() {
        let unstaged = file_with_hunks(vec![hunk(vec![line(LineKind::Addition, None, Some(3))])]);
        let attribution = Attribution::build(Some(&unstaged), None);
        assert!(attribution.staged_dels.is_empty());
        assert!(attribution.unstaged_adds.contains(&3));
    }

    #[test]
    fn absent_unstaged_sub_diff_yields_empty_unstaged_adds() {
        let staged = file_with_hunks(vec![hunk(vec![line(LineKind::Deletion, Some(5), None)])]);
        let attribution = Attribution::build(None, Some(&staged));
        assert!(attribution.unstaged_adds.is_empty());
        assert!(attribution.staged_dels.contains(&5));
    }

    #[test]
    fn both_absent_yields_two_empty_sets() {
        let attribution = Attribution::build(None, None);
        assert!(attribution.unstaged_adds.is_empty());
        assert!(attribution.staged_dels.is_empty());
    }

    #[test]
    fn multi_hunk_files_union_across_hunks() {
        let unstaged = file_with_hunks(vec![
            hunk(vec![line(LineKind::Addition, None, Some(2))]),
            hunk(vec![line(LineKind::Addition, None, Some(40))]),
        ]);
        let staged = file_with_hunks(vec![
            hunk(vec![line(LineKind::Deletion, Some(7), None)]),
            hunk(vec![line(LineKind::Deletion, Some(70), None)]),
        ]);
        let attribution = Attribution::build(Some(&unstaged), Some(&staged));
        assert_eq!(
            attribution.unstaged_adds,
            HashSet::from([2, 40]),
            "adds from every hunk must be unioned, not just the first"
        );
        assert_eq!(
            attribution.staged_dels,
            HashSet::from([7, 70]),
            "dels from every hunk must be unioned, not just the first"
        );
    }

    #[test]
    fn no_cross_contamination_between_add_and_del_axes_at_the_same_lnum() {
        // Line 9 is an UNSTAGED addition (new_lnum 9) and, independently, a STAGED deletion
        // whose old_lnum happens to also be 9 — the two axes must stay on their own set, and a
        // context/other-kind line at a shared lnum on the "wrong" axis must not leak in.
        let unstaged = file_with_hunks(vec![hunk(vec![
            line(LineKind::Addition, None, Some(9)),
            line(LineKind::Deletion, Some(9), None), // wrong axis for unstaged_adds
        ])]);
        let staged = file_with_hunks(vec![hunk(vec![
            line(LineKind::Deletion, Some(9), None),
            line(LineKind::Addition, None, Some(9)), // wrong axis for staged_dels
        ])]);
        let attribution = Attribution::build(Some(&unstaged), Some(&staged));
        assert_eq!(attribution.unstaged_adds, HashSet::from([9]));
        assert_eq!(attribution.staged_dels, HashSet::from([9]));
        // Cross-axis membership is independent: this file has no other lnums at all, so a lookup
        // for a line not present on the RIGHT axis (e.g. an add lookup for a lnum that's only a
        // staged deletion) must not accidentally match.
        assert!(!attribution.add_is_unstaged(100));
        assert!(!attribution.del_is_staged(100));
    }

    #[test]
    fn context_lines_never_contribute_to_either_set() {
        let unstaged = file_with_hunks(vec![hunk(vec![line(LineKind::Context, Some(1), Some(1))])]);
        let staged = file_with_hunks(vec![hunk(vec![line(LineKind::Context, Some(1), Some(1))])]);
        let attribution = Attribution::build(Some(&unstaged), Some(&staged));
        assert!(attribution.unstaged_adds.is_empty());
        assert!(attribution.staged_dels.is_empty());
    }
}
