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

use dialoguer::console::{style, Style};
use dialoguer::theme::ColorfulTheme;
use dialoguer::FuzzySelect;
use log::debug;
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use workon::{get_repo, get_worktrees, WorktreeDescriptor};

use crate::cli::Find;
use crate::display::{format_aligned_rows, worktree_display_row};

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
                debug!("Searching for worktree '{}'", name);

                // Try exact match first
                for (idx, worktree) in worktrees.iter().enumerate() {
                    if let Some(wt_name) = worktree.name() {
                        if wt_name == name {
                            debug!("Found exact match: {}", wt_name);
                            // Return the worktree by consuming the vec
                            return Ok(Some(worktrees.into_iter().nth(idx).unwrap()));
                        }
                    }
                }

                // No exact match - try fuzzy matching (case-insensitive substring)
                debug!("No exact match, trying fuzzy match");
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

                debug!("Found {} fuzzy match(es)", fuzzy_matches.len());

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
    let repo = get_repo(None)?;
    let root = workon::workon_root(&repo)?;
    let current_dir = std::env::current_dir().into_diagnostic()?;

    let rows: Vec<_> = worktrees
        .iter()
        .filter_map(|wt| worktree_display_row(wt, root, &current_dir).ok())
        .collect();
    let active_index = rows.iter().position(|r| r.is_active).unwrap_or(0);
    let items = format_aligned_rows(&rows, false);

    let theme = ColorfulTheme {
        active_item_prefix: style("→".to_string()).for_stderr().green(),
        active_item_style: Style::new().for_stderr(),
        inactive_item_style: Style::new().for_stderr(),
        fuzzy_match_highlight_style: Style::new().for_stderr().underlined(),
        ..ColorfulTheme::default()
    };

    let selection = FuzzySelect::with_theme(&theme)
        .with_prompt("Select a worktree")
        .items(&items)
        .default(active_index)
        .interact()
        .into_diagnostic()
        .wrap_err("Failed to show interactive selection")?;

    // Consume the vec and return the selected worktree
    Ok(Some(worktrees.into_iter().nth(selection).unwrap()))
}
