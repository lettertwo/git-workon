//! Worktree display formatting with status indicators.
//!
//! This module provides consistent formatting for displaying worktrees with visual
//! status indicators, used in interactive modes and list output.
//!
//! ## Status Indicators
//!
//! Each indicator shows a specific worktree state:
//! - `*` (asterisk) - Worktree has uncommitted changes (dirty)
//! - `↑` (up arrow) - Worktree has unpushed commits (ahead of upstream)
//! - `↓` (down arrow) - Worktree is behind upstream
//! - `✗` (cross mark) - Upstream branch has been deleted (gone)
//!
//! Multiple indicators can appear together, e.g., `feature * ↑` indicates a dirty worktree
//! with unpushed commits.
//!
//! ## Display Format
//!
//! The basic format is: `branch-name [indicators...]`
//!
//! Examples:
//! - `main` - Clean worktree, up to date
//! - `feature *` - Dirty worktree
//! - `bugfix ↑` - Has unpushed commits
//! - `experiment * ↑ ↓` - Dirty, has unpushed commits, and is behind upstream
//! - `old-feature ✗` - Upstream branch deleted
//! - `(detached HEAD)` - Detached HEAD state
//!
//! ## Usage in Interactive Modes
//!
//! This formatting is used by:
//! - `git workon find` (interactive selection)
//! - `git workon new` (base branch selection)
//! - Future interactive features
//!
//! The indicators help users quickly identify worktree state when selecting from lists.

use miette::Result;
use workon::WorktreeDescriptor;

use crate::output::style;

/// Format a worktree for display with status indicators
/// Format: "branch-name * ↑ ↓" (shows indicators only when applicable)
pub fn format_worktree_item(wt: &WorktreeDescriptor) -> Result<String> {
    let branch_name = match wt.branch()? {
        Some(name) => name,
        None => "(detached HEAD)".to_string(),
    };

    let mut indicators: Vec<String> = Vec::new();

    // * = dirty
    if wt.is_dirty().unwrap_or(false) {
        indicators.push(style::yellow("*"));
    }

    // ↑ = ahead (unpushed commits)
    if wt.has_unpushed_commits().unwrap_or(false) {
        indicators.push(style::green("↑"));
    }

    // ↓ = behind upstream
    if wt.is_behind_upstream().unwrap_or(false) {
        indicators.push(style::red("↓"));
    }

    // ✗ = upstream gone
    if wt.has_gone_upstream().unwrap_or(false) {
        indicators.push(style::red_bold("✗"));
    }

    if indicators.is_empty() {
        Ok(style::bold(&branch_name))
    } else {
        Ok(format!(
            "{} {}",
            style::bold(&branch_name),
            indicators.join(" ")
        ))
    }
}

/// Format a list of worktrees for display
pub fn format_worktree_items(worktrees: &[WorktreeDescriptor]) -> Vec<String> {
    worktrees
        .iter()
        .map(|wt| {
            format_worktree_item(wt).unwrap_or_else(|_| {
                wt.name()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(unknown)".to_string())
            })
        })
        .collect()
}
