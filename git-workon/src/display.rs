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
//! ## Display Formats
//!
//! ### Flat (non-stack) format
//!
//! Column-aligned output: active marker, dimmed `./` + bold directory name, colored status
//! indicators, dimmed last activity, and an optional dimmed branch name at the end (when the
//! checked-out branch differs from the directory name).
//!
//! ```text
//!   ./main                    2 hours ago
//! → ./feature-auth  *         3 days ago
//!   ./my-feature    ↑         1 hour ago  my-feat-pt2
//! ```
//!
//! ### Tree (stack-active) format
//!
//! Graphite-style connector tree: `◉` for diffs with a checked-out worktree, `◯` for
//! metadata-only diffs. `├─` / `└─` at fork points; linear chains use `│` continuation
//! without extra indent. `← here` marks the current worktree.
//!
//! ```text
//! ◉ main             ↑   2m ago  ← here
//! ├─◯ api-1
//! │ ◉ api-2          ↑   2h ago
//! │ ◯ api-3
//! └─◯ shared  ./base     5d ago
//!   ├─◯ branch-x
//!   └─◯ branch-y
//! ◉ ee/testing           1mo ago
//! ```
//!
//! Used by `list` for output and `find` for interactive selection.

use std::collections::HashMap;
use std::path::Path;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use miette::Result;
use unicode_width::UnicodeWidthStr;
use workon::WorktreeDescriptor;

use crate::output::style;

/// Glyph for a diff/branch that has a checked-out worktree.
pub const GLYPH_WORKTREE: &str = "◉";
/// Glyph for a diff/branch that exists only in stack metadata (no worktree).
pub const GLYPH_METADATA: &str = "◯";

// ── Flat row (shared between non-stack list and find) ────────────────────────

/// Structured data for one row of the aligned worktree list.
pub struct WorktreeDisplayRow {
    pub is_active: bool,
    /// The directory name relative to the workon root (e.g., `my-feature` or `user/feature`).
    pub dir_name: String,
    /// Branch name shown (dimmed) when the checked-out branch differs from the directory name,
    /// or `(detached HEAD)` when HEAD is detached.
    pub branch_annotation: Option<String>,
    pub indicators: Vec<String>,
    pub last_activity: String,
    /// Raw epoch seconds for activity-based sorting (None if unavailable).
    pub activity_epoch: Option<i64>,
}

/// Build a display row from a worktree descriptor.
pub fn worktree_display_row(
    wt: &WorktreeDescriptor,
    root: &Path,
    current_dir: &Path,
) -> Result<WorktreeDisplayRow> {
    let is_active = current_dir.starts_with(wt.path());

    let dir_name = pathdiff::diff_paths(wt.path(), root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| wt.path().display().to_string());

    let branch_annotation = match wt.branch()? {
        Some(branch) if branch == dir_name => None,
        Some(branch) => Some(branch),
        None => Some("(detached HEAD)".to_string()),
    };

    let indicators = collect_indicators(wt);

    let activity_epoch = wt.last_activity().ok().flatten();
    let last_activity = activity_epoch.map(format_relative_time).unwrap_or_default();

    Ok(WorktreeDisplayRow {
        is_active,
        dir_name,
        branch_annotation,
        indicators,
        last_activity,
        activity_epoch,
    })
}

/// Collect the status indicator symbols for a worktree.
///
/// Shared by both the flat and tree renderers to avoid duplication.
pub fn collect_indicators(wt: &WorktreeDescriptor) -> Vec<String> {
    let mut indicators: Vec<String> = Vec::new();
    if wt.is_dirty().unwrap_or(false) {
        indicators.push("*".to_string());
    }
    if wt.has_unpushed_commits().unwrap_or(false) {
        indicators.push("↑".to_string());
    }
    if wt.is_behind_upstream().unwrap_or(false) {
        indicators.push("↓".to_string());
    }
    if wt.has_gone_upstream().unwrap_or(false) {
        indicators.push("✗".to_string());
    }
    indicators
}

/// Color and join the raw indicator symbols from [`collect_indicators`].
pub fn format_indicators(indicators: &[String]) -> String {
    indicators
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

/// Format display rows into column-aligned strings.
///
/// When `show_active_marker` is true, rows are prefixed with `→` for the active
/// worktree (used by `list`). When false, the marker column is omitted (used by
/// interactive selection where the cursor serves as the active indicator).
pub fn format_aligned_rows(rows: &[WorktreeDisplayRow], show_active_marker: bool) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }

    let max_name = rows.iter().map(|r| r.dir_name.width()).max().unwrap_or(0);

    let indicator_widths: Vec<usize> = rows
        .iter()
        .map(|r| r.indicators.join(" ").width())
        .collect();
    let max_indicators = indicator_widths.iter().copied().max().unwrap_or(0);

    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let prefix = style::dim("./");
            let name = style::bold(&row.dir_name);
            let name_pad = max_name - row.dir_name.width();

            let indicators_display = format_indicators(&row.indicators);
            let indicators_pad = max_indicators - indicator_widths[i];

            let activity = style::dim(&row.last_activity);

            let branch = row
                .branch_annotation
                .as_deref()
                .map(|ann| format!("  {}", style::dim(ann)))
                .unwrap_or_default();

            if show_active_marker {
                let marker = if row.is_active {
                    style::green("→")
                } else {
                    " ".to_string()
                };
                format!(
                    "{} {}{}{} {}{}  {}{}",
                    marker,
                    prefix,
                    name,
                    " ".repeat(name_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    activity,
                    branch,
                )
            } else {
                format!(
                    "{}{}{} {}{}  {}{}",
                    prefix,
                    name,
                    " ".repeat(name_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    activity,
                    branch,
                )
            }
        })
        .collect()
}

