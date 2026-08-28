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
//! Graphite-style lane graph, indented by stack membership rather than gutter-marked: column 0
//! holds trunks and ungrouped worktrees, both flush left; every stack member that is not the
//! trunk sits in column 1 or further right. A trunk gathers every stack hanging off it onto its
//! own row (`◎─┴─┴─╯ main`), tallest stack leftmost. `◉`/`◎`/`◯` glyphs encode worktree state;
//! converging lanes close on the fork node's own row (no connector-only rows). Display order is
//! tip-on-top, trunk at the bottom, stacks before ungrouped worktrees. `← here` marks the
//! current worktree.
//!
//! ```text
//!   ◯ app
//!   ◯ feature
//!   │ ◎ shared
//! ◉─┴─╯ main       ← here
//! ◎ scratch
//! ```
//!
//! Used by `list` for output and `find` for interactive selection.

use std::collections::{HashMap, HashSet};
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
    format_aligned_rows_annotated(rows, show_active_marker, &[])
}

/// Like [`format_aligned_rows`], appending a dim trailing annotation per row.
///
/// `annotations` is parallel to `rows`; an empty string (or missing entry) means no
/// annotation for that row. Used by the prune picker to show signal/state notes
/// (e.g. `branch deleted`, `merged into main`, `not prunable`) in the same visual
/// language as the `find`/`list` rows.
pub fn format_aligned_rows_annotated(
    rows: &[WorktreeDisplayRow],
    show_active_marker: bool,
    annotations: &[String],
) -> Vec<String> {
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

            let annotation = annotations
                .get(i)
                .filter(|a| !a.is_empty())
                .map(|a| format!("  {}", style::dim(a)))
                .unwrap_or_default();

            if show_active_marker {
                let marker = if row.is_active {
                    style::green("→")
                } else {
                    " ".to_string()
                };
                format!(
                    "{} {}{}{} {}{}  {}{}{}",
                    marker,
                    prefix,
                    name,
                    " ".repeat(name_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    activity,
                    branch,
                    annotation,
                )
            } else {
                format!(
                    "{}{}{} {}{}  {}{}{}",
                    prefix,
                    name,
                    " ".repeat(name_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    activity,
                    branch,
                    annotation,
                )
            }
        })
        .collect()
}

// ── Tree renderer ─────────────────────────────────────────────────────────────

/// A single node in the stack display tree.
///
/// Roots are trunks (or standalone untracked worktrees). Children are the diffs stacked on that
/// trunk (or on another diff), ordered by subtree size descending (tallest stack leftmost) once
/// the dependency chain is resolved.
pub struct TreeNode {
    /// The branch/diff name — used as the primary label.
    pub branch: String,
    /// Worktree display data. `None` for metadata-only diffs (no checked-out worktree).
    pub row: Option<WorktreeDisplayRow>,
    /// Most-recent activity in this node's subtree (epoch seconds); the tiebreaker once
    /// `subtree_size` is equal, for both root ordering and lane assignment.
    pub subtree_activity: Option<i64>,
    /// Node count of this node's own subtree (including itself). Drives lane assignment and
    /// ordering: the child with the largest `subtree_size` takes the leftmost lane.
    pub subtree_size: usize,
    /// Children (diffs stacked on this branch), in display order.
    pub children: Vec<TreeNode>,
    /// Provider-assigned stack number, set only on a direct child of a trunk (never on the
    /// trunk root itself — `build_tree` merges every stack on one trunk into a single root
    /// node, so the trunk has no single number to show).
    pub stack_number: Option<u64>,
}

impl TreeNode {
    /// Whether this node has a checked-out worktree.
    pub fn has_worktree(&self) -> bool {
        self.row.is_some()
    }
}

