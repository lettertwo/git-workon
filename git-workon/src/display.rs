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
//! Used by `list` for output and `find` for interactive selection.

use std::path::Path;

use miette::Result;
use unicode_width::UnicodeWidthStr;
use workon::WorktreeDescriptor;

use crate::output::style;

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

    let last_activity = wt
        .last_activity()
        .ok()
        .flatten()
        .map(format_relative_time)
        .unwrap_or_default();

    Ok(WorktreeDisplayRow {
        is_active,
        dir_name,
        branch_annotation,
        indicators,
        last_activity,
    })
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

            let indicators_display = row
                .indicators
                .iter()
                .map(|ind| match ind.as_str() {
                    "*" => style::yellow(ind),
                    "↑" => style::green(ind),
                    "↓" => style::red(ind),
                    "✗" => style::red_bold(ind),
                    _ => ind.clone(),
                })
                .collect::<Vec<_>>()
                .join(" ");
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