// ── Tree renderer ─────────────────────────────────────────────────────────────

/// A single node in the stack display tree.
///
/// Roots are trunks (or standalone untracked worktrees). Children are the diffs
/// stacked on that trunk (or on another diff), ordered by most-recent activity
/// descending after the dependency chain is resolved.
pub struct TreeNode {
    /// The branch/diff name — used as the primary label.
    pub branch: String,
    /// Worktree display data. `None` for metadata-only diffs (no checked-out worktree).
    pub row: Option<WorktreeDisplayRow>,
    /// Most-recent activity in this node's subtree (epoch seconds), for sort ordering.
    pub subtree_activity: Option<i64>,
    /// Children (diffs stacked on this branch), in display order.
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Whether this node has a checked-out worktree.
    pub fn has_worktree(&self) -> bool {
        self.row.is_some()
    }
}

/// Build a forest of [`TreeNode`]s from worktrees, their stacks, and the grouping.
///
/// Each distinct trunk gets one root node; all stacks on that trunk hang underneath.
/// Untracked worktrees (those in `ungrouped`) become additional root-level leaf nodes,
/// interleaved with the trunk roots by most-recent activity.
///
/// `all_worktrees` is the full filtered slice; `stacks` is parallel to it (one `Option<Stack>`
/// per worktree). `groups` includes both real and metadata-only stack groups.
pub fn build_tree(
    all_worktrees: &[WorktreeDescriptor],
    groups: &[workon::StackGroup],
    ungrouped: &[usize],
    root: &Path,
    current_dir: &Path,
) -> Vec<TreeNode> {
    // Map branch name → worktree index; build per-worktree display rows.
    let mut branch_to_idx: HashMap<String, usize> = HashMap::new();
    let mut rows: HashMap<usize, WorktreeDisplayRow> = HashMap::new();
    for (idx, wt) in all_worktrees.iter().enumerate() {
        if let Some(branch) = wt.branch().ok().flatten() {
            branch_to_idx.entry(branch).or_insert(idx);
        }
        if let Ok(row) = worktree_display_row(wt, root, current_dir) {
            rows.insert(idx, row);
        }
    }

    // Per-trunk reverse map: parent_branch → Vec<child_branch>
    let mut per_trunk_reverse: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for group in groups {
        let rev = per_trunk_reverse
            .entry(group.stack.trunk.clone())
            .or_default();
        for (child, parent) in &group.stack.parents {
            rev.entry(parent.clone()).or_default().push(child.clone());
        }
    }
    // Sort children for determinism
    for rev_map in per_trunk_reverse.values_mut() {
        for children_list in rev_map.values_mut() {
            children_list.sort();
        }
    }

    // Build trunk root nodes.
    // Collect all distinct trunks.
    let mut trunk_set: Vec<String> = groups
        .iter()
        .map(|g| g.stack.trunk.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    trunk_set.sort();

    let mut forest: Vec<TreeNode> = Vec::new();

    for trunk in &trunk_set {
        let trunk_idx = branch_to_idx.get(trunk).copied();
        let trunk_row = trunk_idx.and_then(|idx| rows.remove(&idx));
        let trunk_activity = trunk_row.as_ref().and_then(|r| r.activity_epoch);

        let empty_rev: HashMap<String, Vec<String>> = HashMap::new();
        let rev_map = per_trunk_reverse.get(trunk).unwrap_or(&empty_rev);

        // Direct children of the trunk: branches whose parent == trunk
        let direct_children = rev_map.get(trunk.as_str()).cloned().unwrap_or_default();
        let children = build_children(&direct_children, rev_map, &branch_to_idx, &mut rows);

        let subtree_activity = subtree_max(trunk_activity, &children);

        forest.push(TreeNode {
            branch: trunk.clone(),
            row: trunk_row,
            subtree_activity,
            children,
        });
    }

    // Add ungrouped (untracked) worktrees as leaf nodes.
    // If a trunk's worktree is in ungrouped, it was already used above; skip it.
    let trunk_set_ref: std::collections::HashSet<&String> = trunk_set.iter().collect();
    for &idx in ungrouped {
        // Skip if this worktree's branch is a known trunk (already in forest as root).
        let branch = all_worktrees[idx].branch().ok().flatten();
        if branch
            .as_ref()
            .map(|b| trunk_set_ref.contains(b))
            .unwrap_or(false)
        {
            continue;
        }
        // Skip if this worktree was already placed in a trunk node.
        if !rows.contains_key(&idx) {
            continue;
        }
        let row = rows.remove(&idx).unwrap();
        let epoch = row.activity_epoch;
        forest.push(TreeNode {
            branch: branch.clone().unwrap_or_else(|| row.dir_name.clone()),
            subtree_activity: epoch,
            row: Some(row),
            children: vec![],
        });
    }

    // Sort roots by most-recent subtree_activity descending (None last).
    forest.sort_by(|a, b| b.subtree_activity.cmp(&a.subtree_activity));

    forest
}

/// Recursively build child nodes for a list of branch names.
fn build_children(
    branch_names: &[String],
    rev_map: &HashMap<String, Vec<String>>,
    branch_to_idx: &HashMap<String, usize>,
    rows: &mut HashMap<usize, WorktreeDisplayRow>,
) -> Vec<TreeNode> {
    branch_names
        .iter()
        .map(|branch| {
            let idx = branch_to_idx.get(branch).copied();
            let row = idx.and_then(|i| rows.remove(&i));
            let epoch = row.as_ref().and_then(|r| r.activity_epoch);

            let grandchildren_names = rev_map.get(branch.as_str()).cloned().unwrap_or_default();
            let children = build_children(&grandchildren_names, rev_map, branch_to_idx, rows);
            let subtree_activity = subtree_max(epoch, &children);

            TreeNode {
                branch: branch.clone(),
                row,
                subtree_activity,
                children,
            }
        })
        .collect()
}

/// Return the most recent activity among a node's own epoch and its children's subtree_activity.
fn subtree_max(own: Option<i64>, children: &[TreeNode]) -> Option<i64> {
    let child_max = children.iter().filter_map(|c| c.subtree_activity).max();
    match (own, child_max) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Render a forest of [`TreeNode`]s into display lines (and, optionally, selection metadata).
///
/// Returns `(display_lines, selection_map)` where `selection_map[i]` maps the i-th display
/// line to the branch name that would be selected. The caller resolves the branch to a
/// worktree (or routes to New).
///
/// `show_here_marker`: when true, append dim `← here` to the active worktree's row (used by
/// `list`). When false, the cursor in the interactive picker serves that role.
pub fn format_tree_lines(
    forest: &[TreeNode],
    show_here_marker: bool,
) -> (Vec<String>, Vec<String>) {
    if forest.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // First pass: collect all (prefix, connector, node) tuples in display order.
    // This lets us compute the max content width for column alignment.
    let flat = flatten_tree(forest);

    // Compute max width of the "prefix+connector+glyph+label[+path]" portion.
    let max_content_w = flat
        .iter()
        .map(|(prefix, connector, node)| content_width(prefix, connector, node))
        .max()
        .unwrap_or(0);

    // Max indicator width across all rows that have worktrees.
    let max_ind_w = flat
        .iter()
        .filter_map(|(_, _, node)| node.row.as_ref().map(|r| r.indicators.join(" ").width()))
        .max()
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    let mut selection: Vec<String> = Vec::new();

    for (prefix, connector, node) in &flat {
        let glyph: String = if node.has_worktree() {
            GLYPH_WORKTREE.to_string()
        } else {
            style::dim(GLYPH_METADATA)
        };

        // Label: branch name (bold if has worktree, dim if metadata-only)
        let label = if node.has_worktree() {
            style::bold(&node.branch)
        } else {
            style::dim(&node.branch)
        };

        // Optional path annotation: dim `./path` only when it differs from branch name.
        let path_ann = node.row.as_ref().and_then(|r| {
            if r.dir_name != node.branch {
                Some(format!("  {}{}", style::dim("./"), r.dir_name.clone()))
            } else {
                None
            }
        });

        // Content column (label + optional path) with padding to align indicators.
        let this_content_w = content_width(prefix, connector, node);
        let content_pad = max_content_w.saturating_sub(this_content_w);

        // Indicators column.
        let (ind_str, ind_w) = if let Some(r) = &node.row {
            let s = format_indicators(&r.indicators);
            let w = r.indicators.join(" ").width();
            (s, w)
        } else {
            (String::new(), 0)
        };
        let ind_pad = max_ind_w.saturating_sub(ind_w);

        // Activity column.
        let activity = node
            .row
            .as_ref()
            .map(|r| style::dim(&r.last_activity))
            .unwrap_or_default();

        // `← here` marker.
        let here = if show_here_marker {
            node.row
                .as_ref()
                .filter(|r| r.is_active)
                .map(|_| format!("  {}", style::dim("← here")))
                .unwrap_or_default()
        } else {
            String::new()
        };

        // Assemble line: [prefix][connector][glyph] [label][path][pad]  [indicators][ind_pad]  [activity][here]
        let path_str = path_ann.unwrap_or_default();
        let line = if node.row.is_some() {
            format!(
                "{}{}{} {}{}{}  {}{}  {}{}",
                prefix,
                connector,
                glyph,
                label,
                path_str,
                " ".repeat(content_pad),
                ind_str,
                " ".repeat(ind_pad),
                activity,
                here,
            )
        } else {
            // Metadata-only: just prefix+connector+glyph+label, no indicator/time columns.
            format!("{}{}{} {}", prefix, connector, glyph, label,)
        };

        lines.push(line);
        selection.push(node.branch.clone());
    }

    (lines, selection)
}

/// Flatten the forest into (prefix_str, connector_str, &TreeNode) tuples in depth-first order.
///
/// The connector-line algorithm:
/// - Root nodes: prefix="", connector=""
/// - A parent with multiple children uses "├─" (not-last) / "└─" (last) for each child.
/// - A parent with exactly one child uses "" (no fork char); the child's visual column doesn't
///   increase — linear chains stay aligned, showing only the ancestor's continuation bar.
/// - After "├─", the child's `child_prefix = parent_prefix + "│ "`
/// - After "└─", the child's `child_prefix = parent_prefix + "  "`
/// - After "" (single child), the child's `child_prefix = parent_prefix + connector_col`
///   where connector_col is "" (since we're just inheriting the same prefix).
fn flatten_tree<'a>(forest: &'a [TreeNode]) -> Vec<(String, String, &'a TreeNode)> {
    let mut result = Vec::new();
    for node in forest {
        flatten_node(node, "", "", &mut result);
    }
    result
}

fn flatten_node<'a>(
    node: &'a TreeNode,
    parent_prefix: &str,
    connector: &str,
    result: &mut Vec<(String, String, &'a TreeNode)>,
) {
    result.push((parent_prefix.to_string(), connector.to_string(), node));

    // Column continuation contributed by this node for its children.
    // Depends on the connector used to arrive at this node:
    //   "├─" → "│ "   (continuation bar; another sibling follows)
    //   "└─" → "  "   (spaces; this was the last sibling)
    //   ""   → ""     (single child or root; pass through unchanged)
    let my_col = match connector {
        "├─" => "│ ",
        "└─" => "  ",
        _ => "",
    };
    let child_prefix = format!("{}{}", parent_prefix, my_col);

    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let child_connector = if n > 1 {
            if i < n - 1 {
                "├─"
            } else {
                "└─"
            }
        } else {
            ""
        };
        flatten_node(child, &child_prefix, child_connector, result);
    }
}

