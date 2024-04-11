use std::{fmt, path::Path};

use git2::{Repository, Worktree};
use miette::{IntoDiagnostic, Result};

pub struct WorktreeDescriptor {
    worktree: Worktree,
}

impl WorktreeDescriptor {
    fn new(repo: &Repository, name: &str) -> Result<Self> {
        Ok(Self {
            worktree: repo.find_worktree(&name).into_diagnostic()?,
        })
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
