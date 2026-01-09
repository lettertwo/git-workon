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
//! TODO: Add --json output option for programmatic use
//! TODO: Optimize status checks for performance with many worktrees

use miette::Result;
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::List;

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

        // Apply filters (AND logic)
        let filtered: Vec<_> = worktrees
            .into_iter()
            .filter(|wt| self.matches_filters(wt))
            .collect();

        for worktree in &filtered {
            println!("{}", worktree);
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
