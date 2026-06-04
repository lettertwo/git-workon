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
//! When a stack model is active (e.g. Graphite), worktrees are grouped by their
//! connected stack and rendered as a tree rather than a flat list. Each stack is
//! rendered once, with a branch per line and an inline worktree reference on the
//! line of the branch checked out there. Trunk/untracked worktrees appear in a
//! trailing "Ungrouped" section. Passing `--no-stack` or using no stack model falls
//! back to the flat list (no headers, identical to the pre-stack behavior).
//!
//! ```text
//! Stack (trunk: main)
//!   feat-a-base
//! * feat-a-top      → ./user/feat-a   1h ago
//!
//! Ungrouped
//!   ./main                         2h ago
//! ```

use std::collections::HashMap;

use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use unicode_width::UnicodeWidthStr;
use workon::{
    current_stack, get_repo, get_worktrees, group_by_stack, WorkonConfig, WorktreeDescriptor,
};

use crate::cli::List;
use crate::display::{format_aligned_rows, worktree_display_row, WorktreeDisplayRow};
use crate::json::worktree_to_json;
use crate::output::style;

use super::Run;

impl Run for List {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        // Error if --dirty and --clean both specified
        if self.dirty && self.clean {
            return Err(miette::miette!(
                "Cannot specify both --dirty and --clean filters"
            ));
        }

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
            .filter(|wt| self.matches_filters(wt))
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
        if effective_model != workon::StackModel::None {
            let covered: std::collections::HashSet<(String, Vec<String>)> = grouping
                .groups
                .iter()
                .map(|g| {
                    let mut sorted = g.stack.diffs.clone();
                    sorted.sort();
                    (g.stack.trunk.clone(), sorted)
                })
                .collect();
            if let Ok(meta_stacks) = workon::enumerate_stacks(&repo, effective_model) {
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
                    json!({
                        "trunk": group.stack.trunk,
                        "diffs": group.stack.diffs,
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

        let mut need_blank = false;

        // Render each stack group as a branch tree with inline worktree references
        for group in &grouping.groups {
            if need_blank {
                println!();
            }
            need_blank = true;

            println!(
                "{}",
                style::dim(&format!("Stack (trunk: {})", group.stack.trunk))
            );

            // Map branch name → worktree index for members of this group
            let branch_to_idx: HashMap<&str, usize> = group
                .members
                .iter()
                .filter_map(|&idx| stacks[idx].as_ref().map(|s| (s.current.as_str(), idx)))
                .collect();

            // Compute per-group column widths for branch and dir columns
            let max_branch_width = group
                .stack
                .diffs
                .iter()
                .filter_map(|b| branch_to_idx.contains_key(b.as_str()).then(|| b.width()))
                .max()
                .unwrap_or(0);

            for branch in &group.stack.diffs {
                match branch_to_idx.get(branch.as_str()) {
                    None => {
                        // Branch exists in stack metadata but has no worktree
                        println!("  {}", style::dim(branch));
                    }
                    Some(&idx) => {
                        if let Ok(row) = worktree_display_row(&filtered[idx], root, &current_dir) {
                            let branch_pad = max_branch_width - branch.width();
                            let active_marker = if row.is_active {
                                style::green("→")
                            } else {
                                " ".to_string()
                            };
                            let indicators = format_indicators(&row);
                            let time = style::dim(&row.last_activity);
                            let suffix = if indicators.is_empty() {
                                format!("  {}", time)
                            } else {
                                format!("  {}  {}", indicators, time)
                            };
                            println!(
                                "* {}{}  {} {}{}{}",
                                style::bold(branch),
                                " ".repeat(branch_pad),
                                active_marker,
                                style::dim("./"),
                                row.dir_name,
                                suffix,
                            );
                        }
                    }
                }
            }
        }

        // Render ungrouped worktrees (trunk / untracked) as a flat aligned list
        if !grouping.ungrouped.is_empty() {
            if need_blank {
                println!();
            }

            // Only show the "Ungrouped" header when there are also stack groups above
            if !grouping.groups.is_empty() {
                println!("{}", style::dim("Ungrouped"));
            }

            let ungrouped_rows: Vec<WorktreeDisplayRow> = grouping
                .ungrouped
                .iter()
                .filter_map(|&idx| worktree_display_row(&filtered[idx], root, &current_dir).ok())
                .collect();
            for line in format_aligned_rows(&ungrouped_rows, true) {
                println!("{}", line);
            }
        }

        Ok(None)
    }
}

/// Format the status indicator symbols for a display row with appropriate colors.
fn format_indicators(row: &WorktreeDisplayRow) -> String {
    row.indicators
        .iter()
        .map(|ind| match ind.as_str() {
            "*" => style::yellow(ind),
            "↑" => style::green(ind),
            "↓" => style::red(ind),
            "✗" => style::red_bold(ind),
            _ => ind.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl List {
    /// Returns true if the worktree matches all active filters
    fn matches_filters(&self, wt: &WorktreeDescriptor) -> bool {
        // No filters = show all
        if !self.dirty && !self.clean && !self.ahead && !self.behind && !self.gone {
            return true;
        }

        // Check each filter - all must pass (AND logic)
        if self.dirty && !wt.is_dirty().unwrap_or(false) {
            return false;
        }

        if self.clean && wt.is_dirty().unwrap_or(true) {
            return false;
        }

        if self.ahead && !wt.has_unpushed_commits().unwrap_or(false) {
            return false;
        }

        if self.behind && !wt.is_behind_upstream().unwrap_or(false) {
            return false;
        }

        if self.gone && !wt.has_gone_upstream().unwrap_or(false) {
            return false;
        }

        true
    }
}
