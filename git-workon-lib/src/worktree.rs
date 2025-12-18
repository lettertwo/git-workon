use std::{fmt, fs::create_dir_all, path::Path};

use git2::{Repository, Worktree};
use miette::{IntoDiagnostic, Result};

use git2::WorktreeAddOptions;
use log::debug;

use super::workon_root;

/// Options for creating a new worktree
#[derive(Debug, Clone, Default)]
pub struct AddWorktreeOptions {
    /// Create an orphan branch (no parent commits)
    pub orphan: bool,
    /// Detach HEAD in the new working tree
    pub detach: bool,
}

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

pub fn add_worktree(
    repo: &Repository,
    branch_name: &str,
    options: &AddWorktreeOptions,
) -> Result<WorktreeDescriptor> {
    // git worktree add <branch>
    debug!(
        "adding worktree for branch {:?} with options: {:?}",
        branch_name, options
    );

    let reference = if options.orphan {
        debug!("creating orphan branch {:?}", branch_name);
        // For orphan branches, create a reference that points to an empty tree
        // This mimics `git worktree add --orphan`
        None
    } else if options.detach {
        debug!("creating detached HEAD worktree at {:?}", branch_name);
        // For detached worktrees, we don't create or use a branch reference
        None
    } else {
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

        Some(branch.into_reference())
    };

    let root = workon_root(repo)?;

    // Git does not support worktree names with slashes in them,
    // so take the base of the branch name as the worktree name.
    let worktree_name = match Path::new(&branch_name).file_name() {
        Some(basename) => basename.to_str().unwrap(),
        None => branch_name,
    };

    let worktree_path = root.join(branch_name);

    // Create parent directories if the branch name contains slashes
    if let Some(parent) = worktree_path.parent() {
        create_dir_all(parent).into_diagnostic()?;
    }

    let mut opts = WorktreeAddOptions::new();
    if let Some(ref r) = reference {
        opts.reference(Some(r));
    }

    debug!(
        "adding worktree {} at {}",
        worktree_name,
        worktree_path.display()
    );

    repo
        .worktree(worktree_name, worktree_path.as_path(), Some(&opts))
        .map(WorktreeDescriptor::of)
        .into_diagnostic()
}
