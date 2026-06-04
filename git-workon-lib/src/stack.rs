//! Stacked diff workflow support.
//!
//! This module provides the infrastructure for detecting and interacting with
//! stacked-diff tools alongside git-workon's worktree management.
//!
//! ## Model
//!
//! Stack awareness is two-dimensional:
//!
//! - [`StackModel`] — which tool manages stacks (v1: Graphite; future: branchless, sapling, spr)
//! - [`Granularity`] — how worktrees map to stacks (v1: [`Granularity::Stack`], one per stack)
//!
//! ## Default-on behavior
//!
//! When `workon.stackModel` resolves to anything other than [`StackModel::None`] (by explicit
//! config or auto-detection), every command that has a meaningful stack-aware variant uses it
//! by default. A `--no-stack` CLI flag (global across subcommands) downgrades any single
//! invocation back to branch-flat behavior.
//!
//! ## Graphite (v1)
//!
//! Stack metadata is read directly from `refs/branch-metadata/*` git refs — blobs containing
//! JSON written by the `gt` CLI. No `gt` process is needed for detection or visualization.
//! `gt track` is invoked only when registering a new branch (in `workon new` when creating a
//! fork off a stack-worktree's branch).
//!
//! ## Cross-worktree grouping
//!
//! [`group_by_stack`] partitions a parallel slice of [`Option<Stack>`] values (one per worktree)
//! into [`StackGrouping`]: a list of [`StackGroup`]s (each covering one connected stack) plus an
//! `ungrouped` list for trunk/untracked worktrees. The grouping key is `(trunk, sorted diff
//! set)`, so two worktrees in the same connected stack always collapse into one group regardless
//! of the [`Stack::diffs`] vector order returned by [`current_stack`].

mod graphite;

use std::collections::{BTreeMap, HashMap};

use git2::Repository;

use crate::error::Result;

/// Which stacked-diff tool is managing stacks in this repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackModel {
    /// No stack tool; today's branch-flat behavior.
    None,
    /// Graphite (`gt`) manages stacks via `refs/branch-metadata/*`.
    Graphite,
}

impl StackModel {
    /// Auto-detect the active stack model from the repository environment.
    ///
    /// Returns [`StackModel::Graphite`] when `gt` is on PATH **and** the repo has been
    /// initialized with `gt init` (`.graphite_repo_config` exists). Otherwise returns
    /// [`StackModel::None`].
    pub fn detect(repo: &Repository) -> Self {
        if graphite::detect_gt() && graphite::is_graphite_repo(repo) {
            Self::Graphite
        } else {
            Self::None
        }
    }
}

/// How worktrees map to stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One worktree hosts an entire stack. The user navigates between branches inside it
    /// using the stack tool's own commands (e.g. `gt up` / `gt down`).
    Stack,
}

/// A stack of branches rooted at a trunk branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// The trunk branch this stack is rooted on (e.g., `"main"`).
    pub trunk: String,
    /// All non-trunk diffs (branches) in the stack, in BFS order from bottom to top.
    pub diffs: Vec<String>,
    /// The branch that is currently HEAD in the worktree.
    pub current: String,
    /// Parent map for the diffs in this stack: `diff → parent_branch`.
    ///
    /// The parent may be another diff or the trunk itself. Only covers diffs in
    /// [`Stack::diffs`]; the trunk's own parent is not recorded. Used to reconstruct
    /// the branching tree structure (a flat [`diffs`] list cannot represent forks).
    pub parents: HashMap<String, String>,
}

/// Return all stacks present in metadata, one per connected component.
///
/// Each returned [`Stack`] corresponds to one potential [`StackGroup`] — the same `(trunk,
/// sorted diffs)` key used by [`group_by_stack`]. Used by the `list` command to surface
/// stacks that have no checked-out worktrees.
pub fn enumerate_stacks(repo: &Repository, model: StackModel) -> Result<Vec<Stack>> {
    match model {
        StackModel::None => Ok(vec![]),
        StackModel::Graphite => graphite::enumerate_stacks(repo).map_err(Into::into),
    }
}

