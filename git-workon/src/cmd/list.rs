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

use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use workon::{current_stack, get_repo, get_worktrees, WorkonConfig, WorktreeDescriptor};

use crate::cli::List;
use crate::display::{format_aligned_rows, worktree_display_row};
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

        if self.json {
            let json_array: Vec<_> = filtered
                .iter()
                .zip(stacks.iter())
                .map(|(wt, stack)| {
                    let mut obj = worktree_to_json(wt);
                    if let Some(s) = stack {
                        obj["stack"] = json!({
                            "trunk": s.trunk,
                            "branches": s.branches,
                            "current": s.current,
                        });
                    }
                    obj
                })
                .collect();
            let output = serde_json::to_string_pretty(&json_array).into_diagnostic()?;
            println!("{}", output);
            return Ok(None);
        }

        let root = workon::workon_root(&repo)?;
        let current_dir = std::env::current_dir().into_diagnostic()?;

        let rows: Vec<_> = filtered
            .iter()
            .filter_map(|wt| worktree_display_row(wt, root, &current_dir).ok())
            .collect();

        for (line, stack) in format_aligned_rows(&rows, true).iter().zip(stacks.iter()) {
            println!("{}", line);
            if let Some(s) = stack {
                for branch in &s.branches {
                    let marker = if *branch == s.current { "*" } else { " " };
                    println!("    {} {}", marker, style::dim(branch));
                }
            }
        }

        Ok(None)
    }
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
