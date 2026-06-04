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
//! Uses a custom hybrid picker that keeps items in stable tree order while still
//! filtering as you type. When a query is active:
//! - Non-matching items are hidden, except ancestors of a match (so connectors
//!   always point to a real visible parent — no orphaned `├─`/`└─` glyphs).
//! - The cursor jumps to the best-scoring match; arrow keys navigate freely.
//! - Matched characters are underlined; non-matched characters are dimmed.
//!
//! When stack-active, uses the graphite-style tree view (`◉`/`◯`). Selecting a
//! `◯` diff that belongs to a stack with no worktree routes to `New` to create/attach one.
//!
//! Pass `--no-interactive` to bypass interactive selection for scripting/testing.

use fuzzy_matcher::skim::SkimMatcherV2;
use log::debug;
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use workon::{
    current_stack, enumerate_stacks, get_repo, get_worktrees, group_by_stack, WorkonConfig,
    WorktreeDescriptor,
};

use crate::cli::{Find, New};
use crate::display::{build_tree, render_flat, render_tree, worktree_display_row};
use crate::picker;

use super::Run;

impl Run for Find {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None).wrap_err("Failed to find git repository")?;
        let mut worktrees = get_worktrees(&repo).wrap_err("Failed to list worktrees")?;

        // Effective stack model (for the third match path below)
        let effective_model = if self.no_stack {
            workon::StackModel::None
        } else {
            WorkonConfig::new(&repo)?.stack_model(None)?
        };

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
                    0 => {
                        // Third path (stack-active): fuzzy match on branch names in stacks
                        if effective_model != workon::StackModel::None {
                            debug!("No fuzzy match, searching stacks for '{}'", name);
                            let name_lower = name.to_lowercase();
                            let all = get_worktrees(&repo)?;
                            let stack_matches: Vec<WorktreeDescriptor> = all
                                .into_iter()
                                .filter(|wt| {
                                    let head = match wt.branch().ok().flatten() {
                                        Some(b) => b,
                                        None => return false,
                                    };
                                    matches!(
                                        current_stack(&repo, &head, effective_model),
                                        Ok(Some(ref s)) if s.diffs.iter().any(|b| {
                                            b.to_lowercase().contains(&name_lower)
                                        })
                                    )
                                })
                                .collect();

                            match stack_matches.len() {
                                0 => {}
                                1 => {
                                    let wt = stack_matches.into_iter().next().unwrap();
                                    debug!(
                                        "Found '{}' in stack of '{}'",
                                        name,
                                        wt.name().unwrap_or("?")
                                    );
                                    return Ok(Some(wt));
                                }
                                _ => {
                                    if self.no_interactive {
                                        bail!(
                                            "Multiple stacks contain branches matching '{}'. Use full branch name or remove --no-interactive.",
                                            name
                                        );
                                    }
                                    return select_from_tree(
                                        self,
                                        stack_matches,
                                        effective_model,
                                        &repo,
                                    );
                                }
                            }
                        }
                        bail!("No matching worktree found for '{}'", name)
                    }
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
                        let matched_worktrees: Vec<WorktreeDescriptor> =
                            fuzzy_matches.into_iter().map(|(_, wt)| wt).collect();
                        select_from_tree(self, matched_worktrees, effective_model, &repo)
                    }
                }
            }
            None => {
                if self.no_interactive {
                    bail!("No worktree name provided. Specify a name or remove --no-interactive.");
                }
                select_from_tree(self, worktrees, effective_model, &repo)
            }
        }
    }
}

/// Returns true if the worktree matches all active filters.
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

/// Show interactive selection.
///
/// Uses the hybrid picker: items stay in stable order; typing filters to
/// matching items (keeping ancestors of matches so connectors stay valid in the
/// tree view) and jumps the cursor to the best-scoring match.
///
/// When stack-active, displays the unified tree view (same as `list`). Selecting a
/// `◯` diff with no worktree in its stack routes to `New` to create/attach one.
/// Falls back to the flat picker when `--no-stack` is set.
fn select_from_tree(
    find: &Find,
    worktrees: Vec<WorktreeDescriptor>,
    effective_model: workon::StackModel,
    repo: &git2::Repository,
) -> Result<Option<WorktreeDescriptor>> {
    let root = workon::workon_root(repo)?;
    let current_dir = std::env::current_dir().into_diagnostic()?;
    let matcher = SkimMatcherV2::default();

    if effective_model != workon::StackModel::None {
        // ── Stack-active: unified tree picker ────────────────────────────────
        let stacks: Vec<Option<workon::Stack>> = worktrees
            .iter()
            .map(|wt| {
                let branch = wt.branch().ok().flatten()?;
                current_stack(repo, &branch, effective_model).ok().flatten()
            })
            .collect();

        let mut grouping = group_by_stack(&stacks);

        // Surface metadata-only stacks (same logic as list).
        let covered: std::collections::HashSet<(String, Vec<String>)> = grouping
            .groups
            .iter()
            .map(|g| {
                let mut sorted = g.stack.diffs.clone();
                sorted.sort();
                (g.stack.trunk.clone(), sorted)
            })
            .collect();
        if let Ok(meta_stacks) = enumerate_stacks(repo, effective_model) {
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

        let forest = build_tree(
            &worktrees,
            &grouping.groups,
            &grouping.ungrouped,
            root,
            &current_dir,
        );

        let selected_branch =
            match picker::select("Select a worktree", |q| render_tree(&forest, q, &matcher))
                .wrap_err("Failed to show interactive selection")?
            {
                Some(key) => key,
                None => return Ok(None),
            };

        // Resolve: find the worktree whose HEAD matches the selected branch.
        if let Some(idx) = worktrees
            .iter()
            .position(|wt| wt.branch().ok().flatten().as_deref() == Some(&selected_branch))
        {
            return Ok(Some(worktrees.into_iter().nth(idx).unwrap()));
        }

        // No direct match — selected_branch is a ◯ metadata-only diff.
        // If another worktree's stack contains it, return that worktree (granularity=Stack).
        for (idx, stack_opt) in stacks.iter().enumerate() {
            if let Some(stack) = stack_opt {
                if stack.diffs.contains(&selected_branch) {
                    return Ok(Some(worktrees.into_iter().nth(idx).unwrap()));
                }
            }
        }

        // Stack has no worktree — route to New to create/attach one.
        debug!("No worktree for diff '{}'; routing to new", selected_branch);
        let new_cmd = New {
            no_stack: find.no_stack,
            name: Some(selected_branch),
            base: None,
            branch: None,
            orphan: false,
            detach: false,
            no_hooks: false,
            copy: false,
            no_copy: false,
            no_copy_ignored: false,
            no_interactive: find.no_interactive,
            lock: false,
        };
        return new_cmd.run();
    }

    // ── Non-stack: flat picker ────────────────────────────────────────────────
    let rows: Vec<_> = worktrees
        .iter()
        .filter_map(|wt| worktree_display_row(wt, root, &current_dir).ok())
        .collect();

    let selected_key =
        match picker::select("Select a worktree", |q| render_flat(&rows, q, &matcher))
            .wrap_err("Failed to show interactive selection")?
        {
            Some(key) => key,
            None => return Ok(None),
        };

    // Map the selected dir_name key back to a worktree.
    let idx = rows
        .iter()
        .position(|r| r.dir_name == selected_key)
        .unwrap_or(0);
    Ok(Some(worktrees.into_iter().nth(idx).unwrap()))
}