/// Compute the visual width of the content portion of a tree row
/// (prefix + connector + glyph + " " + label + optional_path).
///
/// Used for column alignment across all rows.
fn content_width(prefix: &str, connector: &str, node: &TreeNode) -> usize {
    let path_extra = node.row.as_ref().and_then(|r| {
        if r.dir_name != node.branch {
            // "  ./" + dir_name
            Some(2 + 2 + r.dir_name.width())
        } else {
            None
        }
    });
    // prefix + connector + glyph(1) + " "(1) + branch_label + optional_path
    prefix.width()
        + connector.width()
        + GLYPH_WORKTREE.width() // same width as GLYPH_METADATA
        + 1 // space after glyph
        + node.branch.width()
        + path_extra.unwrap_or(0)
}

// ── Picker rendering (interactive selection) ─────────────────────────────────

/// Result of rendering a list for the interactive picker.
pub struct PickerRender {
    /// Display strings (one per visible row), including ANSI styling.
    pub lines: Vec<String>,
    /// Selection key for each visible row (branch name for tree, dir_name for flat).
    pub keys: Vec<String>,
    /// Index of the initially-selected item (best fuzzy match, or active/first node).
    pub cursor: usize,
}

/// Render a forest for the interactive picker with optional fuzzy filtering.
///
/// When `query` is empty, all nodes are visible and the cursor is placed on the
/// active node (current-directory worktree). When `query` is non-empty:
/// - Non-matching nodes are removed unless they are ancestors of a match.
/// - Connectors are recomputed over the visible subset (no orphaned `├─`/`└─`).
/// - Matched characters in a label are underlined; non-matched characters in a
///   matching label are dimmed; ancestor-only labels are fully dimmed.
/// - The cursor is placed on the highest-scoring matching node.
pub fn render_tree(forest: &[TreeNode], query: &str, matcher: &SkimMatcherV2) -> PickerRender {
    if forest.is_empty() {
        return PickerRender {
            lines: vec![],
            keys: vec![],
            cursor: 0,
        };
    }

    // Collect fuzzy match results for every node's branch name.
    let mut match_map: HashMap<String, Option<(i64, Vec<usize>)>> = HashMap::new();
    collect_match_results_tree(forest, query, matcher, &mut match_map);

    // Build a filtered flat list; connectors are recomputed over the visible subset.
    let flat = flatten_tree_filtered(forest, query, &match_map);

    if flat.is_empty() {
        return PickerRender {
            lines: vec![],
            keys: vec![],
            cursor: 0,
        };
    }

    // Column widths — same approach as format_tree_lines.
    let max_content_w = flat
        .iter()
        .map(|(prefix, connector, node)| content_width(prefix, connector, node))
        .max()
        .unwrap_or(0);
    let max_ind_w = flat
        .iter()
        .filter_map(|(_, _, node)| node.row.as_ref().map(|r| r.indicators.join(" ").width()))
        .max()
        .unwrap_or(0);

    let mut lines = Vec::with_capacity(flat.len());
    let mut keys = Vec::with_capacity(flat.len());
    let mut best_score: Option<i64> = None;
    let mut cursor = 0usize;

    for (i, (prefix, connector, node)) in flat.iter().enumerate() {
        let direct_match = match_map.get(&node.branch).and_then(|v| v.as_ref());
        let is_ancestor_only = !query.is_empty() && direct_match.is_none();

        // Track best-match cursor.
        if let Some((score, _)) = direct_match {
            if best_score.map(|b| *score > b).unwrap_or(true) {
                best_score = Some(*score);
                cursor = i;
            }
        }

        // Glyph: keep structural styling regardless of match state.
        let glyph: String = if node.has_worktree() {
            GLYPH_WORKTREE.to_string()
        } else {
            style::dim(GLYPH_METADATA)
        };

        // Label: apply match decoration or fall back to normal styling.
        let label = if query.is_empty() {
            // Same as format_tree_lines: bold if worktree, dim if metadata-only.
            if node.has_worktree() {
                style::bold(&node.branch)
            } else {
                style::dim(&node.branch)
            }
        } else if is_ancestor_only {
            style::dim(&node.branch)
        } else {
            // Direct match: underline matched chars, dim the rest.
            let indices = direct_match.map(|(_, idx)| idx.as_slice()).unwrap_or(&[]);
            decorate_match(&node.branch, indices)
        };

        // Optional path annotation (same logic as format_tree_lines).
        let path_ann = node.row.as_ref().and_then(|r| {
            if r.dir_name != node.branch {
                Some(format!("  {}{}", style::dim("./"), r.dir_name.clone()))
            } else {
                None
            }
        });

        let this_content_w = content_width(prefix, connector, node);
        let content_pad = max_content_w.saturating_sub(this_content_w);

        let (ind_str, ind_w) = if let Some(r) = &node.row {
            let s = format_indicators(&r.indicators);
            let w = r.indicators.join(" ").width();
            (s, w)
        } else {
            (String::new(), 0)
        };
        let ind_pad = max_ind_w.saturating_sub(ind_w);

        let activity = node
            .row
            .as_ref()
            .map(|r| style::dim(&r.last_activity))
            .unwrap_or_default();

        // No `← here` marker: the picker cursor serves that role.
        let path_str = path_ann.unwrap_or_default();
        let line = if node.row.is_some() {
            format!(
                "{}{}{} {}{}{}  {}{}  {}",
                prefix,
                connector,
                glyph,
                label,
                path_str,
                " ".repeat(content_pad),
                ind_str,
                " ".repeat(ind_pad),
                activity,
            )
        } else {
            format!("{}{}{} {}", prefix, connector, glyph, label)
        };

        lines.push(line);
        keys.push(node.branch.clone());
    }

    // With no query, place cursor on the active node instead of best match.
    if query.is_empty() {
        cursor = flat
            .iter()
            .position(|(_, _, n)| n.row.as_ref().map(|r| r.is_active).unwrap_or(false))
            .unwrap_or(0);
    }

    PickerRender {
        lines,
        keys,
        cursor,
    }
}

