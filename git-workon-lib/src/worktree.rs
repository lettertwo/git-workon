//! Worktree descriptor and metadata access.
//!
//! This module provides the core `WorktreeDescriptor` type that wraps git2's `Worktree`
//! and exposes rich metadata about worktree state.
//!
//! ## Completed Metadata Methods
//!
//! The following metadata is fully implemented and working:
//! - **Basic info**: `name()`, `path()`, `branch()`
//! - **State detection**: `is_detached()`, `is_dirty()`, `is_valid()`, `is_locked()`
//! - **Remote tracking**: `remote()`, `remote_branch()`, `remote_url()`, `remote_fetch_url()`, `remote_push_url()`
//! - **Commit info**: `head_commit()`
//! - **Status checks**: `has_unpushed_commits()`, `is_behind_upstream()`, `has_gone_upstream()`, `is_merged_into()`
//!
//! These methods enable status filtering (`--dirty`, `--ahead`, `--behind`, `--gone`) and
//! interactive display with status indicators.
//!
//! ## Branch Types
//!
//! Supports three branch types for worktree creation:
//! - **Normal**: Standard branch, tracks existing or creates from HEAD
//! - **Orphan**: Independent history with initial empty commit (for documentation, gh-pages, etc.)
//! - **Detached**: Detached HEAD state (for exploring specific commits)
//!
//! - **Activity tracking**: `last_activity()`, `is_stale()`
//!
//! ## Future Extensions
//!
//! Planned metadata methods for smart worktree management:
//!
//! TODO: Add worktree notes/descriptions support
//! - Store user-provided notes/context for worktrees
//! - Help remember why a worktree was created
//! - Storage strategy TBD (git notes, config, or metadata file)

use std::{
    fmt,
    fs::create_dir_all,
    path::{Path, PathBuf},
};

use git2::WorktreeAddOptions;
use git2::{Repository, Worktree};
use log::debug;

use crate::error::{Result, WorktreeError};
use crate::workon_root;
use crate::worktree_name::encode_worktree_name;

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

/// A handle to a git worktree with rich metadata access.
///
/// Wraps a [`git2::Worktree`] and exposes branch state, remote tracking info,
/// commit history, and status checks used by the CLI commands.
pub struct WorktreeDescriptor {
    worktree: Worktree,
    /// The worktree's private admin gitdir (`<commondir>/worktrees/<name>`), captured at
    /// construction. Unlike `<workdir>/.git`, this path survives deletion of the working
    /// directory, so [`branch`](Self::branch) can still resolve HEAD for a `prunable` worktree.
    git_dir: PathBuf,
}

impl WorktreeDescriptor {
    /// Open a worktree by name within the given repository.
    pub fn new(repo: &Repository, name: &str) -> Result<Self> {
        let worktree = repo.find_worktree(name)?;
        let git_dir = worktree_admin_git_dir(repo, &worktree);
        Ok(Self { worktree, git_dir })
    }

    /// Wrap an existing [`git2::Worktree`] using `repo` to resolve its admin gitdir.
    pub fn of(repo: &Repository, worktree: Worktree) -> Self {
        let git_dir = worktree_admin_git_dir(repo, &worktree);
        Self { worktree, git_dir }
    }

    /// Returns the name of the worktree, or `None` if the name is invalid UTF-8.
    pub fn name(&self) -> Option<&str> {
        self.worktree.name().ok().flatten()
    }

    /// Returns the filesystem path to the worktree's working directory.
    pub fn path(&self) -> &Path {
        self.worktree.path()
    }

