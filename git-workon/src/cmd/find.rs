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
//! Skim fuzzy scoring on both the worktree directory name and the checked-out branch name.
//! The best score across both fields determines visibility and ranking.
//! - `feat` matches `feature`, `feat-branch`, `new-feature` (dir or branch)
//! - `shared` matches a branch named `shared` even when the dir is named `base`
//! - Exact dir-name matches take priority over fuzzy matches
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
//! When stack-active, uses the graphite-style tree view (`◉`/`◎`/`◯`):
//! - `◉` green+bold — the active (current-directory) worktree
//! - `◎` bold — a worktree exists but is not current
//! - `◯` dim — metadata-only diff (no worktree)
//!
//! The picker cursor uses a cyan `▶` pointer so it is visually distinct from the
//! green active-worktree marker. Selecting a `◯` diff with no worktree routes to
//! `New` to create/attach one.
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
use crate::picker::{self, PickerAction};

use super::Run;

impl Run for Find {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        // --new bypasses resolution entirely: force a fresh worktree, the same
        // escape hatch the bare-name routing and the picker's Tab action use.
        if self.new {
            let Some(name) = &self.name else {
                bail!("--new requires a worktree name");
            };
            let mut new_cmd = New::attach(name.clone());
            new_cmd.no_stack = self.no_stack;
            new_cmd.no_interactive = self.no_interactive;
            return new_cmd.run();
        }

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

                use fuzzy_matcher::FuzzyMatcher;
                let matcher = SkimMatcherV2::default();

                // Pre-collect (dir_name, branch_name) for all worktrees once.
                // branch() reads HEAD from disk on every call, so we gather it up front.
                let fields: Vec<(Option<String>, Option<String>)> = worktrees
                    .iter()
                    .map(|wt| (wt.name().map(|n| n.to_string()), wt.branch().ok().flatten()))
                    .collect();

                // 1. Exact match on directory name (highest priority, unchanged).
                if let Some(idx) = fields
                    .iter()
                    .position(|(dir, _)| dir.as_deref() == Some(name.as_str()))
                {
                    debug!("Found exact match: {}", name);
                    return Ok(Some(worktrees.into_iter().nth(idx).unwrap()));
                }

                // 2. Skim fuzzy match on both dir name and branch name; rank by best score.
                debug!("No exact match, trying Skim fuzzy match on dir + branch");
                let mut scored: Vec<(i64, usize)> = fields
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (dir, branch))| {
                        let dir_score = dir.as_deref().and_then(|d| matcher.fuzzy_match(d, name));
                        let branch_score =
                            branch.as_deref().and_then(|b| matcher.fuzzy_match(b, name));
                        [dir_score, branch_score]
                            .into_iter()
                            .flatten()
                            .max()
                            .map(|s| (s, i))
                    })
                    .collect();
                scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
                debug!("Found {} Skim fuzzy match(es)", scored.len());

                // 3. Stack-member fallback (stack-active only, when paths 1+2 find nothing).
                //
                // When a branch exists only in stack metadata (no worktree), navigating to
                // the worktree that owns the containing stack is the expected behavior.
                // Uses Skim fuzzy (same as path 2) so case-sensitivity and scoring are uniform.
                let is_stack_fallback =
                    scored.is_empty() && effective_model != workon::StackModel::None;
                let stack_indices: Vec<usize> = if is_stack_fallback {
                    debug!("No Skim match, searching stack members for '{}'", name);
                    fields
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, branch))| {
                            let Some(head) = branch.as_deref() else {
                                return false;
                            };
                            matches!(
                                current_stack(&repo, head, effective_model),
                                Ok(Some(ref s)) if s.diffs.iter().any(|b| {
                                    matcher.fuzzy_match(b, name).is_some()
                                })
                            )
                        })
                        .map(|(i, _)| i)
                        .collect()
                } else {
                    vec![]
                };

                let match_indices: Vec<usize> = if is_stack_fallback {
                    stack_indices
                } else {
                    scored.into_iter().map(|(_, i)| i).collect()
                };

                match match_indices.len() {
                    0 => bail!("No matching worktree found for '{}'", name),
                    1 => Ok(Some(worktrees.into_iter().nth(match_indices[0]).unwrap())),
                    _ => {
                        if self.no_interactive {
                            if is_stack_fallback {
                                bail!(
                                    "Multiple stacks contain branches matching '{}'. Use full branch name or remove --no-interactive.",
                                    name
                                );
                            } else {
                                bail!(
                                    "Multiple worktrees match '{}'. Use full name or remove --no-interactive.",
                                    name
                                );
                            }
                        }
                        let matched: Vec<WorktreeDescriptor> = worktrees
                            .into_iter()
                            .enumerate()
                            .filter(|(i, _)| match_indices.contains(i))
                            .map(|(_, wt)| wt)
                            .collect();
                        select_from_tree(self, matched, effective_model, &repo)
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
/// metadata-only `◯` diff with no worktree routes to `New` to create/attach one.
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

        let pick = picker::select("Select a worktree", |q| render_tree(&forest, q, &matcher))
            .wrap_err("Failed to show interactive selection")?;

        let (selected_branch, action) = match pick {
            None => return Ok(None),
            Some((branch, PickerAction::Materialize)) => {
                // Tab: bypass resolution, force a fresh worktree.
                debug!("Tab: force-materializing '{}'", branch);
                let mut new_cmd = New::attach(branch);
                new_cmd.no_stack = find.no_stack;
                new_cmd.no_interactive = find.no_interactive;
                return new_cmd.run();
            }
            Some((branch, action)) => (branch, action),
        };
        let _ = action; // Resolve is the only remaining variant; no extra branching needed.

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
        let mut new_cmd = New::attach(selected_branch);
        new_cmd.no_stack = find.no_stack;
        new_cmd.no_interactive = find.no_interactive;
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
            Some((key, _action)) => key, // Tab and Enter are equivalent in the flat picker
            None => return Ok(None),
        };

    // Map the selected dir_name key back to a worktree.
    let idx = rows
        .iter()
        .position(|r| r.dir_name == selected_key)
        .unwrap_or(0);
    Ok(Some(worktrees.into_iter().nth(idx).unwrap()))
}
