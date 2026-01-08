use miette::Result;
use workon::WorktreeDescriptor;

/// Format a worktree for display with status indicators
/// Format: "branch-name * ↑ ↓" (shows indicators only when applicable)
pub fn format_worktree_item(wt: &WorktreeDescriptor) -> Result<String> {
    let branch_name = match wt.branch()? {
        Some(name) => name,
        None => "(detached HEAD)".to_string(),
    };

    let mut indicators = Vec::new();

    // * = dirty
    if wt.is_dirty().unwrap_or(false) {
        indicators.push("*");
    }

    // ↑ = ahead (unpushed commits)
    if wt.has_unpushed_commits().unwrap_or(false) {
        indicators.push("↑");
    }

    // ↓ = behind upstream
    if wt.is_behind_upstream().unwrap_or(false) {
        indicators.push("↓");
    }

    // ✗ = upstream gone
    if wt.has_gone_upstream().unwrap_or(false) {
        indicators.push("✗");
    }

    if indicators.is_empty() {
        Ok(branch_name)
    } else {
        Ok(format!("{} {}", branch_name, indicators.join(" ")))
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
