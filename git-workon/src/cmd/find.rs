//! Find command with fuzzy matching and interactive selection.
//!
//! Finds worktrees using exact match, fuzzy matching, or interactive selection,
//! with integrated status filtering.
//!
//! ## Three-Mode Strategy
//!
//! 1. **Exact match**: If name matches exactly, return immediately
//! 2. **Single fuzzy match**: If name fuzzy-matches one worktree, return it
//! 3. **Interactive selection**: If multiple matches or no name provided, show interactive picker
//!
//! ## Fuzzy Matching Algorithm
//!
//! Simple case-insensitive substring matching:
//! - `feat` matches `feature`, `feat-branch`, `new-feature`
//! - `user/` matches `user/feature`, `user/bugfix`
//! - Exact matches take priority over fuzzy matches
//!
//! ## Status Filter Integration
//!
//! All status filters work in find:
//! ```bash
//! git workon find --dirty           # Find dirty worktrees
//! git workon find feat --ahead      # Find 'feat*' with unpushed commits
//! git workon find --clean --behind  # Interactive select from clean, behind worktrees
//! ```
//!
//! ## Interactive Mode
//!
//! Uses dialoguer's FuzzySelect widget with:
//! - Status indicators from display.rs (`*`, `↑`, `↓`, `✗`)
//! - Fuzzy searchable selection
//! - `--no-interactive` bypass for testing and scripting
//!
//! TODO: Add tests for interactive UI behavior (requires stdin mocking)

use dialoguer::FuzzySelect;
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Find;
use crate::display::format_worktree_items;

use super::Run;

impl Run for Find {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let mut worktrees = get_worktrees(&repo).wrap_err("Failed to list worktrees")?;

        // Apply status filters
        worktrees.retain(|wt| matches_filters(self, wt));

        if worktrees.is_empty() {
            bail!("No worktrees match the specified filters");
        }

        match &self.name {
            Some(name) => {
                // Try exact match first
                for (idx, worktree) in worktrees.iter().enumerate() {
                    if let Some(wt_name) = worktree.name() {
                        if wt_name == name {
                            // Return the worktree by consuming the vec
                            return Ok(Some(worktrees.into_iter().nth(idx).unwrap()));
                        }
                    }
                }

                // No exact match - try fuzzy matching (case-insensitive substring)
                let fuzzy_matches: Vec<_> = worktrees
                    .into_iter()
                    .enumerate()
                    .filter(|(_, wt)| {
                        if let Some(wt_name) = wt.name() {
                            wt_name.to_lowercase().contains(&name.to_lowercase())
                        } else {
                            false
                        }
                    })
                    .collect();

                match fuzzy_matches.len() {
                    0 => bail!("No matching worktree found for '{}'", name),
                    1 => {
                        let (_, worktree) = fuzzy_matches.into_iter().next().unwrap();
                        Ok(Some(worktree))
                    }
                    _ => {
                        if self.no_interactive {
                            bail!(
                                "Multiple worktrees match '{}'. Use full name or remove --no-interactive.",
                                name
                            );
                        }
                        // Extract just the worktrees from the (index, worktree) tuples
                        let matched_worktrees: Vec<WorktreeDescriptor> =
                            fuzzy_matches.into_iter().map(|(_, wt)| wt).collect();
                        select_from_list(matched_worktrees)
                    }
                }
            }
            None => {
                if self.no_interactive {
                    bail!("No worktree name provided. Specify a name or remove --no-interactive.");
                }
                select_from_list(worktrees)
            }
        }
    }
}

/// Returns true if the worktree matches all active filters
fn matches_filters(find: &Find, wt: &WorktreeDescriptor) -> bool {
    if !find.dirty && !find.clean && !find.ahead && !find.behind && !find.gone {
        return true;
    }

    if find.dirty && !wt.is_dirty().unwrap_or(false) {
        return false;
    }
    if find.clean && wt.is_dirty().unwrap_or(true) {
        return false;
    }
    if find.ahead && !wt.has_unpushed_commits().unwrap_or(false) {
        return false;
    }
    if find.behind && !wt.is_behind_upstream().unwrap_or(false) {
        return false;
    }
    if find.gone && !wt.has_gone_upstream().unwrap_or(false) {
        return false;
    }

    true
}

/// Show interactive fuzzy selection list
fn select_from_list(worktrees: Vec<WorktreeDescriptor>) -> Result<Option<WorktreeDescriptor>> {
    let items = format_worktree_items(&worktrees);

    let selection = FuzzySelect::new()
        .with_prompt("Select a worktree")
        .items(&items)
        .default(0)
        .interact()
        .into_diagnostic()
        .wrap_err("Failed to show interactive selection")?;

    // Consume the vec and return the selected worktree
    Ok(Some(worktrees.into_iter().nth(selection).unwrap()))
}