/// Render flat (non-stack) rows for the interactive picker with optional fuzzy filtering.
///
/// Matches on `dir_name`. Non-matching rows are hidden entirely (no ancestor relationships
/// in the flat list). Keys are `dir_name` values. Matched characters are underlined;
/// non-matched characters are dimmed.
pub fn render_flat(
    rows: &[WorktreeDisplayRow],
    query: &str,
    matcher: &SkimMatcherV2,
) -> PickerRender {
    if rows.is_empty() {
        return PickerRender {
            lines: vec![],
            keys: vec![],
            cursor: 0,
        };
    }

    // Compute match data for every row.
    let match_data: Vec<Option<(i64, Vec<usize>)>> = rows
        .iter()
        .map(|r| {
            if query.is_empty() {
                None
            } else {
                matcher.fuzzy_indices(&r.dir_name, query)
            }
        })
        .collect();

    // Filter to visible rows.
    let visible: Vec<(usize, &WorktreeDisplayRow, Option<&(i64, Vec<usize>)>)> = rows
        .iter()
        .zip(match_data.iter())
        .enumerate()
        .filter(|(_, (_, m))| query.is_empty() || m.is_some())
        .map(|(i, (row, m))| (i, row, m.as_ref()))
        .collect();

    if visible.is_empty() {
        return PickerRender {
            lines: vec![],
            keys: vec![],
            cursor: 0,
        };
    }

    // Column widths over visible rows.
    let max_name = visible
        .iter()
        .map(|(_, r, _)| r.dir_name.width())
        .max()
        .unwrap_or(0);
    let indicator_widths: Vec<usize> = visible
        .iter()
        .map(|(_, r, _)| r.indicators.join(" ").width())
        .collect();
    let max_indicators = indicator_widths.iter().copied().max().unwrap_or(0);

    let mut lines = Vec::with_capacity(visible.len());
    let mut keys = Vec::with_capacity(visible.len());
    let mut best_score: Option<i64> = None;
    let mut cursor = 0usize;

    for (vis_i, (_, row, match_result)) in visible.iter().enumerate() {
        // Track best-match cursor.
        if let Some((score, _)) = match_result {
            if best_score.map(|b| *score > b).unwrap_or(true) {
                best_score = Some(*score);
                cursor = vis_i;
            }
        }

        let prefix = style::dim("./");
        let name = if query.is_empty() {
            style::bold(&row.dir_name)
        } else {
            let indices = match_result.map(|(_, idx)| idx.as_slice()).unwrap_or(&[]);
            decorate_match(&row.dir_name, indices)
        };
        let name_pad = max_name - row.dir_name.width();

        let indicators_display = format_indicators(&row.indicators);
        let indicators_pad = max_indicators - indicator_widths[vis_i];

        let activity = style::dim(&row.last_activity);
        let branch = row
            .branch_annotation
            .as_deref()
            .map(|ann| format!("  {}", style::dim(ann)))
            .unwrap_or_default();

        lines.push(format!(
            "{}{}{} {}{}  {}{}",
            prefix,
            name,
            " ".repeat(name_pad),
            indicators_display,
            " ".repeat(indicators_pad),
            activity,
            branch,
        ));
        keys.push(row.dir_name.clone());
    }

    // With no query, place cursor on the active row instead of best match.
    if query.is_empty() {
        cursor = visible
            .iter()
            .position(|(_, row, _)| row.is_active)
            .unwrap_or(0);
    }

    PickerRender {
        lines,
        keys,
        cursor,
    }
}