/// Build a forest of [`TreeNode`]s from worktrees, their stacks, and the grouping.
///
/// Each distinct trunk gets one root node; all stacks on that trunk hang underneath, ordered
/// tallest subtree first. Untracked worktrees (those in `ungrouped`) become additional
/// root-level leaf nodes, sorted after every trunk (never interleaved with them) by
/// most-recent activity.
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
    // Stack number for each direct child of a trunk (branch whose parent == trunk). Never
    // populated for descendants further down the tree, so a plain lookup at any depth in
    // `build_children` naturally only matches root children — see `TreeNode::stack_number`.
    let mut direct_child_numbers: HashMap<String, u64> = HashMap::new();
    for group in groups {
        let rev = per_trunk_reverse
            .entry(group.stack.trunk.clone())
            .or_default();
        for (child, parent) in &group.stack.parents {
            rev.entry(parent.clone()).or_default().push(child.clone());
        }
        if let Some(number) = group.stack.number {
            for (child, parent) in &group.stack.parents {
                if *parent == group.stack.trunk {
                    direct_child_numbers.insert(child.clone(), number);
                }
            }
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

    // Trunk roots and ungrouped roots are collected separately and only concatenated at the
    // end (trunks first): ungrouped worktrees sort after every stack, never interleaved with
    // them by activity — see `build_tree`'s doc comment.
    let mut trunk_nodes: Vec<TreeNode> = Vec::new();
    // Forest-global: a branch name is unique across the repo, so it belongs in exactly one place
    // in the tree. Overlapping stack groups merged into `per_trunk_reverse` can list the same
    // branch more than once; `visited` ensures the first placement wins and duplicates are dropped.
    let mut visited: HashSet<String> = HashSet::new();

    for trunk in &trunk_set {
        let trunk_idx = branch_to_idx.get(trunk).copied();
        let trunk_row = trunk_idx.and_then(|idx| rows.remove(&idx));
        let trunk_activity = trunk_row.as_ref().and_then(|r| r.activity_epoch);

        let empty_rev: HashMap<String, Vec<String>> = HashMap::new();
        let rev_map = per_trunk_reverse.get(trunk).unwrap_or(&empty_rev);

        // Direct children of the trunk: branches whose parent == trunk
        let direct_children = rev_map.get(trunk.as_str()).cloned().unwrap_or_default();
        let children = build_children(
            &direct_children,
            rev_map,
            &branch_to_idx,
            &mut rows,
            &mut visited,
            &direct_child_numbers,
        );

        let subtree_activity = subtree_max(trunk_activity, &children);
        let subtree_size = subtree_size(&children);

        trunk_nodes.push(TreeNode {
            branch: trunk.clone(),
            row: trunk_row,
            subtree_activity,
            subtree_size,
            children,
            stack_number: None, // never on the trunk root — see `TreeNode::stack_number`.
        });
    }

    // Add ungrouped (untracked) worktrees as leaf nodes.
    // If a trunk's worktree is in ungrouped, it was already used above; skip it.
    let trunk_set_ref: std::collections::HashSet<&String> = trunk_set.iter().collect();
    let mut ungrouped_nodes: Vec<TreeNode> = Vec::new();
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
        ungrouped_nodes.push(TreeNode {
            branch: branch.clone().unwrap_or_else(|| row.dir_name.clone()),
            subtree_activity: epoch,
            subtree_size: 1,
            row: Some(row),
            children: vec![],
            stack_number: None, // ungrouped: not part of a numbered stack.
        });
    }

    // Trunks sort tallest-subtree-first (mirrors the lane-assignment rule so a trunk's rank
    // among its peers matches how its own children fan out); ungrouped roots sort by
    // most-recent activity, as before, and always follow every trunk.
    trunk_nodes.sort_by(node_order);
    ungrouped_nodes.sort_by_key(|n| std::cmp::Reverse(n.subtree_activity));

    let mut forest = trunk_nodes;
    forest.extend(ungrouped_nodes);
    forest
}

/// Ordering used both for root placement and lane assignment: largest subtree first (rule 4,
/// "tall stack leftmost"), most-recent activity breaks a size tie, branch name breaks the rest
/// for determinism.
fn node_order(a: &TreeNode, b: &TreeNode) -> std::cmp::Ordering {
    b.subtree_size
        .cmp(&a.subtree_size)
        .then_with(|| b.subtree_activity.cmp(&a.subtree_activity))
        .then_with(|| a.branch.cmp(&b.branch))
}

/// Recursively build child nodes for a list of branch names.
///
/// `visited` guards against a branch being emitted more than once. A well-formed topology has a
/// single parent per branch, but overlapping stack groups sharing a trunk merge into `rev_map`
/// with duplicate edges (see [`build_tree`]); without this guard `build_children` would re-expand
/// the shared subtree once per duplicate, multiplying rows combinatorially at every fork. Every
/// BFS in `graphite.rs` carries the same guard — this is the one tree walk that also needs it.
fn build_children(
    branch_names: &[String],
    rev_map: &HashMap<String, Vec<String>>,
    branch_to_idx: &HashMap<String, usize>,
    rows: &mut HashMap<usize, WorktreeDisplayRow>,
    visited: &mut HashSet<String>,
    direct_child_numbers: &HashMap<String, u64>,
) -> Vec<TreeNode> {
    let mut nodes = Vec::new();
    for branch in branch_names {
        if !visited.insert(branch.clone()) {
            continue;
        }
        let idx = branch_to_idx.get(branch).copied();
        let row = idx.and_then(|i| rows.remove(&i));
        let epoch = row.as_ref().and_then(|r| r.activity_epoch);

        let grandchildren_names = rev_map.get(branch.as_str()).cloned().unwrap_or_default();
        let children = build_children(
            &grandchildren_names,
            rev_map,
            branch_to_idx,
            rows,
            visited,
            direct_child_numbers,
        );
        let subtree_activity = subtree_max(epoch, &children);
        let subtree_size = subtree_size(&children);

        nodes.push(TreeNode {
            branch: branch.clone(),
            row,
            subtree_activity,
            subtree_size,
            children,
            // Only true direct children of a trunk are keys here (see `build_tree`), so a
            // plain lookup at any recursion depth naturally yields `None` past the root.
            stack_number: direct_child_numbers.get(branch).copied(),
        });
    }
    nodes
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

/// Node count of a subtree given its children's own `subtree_size`s, plus 1 for the node itself.
/// Drives rule 4 ("tall stack leftmost"): the child with the most nodes underneath it (and
/// itself) takes the leftmost lane.
fn subtree_size(children: &[TreeNode]) -> usize {
    1 + children.iter().map(|c| c.subtree_size).sum::<usize>()
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
/// = passthrough columns (`own_lane × 2`) + glyph width (1) + closing connector
/// (`closing_count × 2`). Lane 0 — trunks and ungrouped worktrees — has no leading columns at
/// all, so it renders flush left; indentation is the only signal of stack membership.
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
        + stack_number_suffix(node.stack_number)
            .map(|s| s.width())
            .unwrap_or(0)
}

/// The plain (unstyled) `" #N"` suffix for a stack number, or `None` when there isn't one.
/// Kept separate from styling so [`lane_content_width`] can measure it without depending on
/// [`style::dim`]'s color-state side effects.
fn stack_number_suffix(number: Option<u64>) -> Option<String> {
    number.map(|n| format!(" #{n}"))
}

/// Render the gutter string for a lane row.
///
/// Produces: `(│ | )* glyph [─╯ | ─┴─╯ | …]`
/// - Each column before `own_lane` renders as `│ ` (passthrough) or `  ` (empty/closed).
/// - `own_lane` renders as `glyph`. Lane 0 (trunks, ungrouped worktrees) has no columns before
///   it, so it sits flush left — indentation alone signals stack membership.
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
            // Forest roots are bases (rule 1): a trunk never shares its lane with a child, so
            // every child of a root opens its own lane instead of one of them inheriting lane 0.
            emit_lane_rows(node, 0, &[], true, include, &mut rows);
        }
    }
    rows
}

