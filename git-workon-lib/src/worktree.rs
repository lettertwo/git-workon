use std::{fmt, fs::create_dir_all, path::Path};

use git2::WorktreeAddOptions;
use git2::{Repository, Worktree};
use log::debug;

use crate::error::{Result, WorktreeError};
use crate::workon_root;

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
            worktree: repo.find_worktree(name)?,
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
            let git_file_content = fs::read_to_string(&git_dir)?;
            let git_dir_path = git_file_content
                .strip_prefix("gitdir: ")
                .and_then(|s| s.trim().strip_suffix('\n').or(Some(s.trim())))
                .ok_or(WorktreeError::InvalidGitFile)?;
            Path::new(git_dir_path).join("HEAD")
        } else {
            // Main worktree
            git_dir.join("HEAD")
        };

        let head_content = fs::read_to_string(&head_path)?;

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

    /// Returns true if the worktree has uncommitted changes (dirty working tree).
    ///
    /// This includes:
    /// - Modified files (staged or unstaged)
    /// - New untracked files
    /// - Deleted files
    pub fn is_dirty(&self) -> Result<bool> {
        let repo = Repository::open(self.path())?;
        let statuses = repo.statuses(None)?;
        Ok(!statuses.is_empty())
    }

    /// Returns true if the worktree's branch has unpushed commits (ahead of upstream).
    ///
    /// Returns false if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The branch is up to date with upstream
    ///
    /// Returns true if:
    /// - The branch has commits ahead of its upstream
    /// - The upstream is configured but the remote reference is gone (conservative)
    pub fn has_unpushed_commits(&self) -> Result<bool> {
        // Get the branch name - return false if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(false), // Detached HEAD, no branch to check
        };

        // Open the repository (use the bare repo, not the worktree)
        let repo = Repository::open(self.path())?;

        // Find the local branch
        let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(false), // Branch doesn't exist
        };

        // Check if upstream is configured via git config
        let config = repo.config()?;
        let remote_key = format!("branch.{}.remote", branch_name);

        // If no upstream is configured, there can't be unpushed commits
        let _remote = match config.get_string(&remote_key) {
            Ok(r) => r,
            Err(_) => return Ok(false), // No remote configured
        };

        // Get the upstream branch
        let upstream = match branch.upstream() {
            Ok(u) => u,
            Err(_) => {
                // Upstream is configured but ref is gone - conservatively assume unpushed
                return Ok(true);
            }
        };

        // Get the local and upstream commit OIDs
        let local_oid = branch
            .get()
            .target()
            .ok_or(WorktreeError::NoLocalBranchTarget)?;
        let upstream_oid = upstream
            .get()
            .target()
            .ok_or(WorktreeError::NoBranchTarget)?;

        // Check if local is ahead of upstream
        let (ahead, _behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;

        Ok(ahead > 0)
    }

    /// Returns true if the worktree's branch has been merged into the target branch.
    ///
    /// A branch is considered merged if its HEAD commit is reachable from the target branch,
    /// meaning all commits in this branch exist in the target branch's history.
    ///
    /// Returns false if:
    /// - The worktree is detached (no branch)
    /// - The target branch doesn't exist
    /// - The branch has commits not in the target branch
    ///
    /// Returns true if:
    /// - All commits in this branch are reachable from the target branch
    pub fn is_merged_into(&self, target_branch: &str) -> Result<bool> {
        // Get the branch name - return false if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(false), // Detached HEAD, no branch to check
        };

        // Don't consider the target branch as merged into itself
        if branch_name == target_branch {
            return Ok(false);
        }

        // Open the bare repository (not the worktree) to check actual branch states
        // The worktree's .git points to the commondir (bare repo)
        let worktree_repo = Repository::open(self.path())?;
        let commondir = worktree_repo.commondir();
        let repo = Repository::open(commondir)?;

        // Find the current branch
        let current_branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(false), // Branch doesn't exist
        };

        // Find the target branch
        let target = match repo.find_branch(target_branch, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(false), // Target branch doesn't exist
        };

        // Get commit OIDs
        let current_oid = current_branch
            .get()
            .target()
            .ok_or(WorktreeError::NoCurrentBranchTarget)?;
        let target_oid = target.get().target().ok_or(WorktreeError::NoBranchTarget)?;

        // If they point to the same commit, the branch is merged
        if current_oid == target_oid {
            return Ok(true);
        }

        // Check if current branch's commit is reachable from target
        // This means target is a descendant of (or equal to) current
        Ok(repo.graph_descendant_of(target_oid, current_oid)?)
    }

    /// Returns the commit hash (SHA) of the worktree's current HEAD.
    ///
    /// Returns None if HEAD cannot be resolved (e.g., empty repository).
    pub fn head_commit(&self) -> Result<Option<String>> {
        let repo = Repository::open(self.path())?;

        // Try to resolve HEAD to a commit and extract the OID immediately
        let commit_oid = match repo.head() {
            Ok(head) => match head.peel_to_commit() {
                Ok(commit) => Some(commit.id()),
                Err(_) => return Ok(None), // HEAD exists but can't resolve to commit
            },
            Err(_) => return Ok(None), // No HEAD (unborn branch)
        };

        Ok(commit_oid.map(|oid| oid.to_string()))
    }

    /// Returns the name of the remote that the worktree's branch tracks (e.g., "origin").
    ///
    /// Returns None if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    pub fn remote(&self) -> Result<Option<String>> {
        // Get the branch name - return None if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(None), // Detached HEAD, no branch to check
        };

        let repo = Repository::open(self.path())?;
        let config = repo.config()?;

        // Check for branch.<name>.remote in git config
        let remote_key = format!("branch.{}.remote", branch_name);
        match config.get_string(&remote_key) {
            Ok(remote) => Ok(Some(remote)),
            Err(_) => Ok(None), // No remote configured
        }
    }

    /// Returns the full name of the upstream remote branch (e.g., "refs/remotes/origin/main").
    ///
    /// Returns None if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    pub fn remote_branch(&self) -> Result<Option<String>> {
        // Get the branch name - return None if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(None), // Detached HEAD, no branch to check
        };

        let repo = Repository::open(self.path())?;

        // Find the local branch and get its upstream, extracting the name immediately
        let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(None), // Branch doesn't exist
        };

        let upstream_name = match branch.upstream() {
            Ok(upstream) => match upstream.name() {
                Ok(Some(name)) => Some(name.to_string()),
                _ => None,
            },
            Err(_) => return Ok(None), // No upstream configured
        };

        Ok(upstream_name)
    }

    /// Returns the default URL for the remote (usually the fetch URL).
    ///
    /// Returns None if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The remote has no URL configured
    pub fn remote_url(&self) -> Result<Option<String>> {
        // Get the remote name
        let remote_name = match self.remote()? {
            Some(name) => name,
            None => return Ok(None),
        };

        let repo = Repository::open(self.path())?;

        // Find the remote and extract the URL immediately
        let url = match repo.find_remote(&remote_name) {
            Ok(remote) => remote.url().map(|s| s.to_string()),
            Err(_) => return Ok(None), // Remote doesn't exist
        };

        Ok(url)
    }

    /// Returns the fetch URL for the remote.
    ///
    /// Returns None if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The remote has no fetch URL configured
    pub fn remote_fetch_url(&self) -> Result<Option<String>> {
        // Get the remote name
        let remote_name = match self.remote()? {
            Some(name) => name,
            None => return Ok(None),
        };

        let repo = Repository::open(self.path())?;

        // Find the remote and extract the fetch URL immediately
        let url = match repo.find_remote(&remote_name) {
            Ok(remote) => remote.url().map(|s| s.to_string()),
            Err(_) => return Ok(None), // Remote doesn't exist
        };

        Ok(url)
    }

    /// Returns the push URL for the remote.
    ///
    /// Returns None if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The remote has no push URL configured (falls back to fetch URL)
    pub fn remote_push_url(&self) -> Result<Option<String>> {
        // Get the remote name
        let remote_name = match self.remote()? {
            Some(name) => name,
            None => return Ok(None),
        };

        let repo = Repository::open(self.path())?;

        // Find the remote and extract the push URL (or fallback to fetch URL) immediately
        let url = match repo.find_remote(&remote_name) {
            Ok(remote) => remote
                .pushurl()
                .or_else(|| remote.url())
                .map(|s| s.to_string()),
            Err(_) => return Ok(None), // Remote doesn't exist
        };

        Ok(url)
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
    repo.worktrees()?
        .into_iter()
        .map(|name| {
            let name = name.ok_or(WorktreeError::InvalidName)?;
            WorktreeDescriptor::new(repo, name)
        })
        .collect()
}

