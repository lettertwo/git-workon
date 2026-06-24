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
//! Graphite-style lane graph: each stack is one straight vertical lane; the graph grows
//! horizontally by one column per concurrent sibling stack. `◉`/`◎`/`◯` glyphs encode
//! worktree state; converging lanes close on the fork node's own row (no connector-only rows).
//! Display order is tip-on-top, trunk at the bottom. `← here` marks the current worktree.
//!
//! ```text
//! ◯ api-3
//! ◎ api-2          ↑   2h ago
//! ◯ api-1
//! │ ◯ branch-x
//! │ │ ◯ branch-y
//! │ ◎─╯ shared  ./base     5d ago
//! ◉─╯ main             ↑   2m ago  ← here
//! ◎ ee/testing           1mo ago
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

type MatchMap = HashMap<String, Option<(i64, Vec<usize>, Option<Vec<usize>>)>>;

/// Glyph for a diff/branch that has a checked-out worktree (but is not the active one).
pub const GLYPH_WORKTREE: &str = "◎";
/// Glyph for a diff/branch that exists only in stack metadata (no checked-out worktree).
pub const GLYPH_METADATA: &str = "◯";
/// Glyph for the active (current-directory) worktree.
pub const GLYPH_ACTIVE: &str = "◉";

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
            let name = if row.is_active {
                style::green_bold(&row.dir_name)
            } else {
                style::bold(&row.dir_name)
            };
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
    forest.sort_by_key(|n| std::cmp::Reverse(n.subtree_activity));

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

// ── Lane graph layout ─────────────────────────────────────────────────────────

/// One row in the tip-on-top lane-graph layout.
///
/// Every row corresponds to exactly one [`TreeNode`] — there are no connector-only rows.
/// The gutter is constructed from `own_lane`, `passthrough_lanes`, and `closing_count`.
struct LaneRow<'a> {
    node: &'a TreeNode,
    /// The lane index this node occupies (0 = leftmost).
    own_lane: usize,
    /// Lane indices open at this row that are not this node's lane (rendered as `│ `).
    passthrough_lanes: Vec<usize>,
    /// Number of sibling lanes that converge onto this node's row, closing to its right
    /// (rendered as `─╯`, `─┴─╯`, etc. immediately after the glyph).
    closing_count: usize,
}

/// Visual display width of the gutter portion of a lane row.
///
/// = passthrough columns (`own_lane × 2`) + glyph width (1) + closing connector (`closing_count × 2`).
fn lane_gutter_width(own_lane: usize, closing_count: usize) -> usize {
    own_lane * 2 + GLYPH_WORKTREE.width() + closing_count * 2
}

/// Visual display width of the full content portion of a lane row
/// (gutter + space + branch label + optional path annotation).
fn lane_content_width(own_lane: usize, closing_count: usize, node: &TreeNode) -> usize {
    let path_extra = node.row.as_ref().and_then(|r| {
        if r.dir_name != node.branch {
            Some(2 + 2 + r.dir_name.width()) // "  ./" + dir_name
        } else {
            None
        }
    });
    lane_gutter_width(own_lane, closing_count)
        + 1 // space after glyph/connector
        + node.branch.width()
        + path_extra.unwrap_or(0)
}

/// Render the gutter string for a lane row.
///
/// Produces: `(│ | )* glyph [─╯ | ─┴─╯ | …]`
/// - Each column before `own_lane` renders as `│ ` (passthrough) or `  ` (empty/closed).
/// - `own_lane` renders as `glyph`.
/// - If `closing_count > 0`, appends `─`, then `(closing_count − 1) × ┴─`, then `╯`.
fn render_gutter(
    own_lane: usize,
    passthrough_lanes: &[usize],
    closing_count: usize,
    glyph: &str,
) -> String {
    let pass_set: std::collections::HashSet<usize> = passthrough_lanes.iter().copied().collect();
    let mut gutter = String::new();
    for col in 0..own_lane {
        if pass_set.contains(&col) {
            gutter.push_str("│ ");
        } else {
            gutter.push_str("  ");
        }
    }
    gutter.push_str(glyph);
    if closing_count > 0 {
        gutter.push('─');
        for _ in 0..closing_count - 1 {
            gutter.push_str("┴─");
        }
        gutter.push('╯');
    }
    gutter
}