    /// Returns the branch name if the worktree is on a branch, or None if detached.
    ///
    /// Reads HEAD from the worktree's admin gitdir (`<commondir>/worktrees/<name>`) rather than
    /// `<workdir>/.git`. The admin gitdir survives deletion of the working directory, so this
    /// still resolves for a `prunable` worktree — reading `<workdir>/.git/HEAD` directly would
    /// ENOENT and take down callers that walk every worktree (notably `prune`, whose whole job
    /// is to clean such dead worktrees up).
    pub fn branch(&self) -> Result<Option<String>> {
        // Read HEAD from the admin gitdir (captured at construction), never from <workdir>/.git —
        // the working directory may be gone (a `prunable` worktree). HEAD is always a loose file.
        let head_content = std::fs::read_to_string(self.git_dir.join("HEAD"))?;

        // HEAD contains either "ref: refs/heads/<branch>" (on a branch, born or unborn) or a
        // direct commit SHA (detached HEAD).
        if let Some(ref_line) = head_content.strip_prefix("ref: ") {
            Ok(ref_line
                .trim()
                .strip_prefix("refs/heads/")
                .map(str::to_string))
        } else {
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

    /// Returns true if the worktree has a lock file.
    ///
    /// Locked worktrees are protected from pruning unless `--include-locked`
    /// or `--force` is used.
    pub fn is_locked(&self) -> Result<bool> {
        Ok(!matches!(
            self.worktree.is_locked()?,
            git2::WorktreeLockStatus::Unlocked
        ))
    }

    /// Returns true if the worktree's path and git metadata are intact.
    ///
    /// A worktree is invalid if its directory is missing or its git
    /// metadata is broken.
    pub fn is_valid(&self) -> bool {
        self.worktree.validate().is_ok()
    }

    /// Returns true if the worktree has uncommitted changes to tracked files.
    ///
    /// Unlike `is_dirty()`, this excludes untracked files. Use this when
    /// untracked files should not block an operation (e.g. pruning a worktree
    /// whose remote branch is gone).
    pub fn has_tracked_changes(&self) -> Result<bool> {
        let repo = Repository::open(self.path())?;
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false);
        let statuses = repo.statuses(Some(&mut opts))?;
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

    /// Returns true if the worktree's branch is behind its upstream.
    ///
    /// Returns false if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The branch is up to date with upstream
    /// - The upstream is configured but the remote reference is gone
    ///
    /// Returns true if:
    /// - The branch has commits behind its upstream
    pub fn is_behind_upstream(&self) -> Result<bool> {
        // Get the branch name - return false if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(false), // Detached HEAD, no branch to check
        };

        // Open the repository
        let repo = Repository::open(self.path())?;

        // Find the local branch
        let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(false), // Branch doesn't exist
        };

        // Check if upstream is configured via git config
        let config = repo.config()?;
        let remote_key = format!("branch.{}.remote", branch_name);

        // If no upstream is configured, can't be behind
        let _remote = match config.get_string(&remote_key) {
            Ok(r) => r,
            Err(_) => return Ok(false), // No remote configured
        };

        // Get the upstream branch
        let upstream = match branch.upstream() {
            Ok(u) => u,
            Err(_) => {
                // Upstream is configured but ref is gone - can't be behind non-existent branch
                return Ok(false);
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

        // Check if local is behind upstream
        let (_ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;

        Ok(behind > 0)
    }

    /// Returns true if the worktree's upstream branch reference is gone (deleted on remote).
    ///
    /// Returns false if:
    /// - The worktree is detached (no branch)
    /// - The branch has no upstream configured
    /// - The upstream branch reference exists
    ///
    /// Returns true if:
    /// - Upstream is configured (branch.{name}.remote exists in config)
    /// - But the upstream branch reference cannot be found
    pub fn has_gone_upstream(&self) -> Result<bool> {
        // Get the branch name - return false if detached
        let branch_name = match self.branch()? {
            Some(name) => name,
            None => return Ok(false), // Detached HEAD, no branch to check
        };

        // Open the repository
        let repo = Repository::open(self.path())?;

        // Find the local branch
        let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok(false), // Branch doesn't exist
        };

        // Check if upstream is configured via git config
        let config = repo.config()?;
        let remote_key = format!("branch.{}.remote", branch_name);

        // If no upstream is configured, it's not "gone"
        match config.get_string(&remote_key) {
            Ok(_) => {
                // Upstream is configured - check if the reference exists
                match branch.upstream() {
                    Ok(_) => Ok(false), // Upstream exists
                    Err(_) => Ok(true), // Upstream configured but ref is gone
                }
            }
            Err(_) => Ok(false), // No upstream configured
        }
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

    /// Returns the timestamp of the HEAD commit as the last activity time.
    ///
    /// Returns None if:
    /// - HEAD cannot be resolved (empty/unborn repository)
    /// - HEAD cannot be peeled to a commit
    pub fn last_activity(&self) -> Result<Option<i64>> {
        let repo = Repository::open(self.path())?;
        let seconds = match repo.head() {
            Ok(head) => match head.peel_to_commit() {
                Ok(commit) => Some(commit.time().seconds()),
                Err(_) => None,
            },
            Err(_) => None,
        };
        Ok(seconds)
    }

    /// Returns true if the worktree's last activity is older than `days` days.
    ///
    /// Returns false if:
    /// - Last activity cannot be determined
    /// - The worktree has recent activity within the threshold
    pub fn is_stale(&self, days: u32) -> Result<bool> {
        let last = match self.last_activity()? {
            Some(ts) => ts,
            None => return Ok(false),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs() as i64;
        let threshold = i64::from(days) * 86400;
        Ok((now - last) > threshold)
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
            Ok(remote) => remote.url().ok().map(|s| s.to_string()),
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
            Ok(remote) => remote.url().ok().map(|s| s.to_string()),
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
                .ok()
                .flatten()
                .or_else(|| remote.url().ok())
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

/// The private admin gitdir for a linked worktree: `<commondir>/worktrees/<name>`.
///
/// This is where git keeps the worktree's own `HEAD`, `index`, and refs — it survives deletion
/// of the working directory, unlike the `<workdir>/.git` gitfile. Falls back to the commondir
/// itself if the worktree name is unreadable (an invalid worktree we can't resolve anyway).
fn worktree_admin_git_dir(repo: &Repository, worktree: &Worktree) -> PathBuf {
    match worktree.name() {
        Ok(Some(name)) => repo.commondir().join("worktrees").join(name),
        _ => repo.commondir().to_path_buf(),
    }
}

/// Return all worktrees registered with the repository.
pub fn get_worktrees(repo: &Repository) -> Result<Vec<WorktreeDescriptor>> {
    repo.worktrees()?
        .into_iter()
        .map(|name| {
            let name = name?.ok_or(WorktreeError::InvalidName)?;
            WorktreeDescriptor::new(repo, name)
        })
        .collect()
}

/// Return the worktree that contains the current working directory.
///
/// Returns [`WorktreeError::NotInWorktree`] if the current directory is not
/// inside any registered worktree.
pub fn current_worktree(repo: &Repository) -> Result<WorktreeDescriptor> {
    let current_dir = std::env::current_dir().map_err(std::io::Error::other)?;

    let worktrees = get_worktrees(repo)?;
    worktrees
        .into_iter()
        .find(|wt| current_dir.starts_with(wt.path()))
        .ok_or_else(|| WorktreeError::NotInWorktree.into())
}

/// Find a worktree by its admin name, its branch name, or its root-relative path.
///
/// The admin name is a creation-time artifact (see [`encode_worktree_name`]) and can go
/// stale if a worktree is moved outside `move_worktree` (e.g. a raw `git worktree move`);
/// the root-relative path never does, since git rewrites `gitdir` on every move. Nothing
/// here decodes an admin name back into a path — a stale name is a label, not a key.
///
/// Returns [`WorktreeError::NotFound`] if no matching worktree exists.
pub fn find_worktree(repo: &Repository, name: &str) -> Result<WorktreeDescriptor> {
    let root = workon_root(repo).ok();
    let worktrees = get_worktrees(repo)?;
    worktrees
        .into_iter()
        .find(|wt| {
            wt.name() == Some(name)
                || wt.branch().ok().flatten().as_deref() == Some(name)
                || root
                    .and_then(|root| wt.path().strip_prefix(root).ok())
                    .and_then(|relative| relative.to_str())
                    == Some(name)
        })
        .ok_or_else(|| WorktreeError::NotFound(name.to_string()).into())
}

/// Find the worktree whose checked-out branch is `branch`.
///
/// Unlike [`find_worktree`] this never matches by worktree name: after an
/// in-place checkout a stack-home worktree's name routinely diverges from its
/// branch, and a name match would treat "a worktree once created for T" as
/// "T is checked out" — wrong for resolution decisions.
///
/// Returns [`WorktreeError::NotFound`] if no worktree has `branch` checked out.
pub fn find_worktree_by_branch(repo: &Repository, branch: &str) -> Result<WorktreeDescriptor> {
    let worktrees = get_worktrees(repo)?;
    worktrees
        .into_iter()
        .find(|wt| wt.branch().ok().flatten().as_deref() == Some(branch))
        .ok_or_else(|| WorktreeError::NotFound(branch.to_string()).into())
}

/// Result of looking up which remote carries a branch.
pub enum RemoteResolution {
    /// Exactly one remote (or a clear winner by priority) found.
    Single { remote: String, oid: git2::Oid },
    /// Two or more equally-preferred remotes carry the branch — user must choose.
    Ambiguous(Vec<String>),
    /// No remote has the branch.
    None,
}

/// Find which remote(s) carry a branch, ranked by the shared
/// [`remote_priority`](crate::pr::remote_priority) precedence
/// (`upstream → origin → others`).
///
/// Returns `Ambiguous` when two or more equally-preferred remotes both carry
/// the branch.
pub fn resolve_remote_tracking(repo: &Repository, branch_name: &str) -> RemoteResolution {
    let branches = match repo.branches(Some(git2::BranchType::Remote)) {
        Ok(b) => b,
        Err(_) => return RemoteResolution::None,
    };

    let mut candidates: Vec<(String, git2::Oid)> = branches
        .flatten()
        .filter_map(|(branch, _)| {
            let name = branch.name().ok()??;
            let (remote, br) = name.split_once('/')?;
            if br != branch_name {
                return None;
            }
            Some((remote.to_string(), branch.get().target()?))
        })
        .collect();

    if candidates.is_empty() {
        return RemoteResolution::None;
    }

    candidates.sort_by_key(|(r, _)| crate::pr::remote_priority(r));

    if candidates.len() >= 2
        && crate::pr::remote_priority(&candidates[0].0)
            == crate::pr::remote_priority(&candidates[1].0)
    {
        return RemoteResolution::Ambiguous(candidates.into_iter().map(|(r, _)| r).collect());
    }

    let (remote, oid) = candidates.remove(0);
    RemoteResolution::Single { remote, oid }
}

/// Create local branch `branch` from `remote`'s tracking ref and set its upstream.
///
/// The single place where a local branch is materialized from a remote tracking
/// branch — keeping creation and upstream wiring together so no caller can get
/// one without the other. Used by [`add_worktree`] when it resolves the remote
/// itself, and by callers that resolved an ambiguous remote (e.g. by prompting).
pub fn create_branch_from_remote(repo: &Repository, branch: &str, remote: &str) -> Result<()> {
    let remote_ref = repo.find_reference(&format!("refs/remotes/{}/{}", remote, branch))?;
    let commit = remote_ref.peel_to_commit()?;
    let mut local_branch = repo.branch(branch, &commit, false)?;
    local_branch.set_upstream(Some(&format!("{}/{}", remote, branch)))?;
    Ok(())
}

/// Create a new worktree for the given branch.
///
/// The worktree directory is placed under the workon root (see [`workon_root`]).
/// Branch names containing `/` are supported; parent directories are created
/// automatically and the worktree's admin (metadata) directory is named by encoding
/// the root-relative path (see [`encode_worktree_name`]).
///
/// When `explicit_worktree_name` is `Some`, that value is used as the worktree
/// directory name and filesystem path instead of deriving it from `branch_name`.
/// This allows the worktree directory and the branch to have different names.
///
/// # Branch types
///
/// - [`BranchType::Normal`] — uses an existing local branch, creates a local branch
///   from a matching remote tracking branch (setting upstream automatically), or
///   creates a new branch from `base_branch` (or HEAD if `base_branch` is `None`).
/// - [`BranchType::Orphan`] — creates an independent branch with no shared history,
///   seeded with an empty initial commit.
/// - [`BranchType::Detached`] — creates a worktree with a detached HEAD pointing to
///   the current HEAD commit.
pub fn add_worktree(
    repo: &Repository,
    branch_name: &str,
    explicit_worktree_name: Option<&str>,
    branch_type: BranchType,
    base_branch: Option<&str>,
    lock: bool,
) -> Result<WorktreeDescriptor> {
    // git worktree add <branch>
    debug!(
        "adding worktree for branch {:?} with type: {:?}",
        branch_name, branch_type
    );

    let reference = match branch_type {
        BranchType::Orphan => {
            debug!("creating orphan branch {:?}", branch_name);
            // When `reference` is `None`, libgit2 creates a branch named after the
            // *worktree name* to check out — which may be `~`-encoded and which
            // `git_reference_create` rejects. Create the branch ourselves under
            // `branch_name` instead; the orphan post-processing below rewrites HEAD
            // and history onto it.
            let head_commit = repo.head()?.peel_to_commit()?;
            let branch = repo.branch(branch_name, &head_commit, false)?;
            Some(branch.into_reference())
        }
        BranchType::Detached => {
            debug!("creating detached HEAD worktree at {:?}", branch_name);
            // Same reasoning as the orphan arm: an explicit reference is required to
            // avoid libgit2 deriving a branch name from the (possibly encoded)
            // worktree name. This branch is temporary — detached worktrees carry no
            // branch, so it is deleted once the commit SHA is written to HEAD below.
            let head_commit = repo.head()?.peel_to_commit()?;
            let branch = repo.branch(branch_name, &head_commit, false)?;
            Some(branch.into_reference())
        }
        BranchType::Normal => {
            let branch = match repo.find_branch(branch_name, git2::BranchType::Local) {
                Ok(b) => b,
                Err(e) => {
                    debug!("local branch not found: {:?}", e);
                    debug!("looking for remote tracking branch for {:?}", branch_name);
                    match resolve_remote_tracking(repo, branch_name) {
                        RemoteResolution::Single {
                            remote: remote_name,
                            ..
                        } => {
                            debug!(
                                "found remote tracking branch {}/{}, creating local branch",
                                remote_name, branch_name
                            );
                            create_branch_from_remote(repo, branch_name, &remote_name)?;
                            repo.find_branch(branch_name, git2::BranchType::Local)?
                        }
                        RemoteResolution::Ambiguous(_) | RemoteResolution::None => {
                            debug!(
                                "no remote tracking branch found, creating new local branch {:?}",
                                branch_name
                            );

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

    // Determine worktree name and path.
    // When an explicit name is provided, use it directly.
    // Otherwise, derive from branch_name. Git does not support worktree admin names with
    // slashes, so the root-relative path is encoded into an admin name (see ADR-027).
    let (worktree_name, worktree_path) = if let Some(alias) = explicit_worktree_name {
        (encode_worktree_name(alias), root.join(alias))
    } else {
        (encode_worktree_name(branch_name), root.join(branch_name))
    };

    // Create parent directories if the branch name contains slashes
    if let Some(parent) = worktree_path.parent() {
        create_dir_all(parent)?;
    }

    let mut opts = WorktreeAddOptions::new();
    if let Some(ref r) = reference {
        opts.reference(Some(r));
    }
    if lock {
        opts.lock(true);
    }

    // Backstop: encoding makes an admin-name collision close to unreachable, but a
    // legacy basename-named worktree can still occupy a top-level slot. Name the
    // conflict instead of letting libgit2 surface a bare `mkdir` failure.
    let admin_dir = repo.path().join("worktrees").join(&worktree_name);
    if admin_dir.exists() {
        return Err(WorktreeError::WorktreeNameConflict {
            name: worktree_name.clone(),
            path: admin_dir.display().to_string(),
        }
        .into());
    }

    debug!(
        "adding worktree {} at {}",
        worktree_name,
        worktree_path.display()
    );

    let worktree = repo.worktree(&worktree_name, worktree_path.as_path(), Some(&opts))?;

    // For detached worktrees, set HEAD to point directly to a commit SHA
    if branch_type == BranchType::Detached {
        debug!("setting up detached HEAD for worktree {:?}", branch_name);

        use std::fs;

        // Get the current HEAD commit SHA
        let head_commit = repo.head()?.peel_to_commit()?;
        let commit_sha = head_commit.id().to_string();

        // Write the commit SHA directly to the worktree's HEAD file
        let git_dir = repo.path().join("worktrees").join(&worktree_name);
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, format!("{}\n", commit_sha).as_bytes())?;

        // Remove the temporary branch created only to satisfy `git_worktree_add`'s
        // reference requirement; a detached worktree carries no branch.
        let mut temp_branch = repo.find_branch(branch_name, git2::BranchType::Local)?;
        temp_branch.delete()?;

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

        // Get the common directory (bare repo path) - important when running from a worktree
        let common_dir = repo.commondir();

        // First, manually set HEAD to point to the new branch as a symbolic reference
        // This ensures we're not trying to update an existing branch
        let git_dir = common_dir.join("worktrees").join(&worktree_name);
        let head_path = git_dir.join("HEAD");
        let branch_ref = format!("ref: refs/heads/{}\n", branch_name);
        fs::write(&head_path, branch_ref.as_bytes())?;

        // The branch at refs/heads/<branch_name> is the one we created explicitly above
        // (see the `reference` match), so there is no stray libgit2-created branch to
        // clean up here.

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

    Ok(WorktreeDescriptor::of(repo, worktree))
}

/// Set upstream tracking for a worktree branch
///
/// Configures the branch in the worktree to track a remote branch by setting
/// `branch.*.remote` and `branch.*.merge` configuration entries.
///
/// This is particularly important for PR worktrees to ensure they properly track
/// the PR's remote branch.
pub fn set_upstream_tracking(
    worktree: &WorktreeDescriptor,
    remote: &str,
    remote_ref: &str,
) -> Result<()> {
    let repo = Repository::open(worktree.path())?;
    let mut config = repo.config()?;

    let head = repo.head()?;
    let branch_name = head
        .shorthand()
        .ok()
        .ok_or(WorktreeError::NoCurrentBranchTarget)?;

    // Set branch.*.remote
    let remote_key = format!("branch.{}.remote", branch_name);
    config.set_str(&remote_key, remote)?;

    // Set branch.*.merge
    let merge_key = format!("branch.{}.merge", branch_name);
    config.set_str(&merge_key, remote_ref)?;

    debug!(
        "Set upstream tracking: {} -> {}/{}",
        branch_name, remote, remote_ref
    );
    Ok(())
}