/// Collect fuzzy match results for every branch name in a forest.
fn collect_match_results_tree(
    nodes: &[TreeNode],
    query: &str,
    matcher: &SkimMatcherV2,
    map: &mut HashMap<String, Option<(i64, Vec<usize>)>>,
) {
    for node in nodes {
        let result = if query.is_empty() {
            None
        } else {
            matcher.fuzzy_indices(&node.branch, query)
        };
        map.insert(node.branch.clone(), result);
        collect_match_results_tree(&node.children, query, matcher, map);
    }
}

/// Returns true when the node should appear in the filtered picker list.
///
/// A node is visible if it directly matches the query, or if any descendant matches.
/// When the query is empty, all nodes are visible.
fn is_node_visible(
    node: &TreeNode,
    query: &str,
    match_map: &HashMap<String, Option<(i64, Vec<usize>)>>,
) -> bool {
    if query.is_empty() {
        return true;
    }
    if match_map
        .get(&node.branch)
        .and_then(|v| v.as_ref())
        .is_some()
    {
        return true;
    }
    node.children
        .iter()
        .any(|c| is_node_visible(c, query, match_map))
}

/// Flatten a forest into `(prefix, connector, node)` tuples, skipping non-visible nodes.
///
/// Connectors are recomputed over the visible subset so `├─`/`└─` always refer to
/// real visible siblings — no orphaned connector glyphs.
fn flatten_tree_filtered<'a>(
    forest: &'a [TreeNode],
    query: &str,
    match_map: &HashMap<String, Option<(i64, Vec<usize>)>>,
) -> Vec<(String, String, &'a TreeNode)> {
    let mut result = Vec::new();
    for node in forest {
        if is_node_visible(node, query, match_map) {
            flatten_node_filtered(node, "", "", query, match_map, &mut result);
        }
    }
    result
}