/// Recursively emit `LaneRow`s for `node` and its subtree (tip-on-top: children first, then node).
///
/// - `own_lane`: lane assigned to this node.
/// - `passthrough_lanes`: lanes open above that pass through this subtree unchanged.
/// - `is_base`: true for a forest root (trunk or ungrouped worktree). A base's children never
///   inherit its lane — column 0 is reserved for bases (rule 1), so every child of a base opens
///   a lane of its own (rule 2). A non-base node still hands its lane down to its tallest child
///   (rule 2: "the rest stack vertically above it in the same column"), exactly as before.
/// - `include`: returns true when a node (or its subtree) should appear in output.
///
/// Children are ordered by `node_order` (tallest subtree first, rule 4) before lanes are
/// assigned, so the lane closest to `own_lane` always holds the largest subtree.
///
/// Sibling lanes are assigned relative to `own_lane` (`own_lane + 1`, `+ 2`, …) rather than from a
/// forest-global counter. A sibling lane is only open between its subtree's first row and this
/// node's converging row, so the same index is free to be reused by any other subtree — and the
/// `─┴─╯` connector, which is drawn positionally from `closing_count`, only lines up with the
/// sibling lanes when they sit immediately to the right of `own_lane`.
fn emit_lane_rows<'a, F>(
    node: &'a TreeNode,
    own_lane: usize,
    passthrough_lanes: &[usize],
    is_base: bool,
    include: &F,
    rows: &mut Vec<LaneRow<'a>>,
) where
    F: Fn(&TreeNode) -> bool,
{
    // Visible children, tallest subtree first.
    let mut visible: Vec<&TreeNode> = node.children.iter().filter(|c| include(c)).collect();
    visible.sort_by(|a, b| node_order(a, b));

    if visible.is_empty() {
        rows.push(LaneRow {
            node,
            own_lane,
            passthrough_lanes: passthrough_lanes.to_vec(),
            closing_count: 0,
        });
        return;
    }

    // A base's children all open new lanes (rule 1); a stack member hands its lane down to its
    // tallest child (rule 2) and only the rest become siblings.
    let (primary, siblings): (Option<&TreeNode>, &[&TreeNode]) = if is_base {
        (None, &visible[..])
    } else {
        (Some(visible[0]), &visible[1..])
    };

    // 1. Emit the primary subtree (same lane, same passthrough), if there is one.
    if let Some(primary) = primary {
        emit_lane_rows(primary, own_lane, passthrough_lanes, false, include, rows);
    }

    // 2. Emit each sibling's subtree, in order, claiming lanes immediately right of `own_lane`.
    //    Passthrough for sibling i = passthrough_lanes ∪ {own_lane, if it's a stack member's own
    //    lane} ∪ {earlier siblings' lanes}. A base's own lane (0) never passes through its
    //    children's rows — nothing below a base shares its lane.
    let mut opened: Vec<usize> = if is_base { Vec::new() } else { vec![own_lane] };
    for (si, &sib) in siblings.iter().enumerate() {
        let sib_lane = own_lane + 1 + si;
        let mut sib_pass: Vec<usize> = passthrough_lanes.to_vec();
        sib_pass.extend(opened.iter().copied());
        sib_pass.sort_unstable();
        emit_lane_rows(sib, sib_lane, &sib_pass, false, include, rows);
        opened.push(sib_lane);
    }

    // 3. Emit this node — every child lane converges here (a base) or every sibling lane
    //    converges here (a stack member; the primary lane continues through unclosed).
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

        // Stack number annotation: dim ` #N` (degrades to plain text under NO_COLOR via
        // `style::dim`), set only on a direct child of a trunk — see `TreeNode::stack_number`.
        let number_ann = stack_number_suffix(node.stack_number).map(|s| style::dim(&s));

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

        // Assemble line: [gutter] [label][path][number][pad]  [indicators][ind_pad]  [activity][here]
        let path_str = path_ann.unwrap_or_default();
        let number_str = number_ann.unwrap_or_default();
        let line = if node.row.is_some() {
            format!(
                "{} {}{}{}{}  {}{}  {}{}",
                gutter,
                label,
                path_str,
                number_str,
                " ".repeat(content_pad),
                ind_str,
                " ".repeat(ind_pad),
                activity,
                here,
            )
        } else {
            // Metadata-only: gutter + label + number, no indicator/time columns.
            format!("{} {}{}", gutter, label, number_str)
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

    /// Pin color off for tests that assert exact rendered strings. Color support is
    /// process-global and env-sensitive — FORCE_COLOR enables styling even when stdout
    /// is not a TTY — which would embed ANSI codes in the asserted output.
    fn no_color() {
        crate::output::set_no_color(true);
    }

    /// A worktree leaf with no children. Whether it ends up flush left (a base: trunk or
    /// ungrouped worktree) or indented (a stack member) depends entirely on where it's placed in
    /// the forest — a root is a base, a child is a member — not on any field of the node itself.
    /// Use [`ungrouped_leaf`] as a readability alias when a leaf plays the "untracked worktree"
    /// role in a test.
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
            subtree_size: 1,
            children: vec![],
            stack_number: None,
        }
    }

    /// Readability alias for [`leaf`]: a root-level leaf standing in for an untracked worktree.
    fn ungrouped_leaf(branch: &str, is_active: bool) -> TreeNode {
        leaf(branch, is_active)
    }

    fn meta(branch: &str) -> TreeNode {
        TreeNode {
            branch: branch.to_string(),
            row: None,
            subtree_activity: None,
            subtree_size: 1,
            children: vec![],
            stack_number: None,
        }
    }

    /// Attach `children` to `node`, recomputing `subtree_size`/`subtree_activity` the way
    /// `build_tree` does. Tests build trees top-down (`let mut root = leaf(...); set_children(&mut
    /// root, vec![...])`), so without this the hand-built `subtree_size` would stay `1` and lane
    /// assignment (rule 4, tallest subtree leftmost) would silently use the wrong ordering key.
    fn set_children(node: &mut TreeNode, children: Vec<TreeNode>) {
        node.subtree_activity = subtree_max(node.subtree_activity, &children);
        node.subtree_size = subtree_size(&children);
        node.children = children;
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
        set_children(&mut root, vec![leaf("feature", false)]);
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());
        assert_eq!(result.keys, vec!["feature", "main"]);
    }

    #[test]
    fn render_tree_empty_query_cursor_on_active_node() {
        // Active node is "feature"; tip-on-top puts it at index 0.
        let mut root = leaf("main", false);
        set_children(&mut root, vec![leaf("feature", true)]);
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
        set_children(&mut root, vec![leaf("feature", false)]);
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
        set_children(&mut zebra, vec![leaf("zoo", false)]);
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
        // trunk (a base) → [child-a, child-b, child-c]; all match "child-".
        // A base's children never share its lane (rule 1): each opens its own — lane 1, 2, 3.
        // Equal-size children tie-break alphabetically: child-a, child-b, child-c.
        // Emission order: child-a, child-b, child-c, trunk (trunk last, tip-on-top).
        // Trunk gathers all 3 lanes on its own row → its line contains "─┴─┴─╯".
        let mut root = meta("trunk");
        set_children(
            &mut root,
            vec![
                leaf("child-a", false),
                leaf("child-b", false),
                leaf("child-c", false),
            ],
        );
        let forest = vec![root];
        let result = render_tree(&forest, "child-", &matcher());

        assert_eq!(result.keys, vec!["child-a", "child-b", "child-c", "trunk"]);
        let trunk_line = result.lines.last().unwrap();
        assert!(
            trunk_line.contains("─┴─┴─╯"),
            "trunk gathering 3 stacks should contain ─┴─┴─╯, got: {trunk_line}"
        );
    }

    #[test]
    fn render_tree_lanes_correct_when_middle_child_filtered() {
        // trunk (a base) → [child-a, unrelated, child-c]; "child" matches child-a and child-c.
        // After filtering, both are the trunk's children and neither shares its lane (rule 1):
        // child-a opens lane 1, child-c opens lane 2.
        // Emission: child-a, child-c, trunk; trunk closes 2 lanes → "─╯".
        let mut root = meta("trunk");
        set_children(
            &mut root,
            vec![
                leaf("child-a", false),
                leaf("unrelated", false), // won't match
                leaf("child-c", false),
            ],
        );
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
    fn render_tree_linear_stack_is_indented_flush_left_trunk() {
        // main (a base) → s1 → s2: main's one child never shares its lane (rule 1), so s1/s2
        // sit one column right of it — no fork, so no "│" passthrough anywhere, just indentation.
        no_color();
        let mut s1 = leaf("s1", false);
        set_children(&mut s1, vec![leaf("s2", false)]);
        let mut root = leaf("main", false);
        set_children(&mut root, vec![s1]);
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        // Tip-on-top: s2, s1, main
        assert_eq!(result.keys, vec!["s2", "s1", "main"]);
        for line in &result.lines[..2] {
            assert!(
                line.starts_with("  "),
                "stack members must be indented at least one column, got: {line}"
            );
            assert!(
                !line.contains('│'),
                "a linear stack has no fork, so no passthrough lane, got: {line}"
            );
        }
        assert!(
            result.lines[2].starts_with("◎─╯ main"),
            "the trunk is a base: flush left, closing the one stack lane, got: {}",
            result.lines[2]
        );
    }

    #[test]
    fn render_tree_sibling_stack_closes_on_trunk_row() {
        // main (a base) → [s1, shared]: neither shares the trunk's lane (rule 1), both open new
        // lanes. Equal size ties break alphabetically: s1 (lane 1) before shared (lane 2).
        // Emission: s1, shared, main. main's line gathers both lanes → "─┴─╯".
        let mut root = leaf("main", false);
        set_children(&mut root, vec![leaf("s1", false), leaf("shared", false)]);
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        assert_eq!(result.keys, vec!["s1", "shared", "main"]);
        // shared is in lane 2 — its line has "│ " passthrough for s1's lane.
        assert!(
            result.lines[1].contains("│ "),
            "shared (later lane) should show │ passthrough, got: {}",
            result.lines[1]
        );
        // main gathers both lanes on its own row.
        let main_line = result.lines.last().unwrap();
        assert!(
            main_line.contains("─┴─╯"),
            "main should gather both stacks with ─┴─╯, got: {main_line}"
        );
    }

    #[test]
    fn render_tree_mid_stack_fork_closes_on_fork_node() {
        // main (a base) → a1 → a2 → [b1 (primary), c1 (sibling)].
        // a1 is main's only child, so it opens lane 1 (rule 1); everything under it stays in
        // that same column until a2's fork.
        // Emission: b1, c1, a2, a1, main. a2 closes lane 2 → "─╯".
        let mut a2 = leaf("a2", false);
        set_children(&mut a2, vec![leaf("b1", false), leaf("c1", false)]);
        let mut a1 = leaf("a1", false);
        set_children(&mut a1, vec![a2]);
        let mut root = leaf("main", false);
        set_children(&mut root, vec![a1]);
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        assert_eq!(result.keys, vec!["b1", "c1", "a2", "a1", "main"]);
        let a2_line = &result.lines[2];
        assert!(
            a2_line.contains("─╯"),
            "a2 (fork node) should close sibling lane with ─╯, got: {a2_line}"
        );
        // a1 is back to a single lane, with no fork open above it — no passthrough at all.
        assert!(
            !result.lines[3].contains('│'),
            "a1 after the fork has closed should show no passthrough, got: {}",
            result.lines[3]
        );
    }

    #[test]
    fn render_tree_fork_under_primary_child_reuses_sibling_lane() {
        // main (a base) → [a, z]; a → [p (primary), q (sibling)].
        // a's subtree (3 nodes) is taller than z's (1), so a takes lane 1 and z lane 2 (rule 4).
        // z's lane is open only across z's own row — which comes *below* a's subtree — so q may
        // reuse lane 2. Assigning q a fresh forest-global lane instead over-indents its row and
        // leaves a's "─╯" pointing at an empty column.
        no_color();
        let mut a = leaf("a", false);
        set_children(&mut a, vec![leaf("p", false), leaf("q", false)]);
        let mut root = leaf("main", false);
        set_children(&mut root, vec![a, leaf("z", false)]);
        let forest = vec![root];
        let result = render_tree(&forest, "", &matcher());

        assert_eq!(result.keys, vec!["p", "q", "a", "z", "main"]);
        // p is a's primary child: same lane as a, one column right of the trunk, no passthrough.
        let p_line = &result.lines[0];
        assert!(
            p_line.starts_with("  ◎ p"),
            "p should sit in a's lane with no passthrough, got: {p_line}"
        );
        // q is a's sibling: one lane further right, with a's lane passing through.
        let q_line = &result.lines[1];
        assert!(
            q_line.starts_with("  │ ◎ q"),
            "q should sit one lane right of a, with a's lane passing through, got: {q_line}"
        );
        let a_line = &result.lines[2];
        assert!(
            a_line.starts_with("  ◎─╯ a"),
            "a's connector should close onto q's lane, got: {a_line}"
        );
        // z reuses q's lane index once q's subtree has closed — same column, no over-indent.
        let z_line = &result.lines[3];
        assert!(
            z_line.starts_with("  │ ◎ z"),
            "z should sit in the same lane column q used, with a's lane passing through, got: {z_line}"
        );
        let main_line = &result.lines[4];
        assert!(
            main_line.starts_with("◎─┴─╯ main"),
            "main should gather both a's and z's lanes, got: {main_line}"
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
            subtree_size: 1,
            children: vec![],
            stack_number: None,
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

    // ── build_tree: overlapping same-trunk groups (ghost-tail regression backstop) ──

    /// Collect every branch name in a forest, once per node occurrence.
    fn collect_branches(forest: &[TreeNode], out: &mut Vec<String>) {
        for node in forest {
            out.push(node.branch.clone());
            collect_branches(&node.children, out);
        }
    }

    fn meta_group(diffs: &[&str], parents: &[(&str, &str)]) -> workon::StackGroup {
        workon::StackGroup {
            stack: workon::Stack {
                trunk: "main".to_string(),
                diffs: diffs.iter().map(|s| s.to_string()).collect(),
                current: diffs.first().copied().unwrap_or_default().to_string(),
                parents: parents
                    .iter()
                    .map(|(c, p)| (c.to_string(), p.to_string()))
                    .collect(),
                number: None,
            },
            members: vec![],
        }
    }

    #[test]
    fn build_tree_dedupes_overlapping_same_trunk_groups() {
        // Two groups on trunk "main" describe one physical stack: a full one (carrying a ghost
        // tail off `base`'s second fork) and a pruned subset. Their edges merge into
        // `per_trunk_reverse` with duplicates; the `visited` guard in `build_children` must still
        // emit each branch exactly once instead of re-expanding the shared subtree at every fork
        // — the "thousands of broken lines" bug when ghost metadata makes the two paths diverge.
        let full = meta_group(
            &["base", "live-leaf", "ghost-mid", "ghost-leaf"],
            &[
                ("base", "main"),
                ("live-leaf", "base"),
                ("ghost-mid", "base"),
                ("ghost-leaf", "ghost-mid"),
            ],
        );
        let subset = meta_group(
            &["base", "live-leaf"],
            &[("base", "main"), ("live-leaf", "base")],
        );

        let groups = vec![full, subset];
        let forest = build_tree(&[], &groups, &[], Path::new("/repo"), Path::new("/repo"));

        let mut branches = Vec::new();
        collect_branches(&forest, &mut branches);
        branches.sort();
        let mut unique = branches.clone();
        unique.dedup();
        assert_eq!(
            branches, unique,
            "each branch must appear once despite overlapping groups; got {branches:?}"
        );
        assert_eq!(
            branches,
            vec!["base", "ghost-leaf", "ghost-mid", "live-leaf", "main"]
        );
    }

    fn numbered_group(number: u64, diffs: &[&str], parents: &[(&str, &str)]) -> workon::StackGroup {
        let mut group = meta_group(diffs, parents);
        group.stack.number = Some(number);
        group
    }

    fn find_node<'a>(forest: &'a [TreeNode], branch: &str) -> &'a TreeNode {
        fn find<'a>(nodes: &'a [TreeNode], branch: &str) -> Option<&'a TreeNode> {
            for node in nodes {
                if node.branch == branch {
                    return Some(node);
                }
                if let Some(found) = find(&node.children, branch) {
                    return Some(found);
                }
            }
            None
        }
        find(forest, branch).unwrap_or_else(|| panic!("{branch} not found in forest"))
    }

    #[test]
    fn build_tree_sets_stack_number_only_on_direct_trunk_child() {
        // "base" is the direct child of trunk "main"; "mid" and "top" are descendants further
        // down the same stack. Only "base" should carry the stack's number — never the trunk
        // root ("main" merges every stack on the trunk, so it has no single number) and never
        // a deeper descendant.
        let group = numbered_group(
            12,
            &["base", "mid", "top"],
            &[("base", "main"), ("mid", "base"), ("top", "mid")],
        );
        let groups = vec![group];
        let forest = build_tree(&[], &groups, &[], Path::new("/repo"), Path::new("/repo"));

        assert_eq!(find_node(&forest, "main").stack_number, None);
        assert_eq!(find_node(&forest, "base").stack_number, Some(12));
        assert_eq!(find_node(&forest, "mid").stack_number, None);
        assert_eq!(find_node(&forest, "top").stack_number, None);
    }

    #[test]
    fn format_tree_lines_renders_dim_stack_number_suffix() {
        no_color();
        let mut root = meta("base");
        root.stack_number = Some(12);
        let (lines, _) = format_tree_lines(&[root], false);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with("base #12"),
            "expected a trailing ' #12' suffix (plain text under NO_COLOR), got: {:?}",
            lines[0]
        );
    }

    #[test]
    fn format_tree_lines_stack_member_indented_trunk_and_ungrouped_flush_left() {
        // Column 0 is reserved for bases (rule 1): the trunk and an ungrouped worktree both
        // render flush left; "feature", a stack member that isn't the trunk, is indented at
        // least one column (rule 2) — indentation alone signals stack membership now, not a
        // gutter marker.
        no_color();
        let mut trunk = leaf("main", false);
        set_children(&mut trunk, vec![leaf("feature", false)]);
        let forest = vec![trunk, ungrouped_leaf("scratch", false)];
        let (lines, selection) = format_tree_lines(&forest, false);

        assert_eq!(selection, vec!["feature", "main", "scratch"]);
        let feature_line = &lines[0];
        let main_line = &lines[1];
        let scratch_line = &lines[2];

        assert!(
            feature_line.starts_with("  "),
            "stack member should be indented at least one column, got: {feature_line}"
        );
        assert!(
            main_line.starts_with("◎─╯ main") || main_line.starts_with("◉─╯ main"),
            "the trunk is a base: flush left, closing its one stack lane, got: {main_line}"
        );
        assert!(
            !scratch_line.starts_with(' ') && !scratch_line.starts_with('│'),
            "ungrouped worktree is also a base: flush left, no indent, got: {scratch_line}"
        );
    }

    #[test]
    fn format_tree_lines_indicator_column_aligned_across_lanes() {
        // Forest: main (a base) → [s1, shared], neither sharing main's lane (rule 1); equal size
        // ties break alphabetically, so s1 opens lane 1 and shared lane 2.
        // Emission order (tip-on-top): s1, shared, main
        //
        // s1 gutter   = "  ◎"     (width 3) → narrow
        // main gutter = "◉─┴─╯"  (width 5) → wide (gathers both lanes)
        // shared has no indicators; s1 has "*", main has "↑".
        //
        // Despite the different gutter widths, the indicator column must start at
        // the same byte offset in every row that has indicators.
        no_color();
        let s1 = leaf_with_data("s1", false, &["*"], "2h ago");
        let shared = leaf_with_data("shared", false, &[], "3d ago");
        let mut root = leaf_with_data("main", true, &["↑"], "1d ago");
        set_children(&mut root, vec![s1, shared]);
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
        no_color();
        let s1 = leaf_with_data("s1", false, &["*"], "2h ago");
        let shared = leaf_with_data("shared", false, &[], "3d ago");
        let mut root = leaf_with_data("main", true, &["↑"], "1d ago");
        set_children(&mut root, vec![s1, shared]);
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

    // ── target renders (indentation-as-membership) ───────────────────────────

    #[test]
    fn format_tree_lines_matches_three_stack_target_render() {
        // main gathers 3 stacks: a 3-branch chain (tallest, lane 1), and two single-branch
        // stacks (lanes 2 and 3, ordered by activity since their subtree sizes tie), plus two
        // ungrouped worktrees that must sort after every stack, not interleaved by activity.
        no_color();

        let mut m11_view_state = leaf("m11-view-state", true);
        m11_view_state.row = Some(WorktreeDisplayRow {
            is_active: true,
            dir_name: "git-workon-review".to_string(),
            branch_annotation: None,
            indicators: vec![],
            last_activity: String::new(),
            activity_epoch: None,
        });
        let mut m11_yank_split = meta("m11-yank-split");
        set_children(&mut m11_yank_split, vec![m11_view_state]);
        let mut review_scaffold = meta("review-scaffold");
        set_children(&mut review_scaffold, vec![m11_yank_split]);

        let mut gt_support_v1 = meta("gt-support-v1");
        gt_support_v1.subtree_activity = Some(2); // more recently active than gh-stack-support
        let mut gh_stack_support = leaf("gh-stack-support", false);
        gh_stack_support.subtree_activity = Some(1);

        let mut main = leaf("main", false);
        set_children(
            &mut main,
            vec![review_scaffold, gt_support_v1, gh_stack_support],
        );

        let mut acceptance_probe = leaf("acceptance-probe", false);
        acceptance_probe.subtree_activity = Some(20);
        let mut review_tui = leaf("review-tui", false);
        review_tui.subtree_activity = Some(10);

        let forest = vec![main, acceptance_probe, review_tui];
        let (lines, selection) = format_tree_lines(&forest, true);

        assert_eq!(
            selection,
            vec![
                "m11-view-state",
                "m11-yank-split",
                "review-scaffold",
                "gt-support-v1",
                "gh-stack-support",
                "main",
                "acceptance-probe",
                "review-tui",
            ]
        );

        // Metadata-only rows render without column padding, so an exact match is safe.
        assert_eq!(lines[1], "  ◯ m11-yank-split");
        assert_eq!(lines[2], "  ◯ review-scaffold");
        assert_eq!(lines[3], "  │ ◯ gt-support-v1");
        // Worktree rows carry trailing column padding (indicator/activity columns); only the
        // load-bearing gutter/label prefix is pinned.
        assert!(
            lines[0].starts_with("  ◉ m11-view-state  ./git-workon-review"),
            "got: {}",
            lines[0]
        );
        assert!(lines[0].ends_with("← here"), "got: {}", lines[0]);
        assert!(
            lines[4].starts_with("  │ │ ◎ gh-stack-support"),
            "got: {}",
            lines[4]
        );
        assert!(lines[5].starts_with("◎─┴─┴─╯ main"), "got: {}", lines[5]);
        assert!(
            lines[6].starts_with("◎ acceptance-probe"),
            "got: {}",
            lines[6]
        );
        assert!(lines[7].starts_with("◎ review-tui"), "got: {}", lines[7]);
    }

    #[test]
    fn format_tree_lines_matches_single_stack_target_render() {
        // The common case: one stack on main, no forks, so no "│" anywhere in the stack itself —
        // just indentation. Ungrouped worktrees follow, sorted by activity, including the
        // active one (no "◎"/"◯" distinction from being in a stack; only column 0 vs. indented
        // signals membership).
        no_color();

        let mut m11_view_state = leaf("m11-view-state", false);
        m11_view_state.row = Some(WorktreeDisplayRow {
            is_active: false,
            dir_name: "git-workon-review".to_string(),
            branch_annotation: None,
            indicators: vec![],
            last_activity: String::new(),
            activity_epoch: None,
        });
        let mut m11_yank_split = meta("m11-yank-split");
        set_children(&mut m11_yank_split, vec![m11_view_state]);
        let mut review_scaffold = meta("review-scaffold");
        set_children(&mut review_scaffold, vec![m11_yank_split]);

        let mut main = leaf("main", false);
        set_children(&mut main, vec![review_scaffold]);

        let mut gh_stack_support = leaf("gh-stack-support", true);
        gh_stack_support.subtree_activity = Some(30);
        let mut review_tui = leaf("review-tui", false);
        review_tui.subtree_activity = Some(20);
        let mut gt_support_v1 = leaf("gt-support-v1", false);
        gt_support_v1.subtree_activity = Some(10);

        let forest = vec![main, gh_stack_support, review_tui, gt_support_v1];
        let (lines, selection) = format_tree_lines(&forest, true);

        assert_eq!(
            selection,
            vec![
                "m11-view-state",
                "m11-yank-split",
                "review-scaffold",
                "main",
                "gh-stack-support",
                "review-tui",
                "gt-support-v1",
            ]
        );

        assert!(
            lines[0].starts_with("  ◎ m11-view-state  ./git-workon-review"),
            "got: {}",
            lines[0]
        );
        assert_eq!(lines[1], "  ◯ m11-yank-split");
        assert_eq!(lines[2], "  ◯ review-scaffold");
        assert!(lines[3].starts_with("◎─╯ main"), "got: {}", lines[3]);
        assert!(
            lines[4].starts_with("◉ gh-stack-support"),
            "got: {}",
            lines[4]
        );
        assert!(lines[4].ends_with("← here"), "got: {}", lines[4]);
        assert!(lines[5].starts_with("◎ review-tui"), "got: {}", lines[5]);
        assert!(lines[6].starts_with("◎ gt-support-v1"), "got: {}", lines[6]);
    }

    #[test]
    fn format_tree_lines_matches_mid_stack_fork_target_render() {
        // main → a → [b (primary), c (sibling)]: a is main's only child (rule 1, opens lane 1);
        // b and c fork under a exactly as any non-base fork does. c's lane passes through on the
        // row below it (rule 5) so the "┴"-less "─╯" on a's own row has a line to trace up to.
        no_color();
        let mut a = meta("a");
        set_children(&mut a, vec![meta("b"), meta("c")]);
        let mut main = leaf("main", false);
        set_children(&mut main, vec![a]);
        let forest = vec![main];
        let (lines, selection) = format_tree_lines(&forest, false);

        assert_eq!(selection, vec!["b", "c", "a", "main"]);
        assert_eq!(lines[0], "  ◯ b");
        assert_eq!(lines[1], "  │ ◯ c");
        assert_eq!(lines[2], "  ◯─╯ a");
        assert!(lines[3].starts_with("◎─╯ main"), "got: {}", lines[3]);
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

    // ── format_aligned_rows_annotated ─────────────────────────────────────────

    #[test]
    fn format_aligned_rows_annotated_appends_annotation() {
        no_color();
        let rows = vec![flat_row("feature", false)];
        let lines = format_aligned_rows_annotated(&rows, false, &["branch deleted".to_string()]);
        assert!(
            lines[0].ends_with("branch deleted"),
            "annotation should be appended at the end of the row, got: {:?}",
            lines[0]
        );
        assert!(lines[0].starts_with("./feature"));
    }

    #[test]
    fn format_aligned_rows_annotated_empty_annotation_matches_plain() {
        // An empty annotation string (and a missing entry) must render identically
        // to the un-annotated formatter.
        no_color();
        let rows = vec![flat_row("feature", false), flat_row("other", false)];
        let plain = format_aligned_rows(&rows, false);
        let annotated = format_aligned_rows_annotated(&rows, false, &[String::new()]);
        assert_eq!(plain, annotated);
    }

    #[test]
    fn format_aligned_rows_annotated_preserves_name_alignment() {
        // The annotation is trailing-only: the name/indicator columns must still be
        // aligned across rows of different name widths.
        no_color();
        let mut short = flat_row("a", false);
        short.indicators = vec!["*".to_string()];
        short.last_activity = "1h ago".to_string();
        let mut long = flat_row("long-name", false);
        long.indicators = vec!["*".to_string()];
        long.last_activity = "2h ago".to_string();
        let rows = vec![short, long];

        let lines = format_aligned_rows_annotated(
            &rows,
            false,
            &["gone".to_string(), "not prunable".to_string()],
        );
        let col_a = display_col_of(&lines[0], "*", "short row");
        let col_b = display_col_of(&lines[1], "*", "long row");
        assert_eq!(
            col_a, col_b,
            "indicator column must stay aligned:\nshort: {:?}\nlong:  {:?}",
            lines[0], lines[1]
        );
        assert!(lines[0].ends_with("gone"));
        assert!(lines[1].ends_with("not prunable"));
    }
}