/// Return the stack for the worktree whose HEAD is `head_branch`, or `None` if the branch
/// is not part of a tracked stack under `model`.
///
/// The returned [`Stack`] includes all branches reachable from the same stack root, not just
/// the ancestors of `head_branch`, so branching stacks are fully represented.
pub fn current_stack(
    repo: &Repository,
    head_branch: &str,
    model: StackModel,
) -> Result<Option<Stack>> {
    match model {
        StackModel::None => Ok(None),
        StackModel::Graphite => graphite::current_stack(repo, head_branch).map_err(Into::into),
    }
}

/// Returns `true` if `gt` is on PATH and this repository has been Graphite-initialized.
pub fn is_graphite_active(repo: &Repository) -> bool {
    graphite::detect_gt() && graphite::is_graphite_repo(repo)
}

/// Return the first trunk branch name from `.graphite_repo_config`, or `None` if
/// the file is missing, unparseable, or contains no trunk entries.
///
/// Use this when you need to pass `--parent <trunk>` to `gt track` and want to
/// avoid hardcoding `"main"`. Returns `None` rather than a hardcoded fallback so
/// callers can omit `--parent` entirely and let `gt` infer when the trunk is unknown.
pub fn graphite_trunk(repo: &Repository) -> Option<String> {
    graphite::graphite_trunk(repo)
}

/// One group of worktrees that share a connected stack, identified by trunk + diff set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackGroup {
    /// Representative stack (trunk + diffs in BFS bottom→top order) for the group.
    pub stack: Stack,
    /// Indices into the caller's worktree slice for members of this stack, ordered by
    /// the member branch's position in `stack.diffs` (bottom→top). Members whose
    /// branch isn't found in `stack.diffs` sort last, in original input order.
    pub members: Vec<usize>,
}

/// Result of partitioning a worktree slice by stacks via [`group_by_stack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackGrouping {
    /// Groups of worktrees sharing a connected stack, ordered by `(trunk, sorted branch set)`.
    pub groups: Vec<StackGroup>,
    /// Indices of worktrees with no stack (trunk branches, untracked branches), in input order.
    pub ungrouped: Vec<usize>,
}