fn flatten_node_filtered<'a>(
    node: &'a TreeNode,
    parent_prefix: &str,
    connector: &str,
    query: &str,
    match_map: &HashMap<String, Option<(i64, Vec<usize>)>>,
    result: &mut Vec<(String, String, &'a TreeNode)>,
) {
    result.push((parent_prefix.to_string(), connector.to_string(), node));

    // Column continuation — same rules as flatten_node.
    let my_col = match connector {
        "├─" => "│ ",
        "└─" => "  ",
        _ => "",
    };
    let child_prefix = format!("{}{}", parent_prefix, my_col);

    // Only consider visible children; recompute connectors for the visible subset.
    let visible_children: Vec<&TreeNode> = node
        .children
        .iter()
        .filter(|c| is_node_visible(c, query, match_map))
        .collect();

    let n = visible_children.len();
    for (i, child) in visible_children.iter().enumerate() {
        let child_connector = if n > 1 {
            if i < n - 1 {
                "├─"
            } else {
                "└─"
            }
        } else {
            ""
        };
        flatten_node_filtered(
            child,
            &child_prefix,
            child_connector,
            query,
            match_map,
            result,
        );
    }
}

/// Decorate a label by underlining matched characters and dimming the rest.
///
/// `match_indices` are character (not byte) positions that matched the query.
/// Characters not in `match_indices` are dimmed; those in `match_indices` are underlined.
fn decorate_match(label: &str, match_indices: &[usize]) -> String {
    if match_indices.is_empty() {
        return label.to_string();
    }

    let idx_set: std::collections::HashSet<usize> = match_indices.iter().copied().collect();
    let mut result = String::new();
    let mut span = String::new();
    let mut span_is_match = false;
    let mut first = true;

    for (i, ch) in label.chars().enumerate() {
        let this_is_match = idx_set.contains(&i);
        if !first && this_is_match != span_is_match {
            // Flush the current run.
            if span_is_match {
                result.push_str(&style::underline(&span));
            } else {
                result.push_str(&style::dim(&span));
            }
            span.clear();
        }
        first = false;
        span_is_match = this_is_match;
        span.push(ch);
    }
    // Flush the final run.
    if !span.is_empty() {
        if span_is_match {
            result.push_str(&style::underline(&span));
        } else {
            result.push_str(&style::dim(&span));
        }
    }

    result
}

