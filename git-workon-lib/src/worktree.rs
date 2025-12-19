use std::{fmt, fs::create_dir_all, path::Path};

use git2::{Repository, Worktree};
use miette::{IntoDiagnostic, Result};

use git2::WorktreeAddOptions;
use log::debug;

use super::workon_root;

/// Type of branch to create for a new worktree
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchType {
    /// Normal branch - track existing or create from HEAD
    #[default]
    Normal,
    /// Orphan branch - independent history with initial empty commit
    Orphan,
    /// Detached HEAD
    Detached,
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

    /// Returns the branch name if the worktree is on a branch, or None if detached.
    ///
    /// This reads the HEAD file from the worktree's git directory to determine
    /// if HEAD points to a branch reference or directly to a commit SHA.
    pub fn branch(&self) -> Result<Option<String>> {
        use std::fs;

        // Get the path to the worktree's HEAD file
        let git_dir = self.worktree.path().join(".git");
        let head_path = if git_dir.is_file() {
            // Linked worktree - read .git file to find actual git directory
            let git_file_content = fs::read_to_string(&git_dir).into_diagnostic()?;
            let git_dir_path = git_file_content
                .strip_prefix("gitdir: ")
                .and_then(|s| s.trim().strip_suffix('\n').or(Some(s.trim())))
                .ok_or_else(|| miette::miette!("Invalid .git file format"))?;
            Path::new(git_dir_path).join("HEAD")
        } else {
            // Main worktree
            git_dir.join("HEAD")
        };

        let head_content = fs::read_to_string(&head_path).into_diagnostic()?;

        // HEAD file contains either:
        // - "ref: refs/heads/branch-name" for a branch
        // - A direct SHA for detached HEAD
        if let Some(ref_line) = head_content.strip_prefix("ref: ") {
            let ref_name = ref_line.trim();
            Ok(ref_name.strip_prefix("refs/heads/").map(|s| s.to_string()))
        } else {
            // Direct SHA - detached HEAD
            Ok(None)
        }
    }

    /// Returns true if the worktree has a detached HEAD (not on a branch).
    pub fn is_detached(&self) -> Result<bool> {
        Ok(self.branch()?.is_none())
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
    branch_type: BranchType,
) -> Result<WorktreeDescriptor> {
    // git worktree add <branch>
    debug!(
        "adding worktree for branch {:?} with type: {:?}",
        branch_name, branch_type
    );

    let reference = match branch_type {
        BranchType::Orphan => {
            debug!("creating orphan branch {:?}", branch_name);
            // For orphan branches, we'll create the branch after the worktree
            None
        }
        BranchType::Detached => {
            debug!("creating detached HEAD worktree at {:?}", branch_name);
            // For detached worktrees, we don't create or use a branch reference
            None
        }
        BranchType::Normal => {
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
        }
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

    let worktree = repo
        .worktree(worktree_name, worktree_path.as_path(), Some(&opts))
        .into_diagnostic()?;

    // For detached worktrees, set HEAD to point directly to a commit SHA
    if branch_type == BranchType::Detached {
        debug!("setting up detached HEAD for worktree {:?}", branch_name);

        use std::fs;

        // Get the current HEAD commit SHA
        let head_commit = repo
            .head()
            .into_diagnostic()?
            .peel_to_commit()
            .into_diagnostic()?;
        let commit_sha = head_commit.id().to_string();

        // Write the commit SHA directly to the worktree's HEAD file
        let git_dir = repo.path().join("worktrees").join(worktree_name);
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, format!("{}\n", commit_sha).as_bytes()).into_diagnostic()?;

        debug!(
            "detached HEAD setup complete for worktree {:?} at {}",
            branch_name, commit_sha
        );
    }

    // For orphan branches, create an initial empty commit with no parent
    if branch_type == BranchType::Orphan {
        debug!(
            "setting up orphan branch {:?} with initial empty commit",
            branch_name
        );

        use std::fs;

        // First, manually set HEAD to point to the new branch as a symbolic reference
        // This ensures we're not trying to update an existing branch
        let git_dir = repo.path().join("worktrees").join(worktree_name);
        let head_path = git_dir.join("HEAD");
        let branch_ref = format!("ref: refs/heads/{}\n", branch_name);
        fs::write(&head_path, branch_ref.as_bytes()).into_diagnostic()?;

        // Remove any existing branch ref that libgit2 may have created
        let branch_ref_path = repo.path().join("refs/heads").join(branch_name);
        let _ = fs::remove_file(&branch_ref_path);

        // Open the worktree repository
        let worktree_repo = Repository::open(&worktree_path).into_diagnostic()?;

        // Remove all files from the working directory (but keep .git)
        for entry in fs::read_dir(&worktree_path).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let path = entry.path();
            if path.file_name() != Some(std::ffi::OsStr::new(".git")) {
                if path.is_dir() {
                    fs::remove_dir_all(&path).into_diagnostic()?;
                } else {
                    fs::remove_file(&path).into_diagnostic()?;
                }
            }
        }

        // Clear the index to start fresh
        let mut index = worktree_repo.index().into_diagnostic()?;
        index.clear().into_diagnostic()?;
        index.write().into_diagnostic()?;

        // Create an empty tree for the initial commit
        let tree_id = index.write_tree().into_diagnostic()?;
        let tree = worktree_repo.find_tree(tree_id).into_diagnostic()?;

        // Create signature for the commit
        let config = worktree_repo.config().into_diagnostic()?;
        let sig = worktree_repo
            .signature()
            .or_else(|_| {
                // Fallback if no git config is set
                git2::Signature::now(
                    config
                        .get_string("user.name")
                        .unwrap_or_else(|_| "git-workon".to_string())
                        .as_str(),
                    config
                        .get_string("user.email")
                        .unwrap_or_else(|_| "git-workon@localhost".to_string())
                        .as_str(),
                )
            })
            .into_diagnostic()?;

        // Create initial commit with no parents (orphan)
        worktree_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Initial commit",
                &tree,
                &[], // No parents - this makes it an orphan
            )
            .into_diagnostic()?;

        debug!("orphan branch setup complete for {:?}", branch_name);
    }

    Ok(WorktreeDescriptor::of(worktree))
}