/// Build lane rows for a forest (tip-on-top; each root is an independent lane block).
fn build_lane_rows(forest: &[TreeNode]) -> Vec<LaneRow<'_>> {
    build_lane_rows_impl(forest, &|_: &TreeNode| true)
}

/// Build lane rows for the visible subset of a forest (fuzzy-filtered picker).
fn build_lane_rows_filtered<'a>(
    forest: &'a [TreeNode],
    query: &str,
    match_map: &MatchMap,
) -> Vec<LaneRow<'a>> {
    let include = |node: &TreeNode| is_node_visible(node, query, match_map);
    build_lane_rows_impl(forest, &include)
}

fn build_lane_rows_impl<'a, F>(forest: &'a [TreeNode], include: &F) -> Vec<LaneRow<'a>>
where
    F: Fn(&TreeNode) -> bool,
{
    let mut rows = Vec::new();
    for node in forest {
        if include(node) {
            let mut next_lane = 1usize; // root occupies lane 0; siblings start at 1
            emit_lane_rows(node, 0, &[], &mut next_lane, include, &mut rows);
        }
    }
    rows
}

/// Recursively emit `LaneRow`s for `node` and its subtree (tip-on-top: children first, then node).
///
/// - `own_lane`: lane assigned to this node.
/// - `passthrough_lanes`: lanes open above that pass through this subtree unchanged.
/// - `next_lane`: shared counter; each new sibling lane claims the next value.
/// - `include`: returns true when a node (or its subtree) should appear in output.
fn emit_lane_rows<'a, F>(
    node: &'a TreeNode,
    own_lane: usize,
    passthrough_lanes: &[usize],
    next_lane: &mut usize,
    include: &F,
    rows: &mut Vec<LaneRow<'a>>,
) where
    F: Fn(&TreeNode) -> bool,
{
    // Visible children only.
    let visible: Vec<&TreeNode> = node.children.iter().filter(|c| include(c)).collect();

    if visible.is_empty() {
        rows.push(LaneRow {
            node,
            own_lane,
            passthrough_lanes: passthrough_lanes.to_vec(),
            closing_count: 0,
        });
        return;
    }

    // Primary child = highest subtree_activity; earlier index wins ties.
    let primary_idx: Option<usize> = visible
        .iter()
        .enumerate()
        .max_by_key(|(i, c)| (c.subtree_activity, std::cmp::Reverse(*i)))
        .map(|(i, _)| i);

    // Non-primary children (siblings) each claim a new lane.
    let siblings: Vec<(&TreeNode, usize)> = visible
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| {
            if Some(i) == primary_idx {
                None
            } else {
                let lane = *next_lane;
                *next_lane += 1;
                Some((c, lane))
            }
        })
        .collect();

    // 1. Emit primary subtree (same lane, same passthrough).
    if let Some(pi) = primary_idx {
        emit_lane_rows(
            visible[pi],
            own_lane,
            passthrough_lanes,
            next_lane,
            include,
            rows,
        );
    }

    // 2. Emit each sibling's subtree.
    //    Passthrough for sibling i = passthrough_lanes ∪ {own_lane} ∪ {earlier siblings' lanes}.
    for (si, &(sib, sib_lane)) in siblings.iter().enumerate() {
        let mut sib_pass: Vec<usize> = passthrough_lanes.to_vec();
        sib_pass.push(own_lane);
        for &(_, lane) in siblings.iter().take(si) {
            sib_pass.push(lane);
        }
        sib_pass.sort_unstable();
        emit_lane_rows(sib, sib_lane, &sib_pass, next_lane, include, rows);
    }

    // 3. Emit this node — sibling lanes converge here.
    rows.push(LaneRow {
        node,
        own_lane,
        passthrough_lanes: passthrough_lanes.to_vec(),
        closing_count: siblings.len(),
    });
}