/// Partition a slice of optional stack descriptors into groups sharing the same connected stack.
///
/// The grouping key is `(trunk, sorted diff set)`, so worktrees in the same connected stack
/// always collapse into one group regardless of the [`Stack::diffs`] vector order returned
/// by [`current_stack`]. Members within each group are ordered by their branch's position in
/// the representative stack's `diffs` list (bottom→top).
///
/// Groups are returned in deterministic order: sorted by `(trunk, sorted branch set)`.
///
/// When all entries are `None` (stack model inactive or `--no-stack`), `groups` is empty and
/// every index appears in `ungrouped` — callers get identical flat-list behavior.
pub fn group_by_stack(stacks: &[Option<Stack>]) -> StackGrouping {
    // BTreeMap provides sorted iteration: (trunk, branch_set) order automatically.
    let mut map: BTreeMap<(String, Vec<String>), (Stack, Vec<usize>)> = BTreeMap::new();
    let mut ungrouped: Vec<usize> = Vec::new();

    for (i, opt) in stacks.iter().enumerate() {
        match opt {
            None => ungrouped.push(i),
            Some(stack) => {
                let mut key_branches = stack.diffs.clone();
                key_branches.sort();
                let key = (stack.trunk.clone(), key_branches);
                let entry = map
                    .entry(key)
                    .or_insert_with(|| (stack.clone(), Vec::new()));
                entry.1.push(i);
            }
        }
    }

    let groups = map
        .into_values()
        .map(|(rep_stack, mut members)| {
            // Sort members by their branch's position in rep_stack.diffs (bottom→top).
            members.sort_by_key(|&idx| {
                let branch = stacks[idx]
                    .as_ref()
                    .map(|s| s.current.as_str())
                    .unwrap_or("");
                rep_stack
                    .diffs
                    .iter()
                    .position(|b| b == branch)
                    .unwrap_or(usize::MAX)
            });
            StackGroup {
                stack: rep_stack,
                members,
            }
        })
        .collect();

    StackGrouping { groups, ungrouped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(trunk: &str, diffs: &[&str], current: &str) -> Stack {
        Stack {
            trunk: trunk.to_string(),
            diffs: diffs.iter().map(|s| s.to_string()).collect(),
            current: current.to_string(),
            parents: HashMap::new(),
        }
    }

    #[test]
    fn empty_input_yields_empty_grouping() {
        let result = group_by_stack(&[]);
        assert!(result.groups.is_empty());
        assert!(result.ungrouped.is_empty());
    }

    #[test]
    fn all_none_yields_all_ungrouped() {
        let stacks: Vec<Option<Stack>> = vec![None, None, None];
        let result = group_by_stack(&stacks);
        assert!(result.groups.is_empty());
        assert_eq!(result.ungrouped, vec![0, 1, 2]);
    }

    #[test]
    fn single_stacked_worktree_forms_one_group() {
        let stacks = vec![Some(stack("main", &["feat-a"], "feat-a"))];
        let result = group_by_stack(&stacks);
        assert!(result.ungrouped.is_empty());
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].members, vec![0]);
        assert_eq!(result.groups[0].stack.trunk, "main");
    }

    #[test]
    fn two_worktrees_same_stack_collapse_into_one_group_ordered_bottom_to_top() {
        // Two worktrees in the same 2-branch stack: feat-b is bottom, feat-a is top.
        // Input idx 0 has current="feat-a" (position 1), input idx 1 has current="feat-b" (position 0).
        let stacks = vec![
            Some(stack("main", &["feat-b", "feat-a"], "feat-a")), // top worktree
            Some(stack("main", &["feat-b", "feat-a"], "feat-b")), // bottom worktree
        ];
        let result = group_by_stack(&stacks);
        assert!(result.ungrouped.is_empty());
        assert_eq!(result.groups.len(), 1);
        // Members ordered by branch position: idx 1 (feat-b, pos 0) first, idx 0 (feat-a, pos 1) second.
        assert_eq!(result.groups[0].members, vec![1, 0]);
    }

    #[test]
    fn same_stack_different_branches_vector_order_still_collapses() {
        // The two Stacks have the same set but different vector orderings — must collapse.
        let stacks = vec![
            Some(stack("main", &["feat-a", "feat-b"], "feat-a")),
            Some(stack("main", &["feat-b", "feat-a"], "feat-b")),
        ];
        let result = group_by_stack(&stacks);
        assert_eq!(
            result.groups.len(),
            1,
            "different branch vector order must still collapse"
        );
        assert!(result.ungrouped.is_empty());
    }

    #[test]
    fn two_distinct_stacks_on_same_trunk_stay_separate() {
        let stacks = vec![
            Some(stack("main", &["stack1-a"], "stack1-a")),
            Some(stack("main", &["stack2-a"], "stack2-a")),
        ];
        let result = group_by_stack(&stacks);
        assert_eq!(result.groups.len(), 2);
        assert!(result.ungrouped.is_empty());
    }

    #[test]
    fn mixed_stacked_and_ungrouped() {
        let stacks = vec![
            None, // trunk worktree
            Some(stack("main", &["feat-a", "feat-b"], "feat-a")),
            None, // another ungrouped
            Some(stack("main", &["feat-a", "feat-b"], "feat-b")),
        ];
        let result = group_by_stack(&stacks);
        assert_eq!(result.ungrouped, vec![0, 2]);
        assert_eq!(result.groups.len(), 1);
        // feat-a is position 0, feat-b is position 1 → member idx 1 (current=feat-a) first, then idx 3
        assert_eq!(result.groups[0].members, vec![1, 3]);
    }

    #[test]
    fn groups_ordered_deterministically_by_trunk_then_branch_set() {
        let stacks = vec![
            Some(stack("main", &["z-feat"], "z-feat")),
            Some(stack("dev", &["d-feat"], "d-feat")),
            Some(stack("main", &["a-feat"], "a-feat")),
        ];
        let result = group_by_stack(&stacks);
        assert_eq!(result.groups.len(), 3);
        // Ordered: (dev, [d-feat]), (main, [a-feat]), (main, [z-feat])
        assert_eq!(result.groups[0].stack.trunk, "dev");
        assert_eq!(result.groups[1].stack.diffs, vec!["a-feat"]);
        assert_eq!(result.groups[2].stack.diffs, vec!["z-feat"]);
    }
}
