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
