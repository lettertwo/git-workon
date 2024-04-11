use git2::{Repository, Worktree, WorktreeAddOptions};
use log::debug;
use miette::{bail, IntoDiagnostic, Result};

use super::workon_root;

pub fn add_worktree(repo: &Repository, branch_name: &str) -> Result<Worktree> {
    // git worktree add <branch>
    debug!("looking for local branch {:?}", branch_name);

    let branch = repo
        .find_branch(branch_name, git2::BranchType::Local)
        .into_diagnostic()
        .or_else(|e| {
            debug!("local branch not found: {:?}", e);
            debug!("looking for remote branch {:?}", branch_name);
            repo.find_branch(branch_name, git2::BranchType::Remote)
                .into_diagnostic()
                .or_else(|e| {
                    debug!("remote branch not found: {:?}", e);
                    Err(e)
                })
        })
        .ok()
        .unwrap_or_else(|| {
            debug!("creating new local branch {:?}", branch_name);
            let commit = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch(branch_name, &commit, false)
                .into_diagnostic()
                .unwrap()
        });

    let reference = branch.into_reference();

    let root = workon_root(repo)?;
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    debug!("adding worktree at {}", root.join(branch_name).display());
    repo.worktree(branch_name, root.join(branch_name).as_path(), Some(&opts))
        .into_diagnostic()
}
