use git2::{Repository, Worktree, WorktreeAddOptions};
use log::debug;
use miette::{IntoDiagnostic, Result};

use super::workon_root;

pub fn add_worktree(repo: &Repository, branch: &str) -> Result<Worktree> {
    // git worktree add <branch>
    debug!("Adding worktree {}", branch);
    let reference = repo
        .find_branch(branch, git2::BranchType::Local)
        .into_diagnostic()?
        .into_reference();
    let root = workon_root(repo)?;
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    Ok(repo
        .worktree(branch, root.join(branch).as_path(), Some(&opts))
        .into_diagnostic()?)
}
