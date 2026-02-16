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
//! Column-aligned output with active marker, indicators, path, and last activity:
//! ```text
//!   main              ./main           2 hours ago
//! → feature-auth   *  ./feature-auth   3 days ago
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
    pub branch_name: String,
    pub indicators: Vec<String>,
    pub path: String,
    pub last_activity: String,
}

/// Build a display row from a worktree descriptor.
pub fn worktree_display_row(
    wt: &WorktreeDescriptor,
    root: &Path,
    current_dir: &Path,
) -> Result<WorktreeDisplayRow> {
    let is_active = current_dir.starts_with(wt.path());

    let branch_name = match wt.branch()? {
        Some(name) => name,
        None => "(detached HEAD)".to_string(),
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

    let rel_path = pathdiff::diff_paths(wt.path(), root)
        .map(|p| format!("./{}", p.display()))
        .unwrap_or_else(|| wt.path().display().to_string());

    let last_activity = wt
        .last_activity()
        .ok()
        .flatten()
        .map(format_relative_time)
        .unwrap_or_default();

    Ok(WorktreeDisplayRow {
        is_active,
        branch_name,
        indicators,
        path: rel_path,
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

    let max_branch = rows
        .iter()
        .map(|r| r.branch_name.width())
        .max()
        .unwrap_or(0);
    let max_indicators = rows
        .iter()
        .map(|r| r.indicators.join(" ").width())
        .max()
        .unwrap_or(0);
    let max_path = rows.iter().map(|r| r.path.width()).max().unwrap_or(0);

    rows.iter()
        .map(|row| {
            let branch = style::bold(&row.branch_name);
            let branch_pad = max_branch - row.branch_name.width();

            let indicators_plain = row.indicators.join(" ");
            let indicators_display = if row.indicators.is_empty() {
                indicators_plain.clone()
            } else {
                row.indicators
                    .iter()
                    .map(|i| match i.as_str() {
                        "*" => style::yellow(i),
                        "↑" => style::green(i),
                        "↓" => style::red(i),
                        "✗" => style::red_bold(i),
                        _ => i.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let indicators_pad = max_indicators - indicators_plain.width();

            let path = style::dim(&row.path);
            let path_pad = max_path - row.path.width();

            let activity = style::dim(&row.last_activity);

            if show_active_marker {
                let marker = if row.is_active {
                    style::green("→")
                } else {
                    " ".to_string()
                };
                format!(
                    "{} {}{} {}{} {}{}  {}",
                    marker,
                    branch,
                    " ".repeat(branch_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    path,
                    " ".repeat(path_pad),
                    activity,
                )
            } else {
                format!(
                    "{}{} {}{} {}{}  {}",
                    branch,
                    " ".repeat(branch_pad),
                    indicators_display,
                    " ".repeat(indicators_pad),
                    path,
                    " ".repeat(path_pad),
                    activity,
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
