//! Shared status-filter logic for commands that accept `--dirty/--clean/--ahead/--behind/--gone`.
//!
//! The five filter flags are defined on both [`crate::cli::List`] and [`crate::cli::Find`]
//! (and surfaced on the bare default command through the flattened `Find`). This module
//! centralises the "any filter active?", validation, and per-worktree matching logic so it
//! does not have to be duplicated in each command file.
//!
//! ## Semantics
//!
//! Filters select **worktrees** by status. A metadata-only stack diff (`◯`) has no working
//! tree and therefore no definable status — it can never satisfy a worktree-status filter.
//! When any filter is active the stack tree is abandoned and the command produces a flat list
//! of matching worktrees only. See ADR-025.

use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::{Find, List};

/// Captures the five status-filter flags from a `List` or `Find` command.
pub struct StatusFilter {
    dirty: bool,
    clean: bool,
    ahead: bool,
    behind: bool,
    gone: bool,
}

impl StatusFilter {
    /// Returns `true` when at least one filter flag is active.
    pub fn any_active(&self) -> bool {
        self.dirty || self.clean || self.ahead || self.behind || self.gone
    }

    /// Returns an error if `--dirty` and `--clean` are both specified.
    pub fn validate(&self) -> Result<()> {
        if self.dirty && self.clean {
            miette::bail!("Cannot specify both --dirty and --clean filters");
        }
        Ok(())
    }

    /// Returns `true` if the worktree satisfies all active filters (AND logic).
    ///
    /// When no filter is active every worktree matches.
    ///
    /// Error handling uses `.unwrap_or(false)` for positive checks and `.unwrap_or(true)`
    /// for negative checks, so a status-read failure causes the worktree to be excluded from
    /// both `--dirty` and `--clean` results (conservative/fail-safe behaviour).
    pub fn matches(&self, wt: &WorktreeDescriptor) -> bool {
        if !self.any_active() {
            return true;
        }

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

impl From<&List> for StatusFilter {
    fn from(cmd: &List) -> Self {
        StatusFilter {
            dirty: cmd.dirty,
            clean: cmd.clean,
            ahead: cmd.ahead,
            behind: cmd.behind,
            gone: cmd.gone,
        }
    }
}

impl From<&Find> for StatusFilter {
    fn from(cmd: &Find) -> Self {
        StatusFilter {
            dirty: cmd.dirty,
            clean: cmd.clean,
            ahead: cmd.ahead,
            behind: cmd.behind,
            gone: cmd.gone,
        }
    }
}
