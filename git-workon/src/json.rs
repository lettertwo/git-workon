use serde_json::{json, Value};
use workon::WorktreeDescriptor;

/// Convert a WorktreeDescriptor to a JSON value.
///
/// Fields that error during access are represented as `null`.
pub fn worktree_to_json(wt: &WorktreeDescriptor) -> Value {
    json!({
        "name": wt.name(),
        "path": wt.path().to_str(),
        "branch": wt.branch().ok().flatten(),
        "head_commit": wt.head_commit().ok().flatten(),
        "is_dirty": wt.is_dirty().ok(),
        "has_unpushed_commits": wt.has_unpushed_commits().ok(),
        "is_behind_upstream": wt.is_behind_upstream().ok(),
        "has_gone_upstream": wt.has_gone_upstream().ok(),
        "remote": wt.remote().ok().flatten(),
        "remote_branch": wt.remote_branch().ok().flatten(),
        "remote_url": wt.remote_url().ok().flatten(),
        "last_activity": wt.last_activity().ok().flatten(),
    })
}
