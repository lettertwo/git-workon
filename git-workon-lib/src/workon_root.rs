use std::path::Path;

use git2::Repository;

use crate::error::{Result, WorktreeError};

/// Resolve the directory that contains all worktrees for the repository.
///
/// For the standard bare-repo layout (`project/.bare`, `project/main`, …) this
/// returns the `project/` directory — the common ancestor of the `.bare` git
/// directory and any working directory.
///
/// Returns [`WorktreeError::NoParent`] if the path has no parent.
pub fn workon_root(repo: &Repository) -> Result<&Path> {
    let path = repo.path();

    match repo.workdir() {
        Some(workdir) if workdir != path => {
            let repo_ancestors: Vec<_> = path.ancestors().collect();
            let common_root = workdir
                .ancestors()
                .find(|ancestor| repo_ancestors.contains(ancestor));
            if let Some(common_root) = common_root {
                return Ok(common_root);
            }
        }
        _ => {}
    }

    path.parent().ok_or(WorktreeError::NoParent.into())
}