// ── Relative time ─────────────────────────────────────────────────────────────

/// Convert a Unix timestamp to a human-readable relative time string.
pub fn format_relative_time(epoch_seconds: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - epoch_seconds;

    if diff < 0 {
        return "just now".to_string();
    }

    let seconds = diff;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    let weeks = days / 7;
    let months = days / 30;
    let years = days / 365;

    if seconds < 60 {
        "just now".to_string()
    } else if minutes == 1 {
        "1 minute ago".to_string()
    } else if minutes < 60 {
        format!("{minutes} minutes ago")
    } else if hours == 1 {
        "1 hour ago".to_string()
    } else if hours < 24 {
        format!("{hours} hours ago")
    } else if days == 1 {
        "1 day ago".to_string()
    } else if days < 7 {
        format!("{days} days ago")
    } else if weeks == 1 {
        "1 week ago".to_string()
    } else if weeks < 5 {
        format!("{weeks} weeks ago")
    } else if months == 1 {
        "1 month ago".to_string()
    } else if months < 12 {
        format!("{months} months ago")
    } else if years == 1 {
        "1 year ago".to_string()
    } else {
        format!("{years} years ago")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use fuzzy_matcher::skim::SkimMatcherV2;

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn leaf(branch: &str, is_active: bool) -> TreeNode {
        TreeNode {
            branch: branch.to_string(),
            row: Some(WorktreeDisplayRow {
                is_active,
                dir_name: branch.to_string(),
                branch_annotation: None,
                indicators: vec![],
                last_activity: String::new(),
                activity_epoch: None,
            }),
            subtree_activity: None,
            children: vec![],
        }
    }

    fn meta(branch: &str) -> TreeNode {
        TreeNode {
            branch: branch.to_string(),
            row: None,
            subtree_activity: None,
            children: vec![],
        }
    }

    fn flat_row(dir_name: &str, is_active: bool) -> WorktreeDisplayRow {
        WorktreeDisplayRow {
            is_active,
            dir_name: dir_name.to_string(),
            branch_annotation: None,
            indicators: vec![],
            last_activity: String::new(),
            activity_epoch: None,
        }
    }

    fn matcher() -> SkimMatcherV2 {
        SkimMatcherV2::default()
    }

    // ── render_tree: empty query ──────────────────────────────────────────────

    #[test]
    fn render_tree_empty_query_shows_all_nodes() {
        let mut root = leaf("main", false);
        root.children = vec![leaf("feature", false)];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());
        assert_eq!(result.keys, vec!["main", "feature"]);
    }

    #[test]
    fn render_tree_empty_query_cursor_on_active_node() {
        // Active node is "feature" at depth 1; cursor should point to it.
        let mut root = leaf("main", false);
        root.children = vec![leaf("feature", true)];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());
        // flat order: main=0, feature=1
        assert_eq!(result.cursor, 1);
    }

    // ── render_tree: filtered query ───────────────────────────────────────────

    #[test]
    fn render_tree_query_hides_nonmatching_leaf() {
        let forest = vec![leaf("apple", false), leaf("zebra", false)];
        let result = render_tree(&forest, "apple", &matcher());
        assert_eq!(result.keys, vec!["apple"]);
    }

    #[test]
    fn render_tree_query_keeps_ancestor_of_matching_descendant() {
        // "main" doesn't match "feat", but "feature" (its child) does.
        // Both should appear; cursor on the match (feature), not the ancestor.
        let mut root = leaf("main", false);
        root.children = vec![leaf("feature", false)];
        let forest = vec![root];
        let result = render_tree(&forest, "feat", &matcher());
        assert_eq!(result.keys.len(), 2, "ancestor should be retained");
        assert!(result.keys.contains(&"main".to_string()));
        assert!(result.keys.contains(&"feature".to_string()));
        assert_eq!(
            result.keys[result.cursor], "feature",
            "cursor should be on the matching node"
        );
    }

    #[test]
    fn render_tree_query_drops_unrelated_subtree() {
        // "apple" matches "apple"; "zebra" and its child "zoo" do not.
        let mut zebra = leaf("zebra", false);
        zebra.children = vec![leaf("zoo", false)];
        let forest = vec![leaf("apple", false), zebra];
        let result = render_tree(&forest, "apple", &matcher());
        assert_eq!(result.keys, vec!["apple"]);
    }

    #[test]
    fn render_tree_no_match_returns_empty_render() {
        let forest = vec![leaf("apple", false), leaf("feature", false)];
        let result = render_tree(&forest, "zzz", &matcher());
        assert!(result.lines.is_empty());
        assert!(result.keys.is_empty());
    }

    #[test]
    fn render_tree_connectors_recomputed_for_visible_subset() {
        // Forest: root → [child-a, child-b, child-c]
        // Query matches only child-a and child-c; child-b is hidden.
        // Connectors should be ├─ (child-a) and └─ (child-c), skipping child-b.
        let mut root = meta("trunk");
        root.children = vec![
            leaf("child-a", false),
            leaf("child-b", false),
            leaf("child-c", false),
        ];
        let forest = vec![root];
        let result = render_tree(&forest, "child-", &matcher());

        // child-a, child-b, child-c all match "child-" — all visible.
        // Connector for the last child should be └─.
        assert_eq!(result.keys.len(), 4); // trunk + 3 children
        let last_child_line = &result.lines[3];
        assert!(
            last_child_line.contains("└─"),
            "last visible child should get └─, got: {last_child_line}"
        );
    }

    #[test]
    fn render_tree_connector_correct_when_middle_child_filtered() {
        // Forest: trunk → [child-a, child-b, child-c]
        // Query matches only child-a and child-c; child-b is hidden.
        // After filtering: trunk has 2 visible children → ├─ / └─.
        let mut root = meta("trunk");
        root.children = vec![
            leaf("child-a", false),
            leaf("unrelated", false), // won't match
            leaf("child-c", false),
        ];
        let forest = vec![root];
        let result = render_tree(&forest, "child", &matcher());

        // trunk (ancestor of matches) + child-a + child-c (unrelated filtered out)
        assert_eq!(result.keys, vec!["trunk", "child-a", "child-c"]);
        // Two visible children → first gets ├─, last gets └─
        assert!(
            result.lines[1].contains("├─"),
            "first of two visible children should get ├─, got: {}",
            result.lines[1]
        );
        assert!(
            result.lines[2].contains("└─"),
            "last of two visible children should get └─, got: {}",
            result.lines[2]
        );
    }

    // ── render_flat ────────────────────────────────────────────────────────────

    #[test]
    fn render_flat_empty_query_shows_all_rows() {
        let rows = vec![flat_row("apple", false), flat_row("zebra", false)];
        let result = render_flat(&rows, "", &matcher());
        assert_eq!(result.keys, vec!["apple", "zebra"]);
        assert_eq!(result.cursor, 0);
    }

    #[test]
    fn render_flat_empty_query_cursor_on_active_row() {
        let rows = vec![flat_row("apple", false), flat_row("zebra", true)];
        let result = render_flat(&rows, "", &matcher());
        assert_eq!(result.cursor, 1); // zebra is active
    }

    #[test]
    fn render_flat_query_filters_rows() {
        let rows = vec![flat_row("apple", false), flat_row("zebra", false)];
        let result = render_flat(&rows, "app", &matcher());
        assert_eq!(result.keys, vec!["apple"]);
        assert_eq!(result.cursor, 0);
    }

    #[test]
    fn render_flat_no_match_returns_empty_render() {
        let rows = vec![flat_row("apple", false)];
        let result = render_flat(&rows, "zzz", &matcher());
        assert!(result.lines.is_empty());
        assert!(result.keys.is_empty());
    }

    #[test]
    fn render_flat_cursor_on_best_match() {
        // "app" is a strong prefix match for "apple" but a weaker match for "grapple"
        let rows = vec![flat_row("grapple", false), flat_row("apple", false)];
        let result = render_flat(&rows, "app", &matcher());
        // Both "grapple" and "apple" match "app"; apple should score higher (prefix)
        // and the cursor should land on it even though grapple comes first in list.
        assert_eq!(result.keys[result.cursor], "apple");
    }
}
