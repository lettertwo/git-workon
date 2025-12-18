use std::{fmt, path::Path};

use git2::{Repository, Worktree};
use miette::{IntoDiagnostic, Result};

use git2::WorktreeAddOptions;
use log::debug;

use super::workon_root;

pub struct WorktreeDescriptor {
    worktree: Worktree,
}

impl WorktreeDescriptor {
    pub fn new(repo: &Repository, name: &str) -> Result<Self> {
        Ok(Self {
            worktree: repo.find_worktree(name).into_diagnostic()?,
        })
    }

    pub fn of(worktree: Worktree) -> Self {
        Self { worktree }
    }

    pub fn name(&self) -> Option<&str> {
        self.worktree.name()
    }

    pub fn path(&self) -> &Path {
        self.worktree.path()
    }

    pub fn branch(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.branch()
    }

    pub fn status(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.status()
    }

    pub fn head_commit(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.head_commit()
    }

    pub fn remote(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote()
    }

    pub fn remote_branch(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_branch()
    }

    pub fn remote_status(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_status()
    }

    pub fn remote_head_commit(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_head_commit()
    }

    pub fn remote_url(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_url()
    }

    pub fn remote_fetch_url(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_fetch_url()
    }

    pub fn remote_push_url(&self) -> Option<&str> {
        unimplemented!()
        // self.worktree.remote_push_url()
    }
}

impl fmt::Debug for WorktreeDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WorktreeDescriptor({:?})", self.worktree.path())
    }
}

impl fmt::Display for WorktreeDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.worktree.path().display())
    }
}

pub fn get_worktrees(repo: &Repository) -> Result<Vec<WorktreeDescriptor>> {
    repo.worktrees()
        .into_diagnostic()?
        .into_iter()
        .map(|name| WorktreeDescriptor::new(repo, name.unwrap_or_default()))
        .collect()
}

pub fn add_worktree(repo: &Repository, branch_name: &str) -> Result<WorktreeDescriptor> {
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
                .map_err(|e| {
                    debug!("remote branch not found: {:?}", e);
                    e
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

    // Git does not support worktree names with slashes in them,
    // so take the base of the branch name as the worktree name.
    let worktree_name = match Path::new(&branch_name).file_name() {
        Some(basename) => basename.to_str().unwrap(),
        None => branch_name,
    };

    let worktree_path = root.join(branch_name);

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));

    debug!(
        "adding worktree {} at {}",
        worktree_name,
        worktree_path.display()
    );

    repo.worktree(worktree_name, worktree_path.as_path(), Some(&opts))
        .map(WorktreeDescriptor::of)
        .into_diagnostic()
}