/// Return the styled glyph for a tree node based on its state.
///
/// Three-state vocabulary:
/// - `◉` green+bold — active (the current-directory worktree)
/// - `◎` plain — a worktree exists but is not current
/// - `◯` dim — metadata-only diff (no worktree)
fn node_glyph(is_active: bool, has_worktree: bool) -> String {
    if is_active {
        style::green_bold(GLYPH_ACTIVE)
    } else if has_worktree {
        GLYPH_WORKTREE.to_string()
    } else {
        style::dim(GLYPH_METADATA)
    }
}

/// Return the styled branch label for a tree node.
///
/// - active → green+bold
/// - has worktree → bold
/// - metadata-only → dim
fn node_label(branch: &str, is_active: bool, has_worktree: bool) -> String {
    if is_active {
        style::green_bold(branch)
    } else if has_worktree {
        style::bold(branch)
    } else {
        style::dim(branch)
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

    // Build lane rows (tip-on-top; each root is an independent block).
    let flat = build_lane_rows(forest);

    // Compute max content width for column alignment.
    let max_content_w = flat
        .iter()
        .map(|r| lane_content_width(r.own_lane, r.closing_count, r.node))
        .max()
        .unwrap_or(0);

    // Max indicator width across all rows that have worktrees.
    let max_ind_w = flat
        .iter()
        .filter_map(|r| {
            r.node
                .row
                .as_ref()
                .map(|row| row.indicators.join(" ").width())
        })
        .max()
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    let mut selection: Vec<String> = Vec::new();

    for lr in &flat {
        let node = lr.node;
        let is_active = node.row.as_ref().map(|r| r.is_active).unwrap_or(false);
        let glyph = node_glyph(is_active, node.has_worktree());
        let label = node_label(&node.branch, is_active, node.has_worktree());
        let gutter = render_gutter(lr.own_lane, &lr.passthrough_lanes, lr.closing_count, &glyph);

        // Optional path annotation: dim `./path` only when it differs from branch name.
        let path_ann = node.row.as_ref().and_then(|r| {
            if r.dir_name != node.branch {
                Some(format!("  {}{}", style::dim("./"), r.dir_name.clone()))
            } else {
                None
            }
        });

        // Padding to align the indicator column.
        let this_content_w = lane_content_width(lr.own_lane, lr.closing_count, node);
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

        // Assemble line: [gutter] [label][path][pad]  [indicators][ind_pad]  [activity][here]
        let path_str = path_ann.unwrap_or_default();
        let line = if node.row.is_some() {
            format!(
                "{} {}{}{}  {}{}  {}{}",
                gutter,
                label,
                path_str,
                " ".repeat(content_pad),
                ind_str,
                " ".repeat(ind_pad),
                activity,
                here,
            )
        } else {
            // Metadata-only: gutter + label, no indicator/time columns.
            format!("{} {}", gutter, label)
        };

        lines.push(line);
        selection.push(node.branch.clone());
    }

    (lines, selection)
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
/// - Lanes are recomputed over the visible subset (no orphaned gutter segments).
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

    // Collect fuzzy match results for every node (branch name + dir name when different).
    let mut match_map: MatchMap = HashMap::new();
    collect_match_results_tree(forest, query, matcher, &mut match_map);

    // Build lane rows over the visible subset; lanes recomputed for the filtered tree.
    let flat = build_lane_rows_filtered(forest, query, &match_map);

    if flat.is_empty() {
        return PickerRender {
            lines: vec![],
            keys: vec![],
            cursor: 0,
        };
    }

    // Column widths.
    let max_content_w = flat
        .iter()
        .map(|r| lane_content_width(r.own_lane, r.closing_count, r.node))
        .max()
        .unwrap_or(0);
    let max_ind_w = flat
        .iter()
        .filter_map(|r| {
            r.node
                .row
                .as_ref()
                .map(|row| row.indicators.join(" ").width())
        })
        .max()
        .unwrap_or(0);

    let mut lines = Vec::with_capacity(flat.len());
    let mut keys = Vec::with_capacity(flat.len());
    let mut top_score: Option<i64> = None;
    let mut cursor = 0usize;

    for (i, lr) in flat.iter().enumerate() {
        let node = lr.node;
        let direct_match = match_map.get(&node.branch).and_then(|v| v.as_ref());
        let is_ancestor_only = !query.is_empty() && direct_match.is_none();

        // Track best-match cursor by combined score.
        if let Some((score, _, _)) = direct_match {
            if top_score.map(|b| *score > b).unwrap_or(true) {
                top_score = Some(*score);
                cursor = i;
            }
        }

        // Glyph: reflect active/worktree/metadata state regardless of match state.
        let is_active = node.row.as_ref().map(|r| r.is_active).unwrap_or(false);
        let glyph = node_glyph(is_active, node.has_worktree());
        let gutter = render_gutter(lr.own_lane, &lr.passthrough_lanes, lr.closing_count, &glyph);

        // Label: apply match decoration or fall back to normal styling.
        let branch_indices = direct_match
            .map(|(_, idx, _)| idx.as_slice())
            .unwrap_or(&[]);
        let label = if query.is_empty() {
            node_label(&node.branch, is_active, node.has_worktree())
        } else if is_ancestor_only {
            style::dim(&node.branch)
        } else {
            decorate_match(&node.branch, branch_indices)
        };

        // Optional path annotation; decorate with dir-name match indices when present.
        let path_ann = node.row.as_ref().and_then(|r| {
            if r.dir_name != node.branch {
                let ann = if query.is_empty() || is_ancestor_only {
                    format!("  {}{}", style::dim("./"), r.dir_name.clone())
                } else {
                    let dir_indices = direct_match
                        .and_then(|(_, _, d)| d.as_deref())
                        .unwrap_or(&[]);
                    format!(
                        "  {}{}",
                        style::dim("./"),
                        decorate_match(&r.dir_name, dir_indices)
                    )
                };
                Some(ann)
            } else {
                None
            }
        });

        let this_content_w = lane_content_width(lr.own_lane, lr.closing_count, node);
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
                "{} {}{}{}  {}{}  {}",
                gutter,
                label,
                path_str,
                " ".repeat(content_pad),
                ind_str,
                " ".repeat(ind_pad),
                activity,
            )
        } else {
            format!("{} {}", gutter, label)
        };

        lines.push(line);
        keys.push(node.branch.clone());
    }

    // With no query, place cursor on the active node instead of best match.
    if query.is_empty() {
        cursor = flat
            .iter()
            .position(|r| {
                r.node
                    .row
                    .as_ref()
                    .map(|row| row.is_active)
                    .unwrap_or(false)
            })
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
/// Matches against both `dir_name` and `branch_annotation` (when present); a row is visible
/// if either field matches. Ranking and cursor position use the best score across both fields.
/// Each field is decorated with its own match indices so underlines appear at correct positions.
/// Non-matching rows are hidden entirely (no ancestor relationships in the flat list).
/// Keys are `dir_name` values.
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

    // Match data per row: (combined_score, dir_indices, branch_indices).
    // A row is visible when this is Some (at least one field matched).
    #[allow(clippy::type_complexity)]
    let match_data: Vec<Option<(i64, Option<Vec<usize>>, Option<Vec<usize>>)>> = rows
        .iter()
        .map(|r| {
            if query.is_empty() {
                return None;
            }
            let dir = matcher.fuzzy_indices(&r.dir_name, query);
            let branch = r
                .branch_annotation
                .as_deref()
                .and_then(|ann| matcher.fuzzy_indices(ann, query));
            let score = [
                dir.as_ref().map(|(s, _)| *s),
                branch.as_ref().map(|(s, _)| *s),
            ]
            .into_iter()
            .flatten()
            .max();
            score.map(|s| (s, dir.map(|(_, idx)| idx), branch.map(|(_, idx)| idx)))
        })
        .collect();

    // Filter to visible rows.
    #[allow(clippy::type_complexity)]
    let visible: Vec<(
        usize,
        &WorktreeDisplayRow,
        Option<&(i64, Option<Vec<usize>>, Option<Vec<usize>>)>,
    )> = rows
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
    let mut top_score: Option<i64> = None;
    let mut cursor = 0usize;

    for (vis_i, (_, row, match_result)) in visible.iter().enumerate() {
        // Track best-match cursor by combined score.
        if let Some((score, _, _)) = match_result {
            if top_score.map(|b| *score > b).unwrap_or(true) {
                top_score = Some(*score);
                cursor = vis_i;
            }
        }

        let prefix = style::dim("./");
        let dir_indices = match_result
            .and_then(|(_, d, _)| d.as_deref())
            .unwrap_or(&[]);
        let name = if query.is_empty() {
            if row.is_active {
                style::green_bold(&row.dir_name)
            } else {
                style::bold(&row.dir_name)
            }
        } else {
            decorate_match(&row.dir_name, dir_indices)
        };
        let name_pad = max_name - row.dir_name.width();

        let indicators_display = format_indicators(&row.indicators);
        let indicators_pad = max_indicators - indicator_widths[vis_i];

        let activity = style::dim(&row.last_activity);
        // Decorate the branch annotation with its own match indices when present.
        let branch = if query.is_empty() {
            row.branch_annotation
                .as_deref()
                .map(|ann| format!("  {}", style::dim(ann)))
                .unwrap_or_default()
        } else {
            let branch_indices = match_result
                .and_then(|(_, _, b)| b.as_deref())
                .unwrap_or(&[]);
            row.branch_annotation
                .as_deref()
                .map(|ann| format!("  {}", decorate_match(ann, branch_indices)))
                .unwrap_or_default()
        };

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

/// Collect fuzzy match results for every node in a forest, matching against both the branch
/// name and the directory name (when the node has a worktree with a different dir name).
///
/// Map value: `(combined_score, branch_indices, dir_indices)`.
/// - `combined_score` — best score across both fields; drives cursor ranking and visibility.
/// - `branch_indices` — Skim match positions within `node.branch` (empty when no branch match).
/// - `dir_indices` — Skim match positions within `node.row.dir_name` when present (None when
///   the node has no worktree, the dir name equals the branch name, or the dir name didn't match).
fn collect_match_results_tree(
    nodes: &[TreeNode],
    query: &str,
    matcher: &SkimMatcherV2,
    map: &mut MatchMap,
) {
    for node in nodes {
        let result = if query.is_empty() {
            None
        } else {
            let branch_match = matcher.fuzzy_indices(&node.branch, query);
            // Match against the dir name only when it differs from the branch name.
            let dir_match = node.row.as_ref().and_then(|r| {
                if r.dir_name != node.branch {
                    matcher.fuzzy_indices(&r.dir_name, query)
                } else {
                    None
                }
            });
            let score = [
                branch_match.as_ref().map(|(s, _)| *s),
                dir_match.as_ref().map(|(s, _)| *s),
            ]
            .into_iter()
            .flatten()
            .max();
            score.map(|s| {
                (
                    s,
                    branch_match.map(|(_, idx)| idx).unwrap_or_default(),
                    dir_match.map(|(_, idx)| idx),
                )
            })
        };
        map.insert(node.branch.clone(), result);
        collect_match_results_tree(&node.children, query, matcher, map);
    }
}

/// Returns true when the node should appear in the filtered picker list.
///
/// A node is visible if it directly matches the query, or if any descendant matches.
/// When the query is empty, all nodes are visible.
fn is_node_visible(node: &TreeNode, query: &str, match_map: &MatchMap) -> bool {
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
        // Tip-on-top: child emitted before parent.
        let mut root = leaf("main", false);
        root.children = vec![leaf("feature", false)];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());
        assert_eq!(result.keys, vec!["feature", "main"]);
    }

    #[test]
    fn render_tree_empty_query_cursor_on_active_node() {
        // Active node is "feature"; tip-on-top puts it at index 0.
        let mut root = leaf("main", false);
        root.children = vec![leaf("feature", true)];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());
        assert_eq!(result.cursor, 0);
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
        // Both appear; tip-on-top puts feature first, then main (ancestor).
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
    fn render_tree_lanes_recomputed_for_visible_subset() {
        // trunk → [child-a, child-b, child-c]; all match "child-".
        // child-a is primary (index 0), child-b=lane1, child-c=lane2.
        // Emission order: child-a, child-b, child-c, trunk (trunk last, tip-on-top).
        // Trunk closes 2 sibling lanes → its line contains "─┴─╯".
        let mut root = meta("trunk");
        root.children = vec![
            leaf("child-a", false),
            leaf("child-b", false),
            leaf("child-c", false),
        ];
        let forest = vec![root];
        let result = render_tree(&forest, "child-", &matcher());

        assert_eq!(result.keys, vec!["child-a", "child-b", "child-c", "trunk"]);
        let trunk_line = result.lines.last().unwrap();
        assert!(
            trunk_line.contains("─┴─╯"),
            "trunk with 2 closing sibling lanes should contain ─┴─╯, got: {trunk_line}"
        );
    }

    #[test]
    fn render_tree_lanes_correct_when_middle_child_filtered() {
        // trunk → [child-a, unrelated, child-c]; "child" matches child-a and child-c.
        // After filtering: primary=child-a (lane 0), sibling=child-c (lane 1).
        // Emission: child-a, child-c, trunk; trunk closes 1 lane → "─╯".
        let mut root = meta("trunk");
        root.children = vec![
            leaf("child-a", false),
            leaf("unrelated", false), // won't match
            leaf("child-c", false),
        ];
        let forest = vec![root];
        let result = render_tree(&forest, "child", &matcher());

        assert_eq!(result.keys, vec!["child-a", "child-c", "trunk"]);
        let trunk_line = result.lines.last().unwrap();
        assert!(
            trunk_line.contains("─╯"),
            "trunk closing 1 sibling lane should contain ─╯, got: {trunk_line}"
        );
        // child-c is the sibling lane — its line has passthrough "│ " for trunk's lane.
        assert!(
            result.lines[1].contains("│ "),
            "child-c (sibling lane) should show │ passthrough, got: {}",
            result.lines[1]
        );
    }

    // ── render_tree: lane graph layout ───────────────────────────────────────

    #[test]
    fn render_tree_linear_stack_is_single_pillar() {
        // main → s1 → s2: no siblings, so one straight lane (no │, no ─╯).
        let mut s1 = leaf("s1", false);
        s1.children = vec![leaf("s2", false)];
        let mut root = leaf("main", false);
        root.children = vec![s1];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        // Tip-on-top: s2, s1, main
        assert_eq!(result.keys, vec!["s2", "s1", "main"]);
        // No passthrough or closing connectors (single lane throughout).
        for line in &result.lines {
            assert!(
                !line.contains("│ "),
                "linear stack should have no passthrough │, got: {line}"
            );
            assert!(
                !line.contains("─╯"),
                "linear stack should have no closing ─╯, got: {line}"
            );
        }
    }

    #[test]
    fn render_tree_sibling_stack_closes_on_trunk_row() {
        // main → [s1 (primary), shared (sibling lane 1)].
        // Emission: s1, shared, main.  main's line closes lane 1 → "─╯".
        let mut root = leaf("main", false);
        root.children = vec![leaf("s1", false), leaf("shared", false)];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        assert_eq!(result.keys, vec!["s1", "shared", "main"]);
        // shared is in lane 1 — its line has "│ " passthrough for lane 0.
        assert!(
            result.lines[1].contains("│ "),
            "shared (sibling lane) should show │ passthrough, got: {}",
            result.lines[1]
        );
        // main closes lane 1 on its own row.
        let main_line = result.lines.last().unwrap();
        assert!(
            main_line.contains("─╯"),
            "main should close sibling lane with ─╯, got: {main_line}"
        );
    }

    #[test]
    fn render_tree_mid_stack_fork_closes_on_fork_node() {
        // main → a1 → a2 → [b1 (primary), c1 (sibling)].
        // Emission: b1, c1, a2, a1, main.  a2 closes lane 1 → "─╯".
        let mut a2 = leaf("a2", false);
        a2.children = vec![leaf("b1", false), leaf("c1", false)];
        let mut a1 = leaf("a1", false);
        a1.children = vec![a2];
        let mut root = leaf("main", false);
        root.children = vec![a1];
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        assert_eq!(result.keys, vec!["b1", "c1", "a2", "a1", "main"]);
        let a2_line = &result.lines[2];
        assert!(
            a2_line.contains("─╯"),
            "a2 (fork node) should close sibling lane with ─╯, got: {a2_line}"
        );
        // a1 and main are back to a single lane — no passthrough or closing.
        assert!(
            !result.lines[3].contains("│ "),
            "a1 after fork should have no passthrough, got: {}",
            result.lines[3]
        );
    }

    // ── column alignment ─────────────────────────────────────────────────────

    /// Build a leaf with indicators and activity set (for alignment tests).
    fn leaf_with_data(
        branch: &str,
        is_active: bool,
        indicators: &[&str],
        activity: &str,
    ) -> TreeNode {
        TreeNode {
            branch: branch.to_string(),
            row: Some(WorktreeDisplayRow {
                is_active,
                dir_name: branch.to_string(),
                branch_annotation: None,
                indicators: indicators.iter().map(|s| s.to_string()).collect(),
                last_activity: activity.to_string(),
                activity_epoch: Some(0),
            }),
            subtree_activity: Some(0),
            children: vec![],
        }
    }

    /// Return the display-column position of `needle` in `haystack`.
    /// Uses Unicode display widths so multi-byte glyphs (◉, ─, ╯…) count as 1 column.
    fn display_col_of(haystack: &str, needle: &str, context: &str) -> usize {
        let byte_pos = haystack
            .find(needle)
            .unwrap_or_else(|| panic!("{context}: '{needle}' not found in: {haystack:?}"));
        haystack[..byte_pos].width()
    }

    #[test]
    fn format_tree_lines_indicator_column_aligned_across_lanes() {
        // Forest: main → [s1 (primary, lane 0), shared (sibling, lane 1)]
        // Emission order (tip-on-top): s1, shared, main
        //
        // s1 gutter  = "◎"     (width 1) → narrow
        // main gutter = "◉─╯"  (width 3) → wide (closes sibling lane)
        // shared has no indicators; s1 has "*", main has "↑".
        //
        // Despite the different gutter widths, the indicator column must start at
        // the same byte offset in every row that has indicators.
        let s1 = leaf_with_data("s1", false, &["*"], "2h ago");
        let shared = leaf_with_data("shared", false, &[], "3d ago");
        let mut root = leaf_with_data("main", true, &["↑"], "1d ago");
        root.children = vec![s1, shared];
        let forest = vec![root];

        let (lines, _) = format_tree_lines(&forest, true);
        // lines[0]=s1 (tip), lines[1]=shared (sibling lane), lines[2]=main (trunk)
        assert_eq!(lines.len(), 3, "expected 3 rows");

        let s1_ind_col = display_col_of(&lines[0], "*", "s1");
        let main_ind_col = display_col_of(&lines[2], "↑", "main");
        assert_eq!(
            s1_ind_col, main_ind_col,
            "indicator column must be aligned:\ns1:   {:?}\nmain: {:?}",
            lines[0], lines[2]
        );
    }

    #[test]
    fn format_tree_lines_activity_column_aligned_across_lanes() {
        // Same setup as above; also verify the activity strings start at the same offset.
        let s1 = leaf_with_data("s1", false, &["*"], "2h ago");
        let shared = leaf_with_data("shared", false, &[], "3d ago");
        let mut root = leaf_with_data("main", true, &["↑"], "1d ago");
        root.children = vec![s1, shared];
        let forest = vec![root];

        let (lines, _) = format_tree_lines(&forest, true);
        assert_eq!(lines.len(), 3);

        // Activity strings are unique enough to find directly.
        let s1_act_col = display_col_of(&lines[0], "2h ago", "s1");
        let shared_act_col = display_col_of(&lines[1], "3d ago", "shared");
        let main_act_col = display_col_of(&lines[2], "1d ago", "main");
        assert_eq!(
            s1_act_col, shared_act_col,
            "activity column must align between s1 and shared:\ns1:     {:?}\nshared: {:?}",
            lines[0], lines[1]
        );
        assert_eq!(
            s1_act_col, main_act_col,
            "activity column must align between s1 and main:\ns1:   {:?}\nmain: {:?}",
            lines[0], lines[2]
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