pub fn add_worktree(
    repo: &Repository,
    branch_name: &str,
    branch_type: BranchType,
    base_branch: Option<&str>,
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
            let branch = match repo.find_branch(branch_name, git2::BranchType::Local) {
                Ok(b) => b,
                Err(e) => {
                    debug!("local branch not found: {:?}", e);
                    debug!("looking for remote branch {:?}", branch_name);
                    match repo.find_branch(branch_name, git2::BranchType::Remote) {
                        Ok(b) => b,
                        Err(e) => {
                            debug!("remote branch not found: {:?}", e);
                            debug!("creating new local branch {:?}", branch_name);

                            // Determine which commit to branch from
                            let base_commit = if let Some(base) = base_branch {
                                // Branch from specified base branch
                                debug!("branching from base branch {:?}", base);
                                // Try local branch first, then remote branch
                                let base_branch =
                                    match repo.find_branch(base, git2::BranchType::Local) {
                                        Ok(b) => b,
                                        Err(_) => {
                                            debug!("base branch not found as local, trying remote");
                                            repo.find_branch(base, git2::BranchType::Remote)?
                                        }
                                    };
                                base_branch.into_reference().peel_to_commit()?
                            } else {
                                // Default: branch from HEAD
                                repo.head()?.peel_to_commit()?
                            };

                            repo.branch(branch_name, &base_commit, false)?
                        }
                    }
                }
            };

            Some(branch.into_reference())
        }
    };

    let root = workon_root(repo)?;

    // Git does not support worktree names with slashes in them,
    // so take the base of the branch name as the worktree name.
    let worktree_name = match Path::new(&branch_name).file_name() {
        Some(basename) => basename.to_str().ok_or(WorktreeError::InvalidName)?,
        None => branch_name,
    };

    let worktree_path = root.join(branch_name);

    // Create parent directories if the branch name contains slashes
    if let Some(parent) = worktree_path.parent() {
        create_dir_all(parent)?;
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

    let worktree = repo.worktree(worktree_name, worktree_path.as_path(), Some(&opts))?;

    // For detached worktrees, set HEAD to point directly to a commit SHA
    if branch_type == BranchType::Detached {
        debug!("setting up detached HEAD for worktree {:?}", branch_name);

        use std::fs;

        // Get the current HEAD commit SHA
        let head_commit = repo.head()?.peel_to_commit()?;
        let commit_sha = head_commit.id().to_string();

        // Write the commit SHA directly to the worktree's HEAD file
        let git_dir = repo.path().join("worktrees").join(worktree_name);
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, format!("{}\n", commit_sha).as_bytes())?;

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
        fs::write(&head_path, branch_ref.as_bytes())?;

        // Remove any existing branch ref that libgit2 may have created
        let branch_ref_path = repo.path().join("refs/heads").join(branch_name);
        let _ = fs::remove_file(&branch_ref_path);

        // Open the worktree repository
        let worktree_repo = Repository::open(&worktree_path)?;

        // Remove all files from the working directory (but keep .git)
        for entry in fs::read_dir(&worktree_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.file_name() != Some(std::ffi::OsStr::new(".git")) {
                if path.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
            }
        }

        // Clear the index to start fresh
        let mut index = worktree_repo.index()?;
        index.clear()?;
        index.write()?;

        // Create an empty tree for the initial commit
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;

        // Create signature for the commit
        let config = worktree_repo.config()?;
        let sig = worktree_repo.signature().or_else(|_| {
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
        })?;

        // Create initial commit with no parents (orphan)
        worktree_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Initial commit",
            &tree,
            &[], // No parents - this makes it an orphan
        )?;

        debug!("orphan branch setup complete for {:?}", branch_name);
    }

    Ok(WorktreeDescriptor::of(worktree))
}
