//! List command with status filtering.
//!
//! Lists worktrees with optional status-based filtering to help discover
//! worktrees in specific states.
//!
//! ## Status Filters
//!
//! - `--dirty` - Show worktrees with uncommitted changes
//! - `--clean` - Show worktrees without uncommitted changes
//! - `--ahead` - Show worktrees with unpushed commits
//! - `--behind` - Show worktrees behind their upstream
//! - `--gone` - Show worktrees whose upstream branch has been deleted
//!
//! ## Filter Combination Logic
//!
//! Multiple filters use AND logic - all must match:
//! ```bash
//! git workon list --dirty --ahead  # Show dirty AND ahead worktrees
//! git workon list --clean --gone   # Show clean AND gone worktrees
//! ```
//!
//! Conflicting filters (--dirty and --clean together) produce an error.
//!
//! ## Fail-Safe Error Handling
//!
//! When checking status (dirty, unpushed, etc.), errors default to false
//! using `.unwrap_or(false)`. This ensures that problems reading one worktree
//! don't break the entire list operation.
//!
//! Conservative behavior: `has_unpushed_commits()` returns true for gone upstreams
//! (we can't know if commits are pushed when the upstream is deleted).
//!
//! ## Stack-aware output
//!
//! When a stack model is active (e.g. Graphite) **and no filter is active**, worktrees
//! and stack metadata are rendered as a unified tree. Each trunk is a single root node;
//! stacks hang underneath; untracked worktrees appear at the root level sorted by
//! most-recent activity. Metadata-only diffs (branches tracked by Graphite with no
//! worktree) appear as `◯` nodes in the tree.
//!
//! When any status filter is active the tree is suppressed: only matching worktrees are
//! listed in the plain flat format (same as `--no-stack`). Metadata-only diffs have no
//! working tree and can never satisfy a worktree-status filter, so they are excluded.
//! See ADR-025.
//!
//! The glyph vocabulary encodes two independent axes:
//! fill = a worktree exists on disk; halo = you are standing in it right now.
//!
//! - `◯` dim — metadata-only (no worktree)
//! - `◎` bold — a worktree exists but is not the current directory
//! - `◉` green+bold — the active (current-directory) worktree
//!
//! Passing `--no-stack` or using no stack model falls back to the flat list (identical
//! to the pre-stack behavior, no glyphs or connectors).
//!
//! ```text
//! ◉ main             ↑   2m ago  ← here
//! ├─◯ api-1
//! │ ◎ api-2          ↑   2h ago
//! └─◯ shared  ./base     5d ago
//!   ├─◯ branch-x
//!   └─◯ branch-y
//! ◎ ee/testing           1mo ago
//! ```

use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use workon::{
    current_stack, enumerate_stacks, get_repo, get_worktrees, group_by_stack, WorkonConfig,
    WorktreeDescriptor,
};

use crate::cli::List;
use crate::cmd::filter::StatusFilter;
use crate::display::{build_tree, format_aligned_rows, format_tree_lines, worktree_display_row};
use crate::json::worktree_to_json;

use super::Run;

impl Run for List {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let filter = StatusFilter::from(self);
        filter.validate()?;

        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;
        debug!("Found {} worktree(s)", worktrees.len());

        // Determine effective stack model
        let effective_model = if self.no_stack {
            workon::StackModel::None
        } else {
            WorkonConfig::new(&repo)?.stack_model(None)?
        };

        // Apply filters (AND logic)
        let filtered: Vec<_> = worktrees
            .into_iter()
            .filter(|wt| filter.matches(wt))
            .collect();
        debug!("{} worktree(s) after filtering", filtered.len());

        // Gather stack info per worktree when stack-active
        let stacks: Vec<Option<workon::Stack>> = filtered
            .iter()
            .map(|wt| {
                let branch = wt.branch().ok().flatten()?;
                current_stack(&repo, &branch, effective_model)
                    .ok()
                    .flatten()
            })
            .collect();

        // Partition into stack groups and ungrouped worktrees
        let mut grouping = group_by_stack(&stacks);

        // Add synthetic (member-less) groups for stacks present in metadata but not yet
        // represented by any worktree (e.g. stack branches with no checked-out worktree).
        // Skipped when any filter is active: metadata-only diffs have no worktree and can
        // never satisfy a worktree-status filter, so injecting them under a filter would
        // surface irrelevant stacks. See ADR-025.
        if effective_model != workon::StackModel::None && !filter.any_active() {
            let covered: std::collections::HashSet<(String, Vec<String>)> = grouping
                .groups
                .iter()
                .map(|g| {
                    let mut sorted = g.stack.diffs.clone();
                    sorted.sort();
                    (g.stack.trunk.clone(), sorted)
                })
                .collect();
            if let Ok(meta_stacks) = enumerate_stacks(&repo, effective_model) {
                for meta_stack in meta_stacks {
                    let mut sorted_diffs = meta_stack.diffs.clone();
                    sorted_diffs.sort();
                    if !covered.contains(&(meta_stack.trunk.clone(), sorted_diffs)) {
                        grouping.groups.push(workon::StackGroup {
                            stack: meta_stack,
                            members: vec![],
                        });
                    }
                }
            }
        }

        if self.json {
            let worktrees_json: Vec<_> = filtered.iter().map(worktree_to_json).collect();

            let wt_name =
                |idx: usize| -> String { filtered[idx].name().unwrap_or_default().to_string() };

            let stacks_json: Vec<_> = grouping
                .groups
                .iter()
                .map(|group| {
                    let checkouts: serde_json::Map<_, _> = group
                        .members
                        .iter()
                        .filter_map(|&idx| {
                            let diff = stacks[idx].as_ref().map(|s| s.current.clone())?;
                            Some((diff, json!(wt_name(idx))))
                        })
                        .collect();
                    let parents: serde_json::Map<_, _> = group
                        .stack
                        .parents
                        .iter()
                        .map(|(child, parent)| (child.clone(), json!(parent)))
                        .collect();
                    json!({
                        "trunk": group.stack.trunk,
                        "diffs": group.stack.diffs,
                        "parents": parents,
                        "checkouts": checkouts,
                    })
                })
                .collect();

            let output = serde_json::to_string_pretty(&json!({
                "worktrees": worktrees_json,
                "stacks": stacks_json,
            }))
            .into_diagnostic()?;
            println!("{}", output);
            return Ok(None);
        }

        let root = workon::workon_root(&repo)?;
        let current_dir = std::env::current_dir().into_diagnostic()?;

        if effective_model != workon::StackModel::None && !filter.any_active() {
            // Stack-active with no filter: render as unified tree
            let forest = build_tree(
                &filtered,
                &grouping.groups,
                &grouping.ungrouped,
                root,
                &current_dir,
            );
            let (lines, _) = format_tree_lines(&forest, true);
            for line in lines {
                println!("{}", line);
            }
        } else {
            // Non-stack, or any filter active: flat aligned list (pre-stack behavior, no headers)
            let rows: Vec<_> = filtered
                .iter()
                .filter_map(|wt| worktree_display_row(wt, root, &current_dir).ok())
                .collect();
            for line in format_aligned_rows(&rows, true) {
                println!("{}", line);
            }
        }

        Ok(None)
    }
}
