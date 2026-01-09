//! Atomic worktree and branch renaming.
//!
//! This module provides atomic renaming of worktrees and their associated branches,
//! keeping the branch name and directory structure synchronized.
//!
//! ## Atomic Operation Strategy
//!
//! The move operation consists of three steps:
//! 1. Rename the branch using `git branch -m`
//! 2. Move the worktree directory to match the new branch name
//! 3. Update git worktree metadata bidirectionally:
//!    - Update `.git/worktrees/<name>/gitdir` to point to new location
//!    - Update worktree's `.git` file to point to correct admin directory
//!
//! If the directory move fails after branch rename, the operation rolls back the branch
//! rename to maintain consistency.
//!
//! ## Safety Checks
//!
//! By default, the operation performs several safety checks:
//! - Source worktree exists
//! - Target doesn't exist (no conflicts with existing worktrees or branches)
//! - Source is not detached HEAD (can't rename detached HEAD)
//! - Source is not protected (matches `workon.pruneProtectedBranches`)
//! - Source is not dirty (no uncommitted changes)
//! - Source has no unpushed commits (all commits are pushed to remote)
//!
//! The `--force` flag overrides all safety checks (single flag for simplicity).
//!
//! ## Namespace Support
//!
//! Supports moving worktrees between namespaces:
//! ```bash
//! git workon move feature user/feature        # Move into namespace
//! git workon move user/feature feature        # Move out of namespace
//! git workon move old/path new/deeper/path    # Reorganize
//! ```
//!
//! Parent directories are created automatically as needed.
//!
//! ## CLI Modes
//!
//! Two invocation modes:
//! 1. **Single-arg mode**: `git workon move <new-name>` - Renames current worktree (when run from within a worktree)
//! 2. **Two-arg mode**: `git workon move <from> <to>` - Explicit source and target
//!
//! ## Example Usage
//!
//! ```bash
//! # Rename current worktree
//! cd ~/repos/project/feature
//! git workon move new-feature-name
//!
//! # Rename specific worktree
//! git workon move old-name new-name
//!
//! # Move into namespace
//! git workon move feature user/feature
//!
//! # Preview changes
//! git workon move --dry-run old new
//!
//! # Override safety checks
//! git workon move --force dirty-branch new-name
//! ```
//!
//! TODO: Update shell integration cache when move command is used

use git2::BranchType;
use std::{fs, path::Path};

use crate::{
    error::Result, find_worktree, get_worktrees, WorkonConfig, WorkonError, WorktreeDescriptor,
    WorktreeError,
};

/// Options for moving a worktree
#[derive(Default)]
pub struct MoveOptions {
    /// Override safety checks (dirty, unpushed, protected)
    pub force: bool,
}

/// Move (rename) a worktree and its branch atomically.
///
/// This performs the following operations:
/// 1. Renames the branch
/// 2. Moves the worktree directory
/// 3. Updates worktree metadata
///
/// The operation includes rollback if the directory move fails after branch rename.
///
/// # Arguments
///
/// * `repo` - The repository containing the worktree
/// * `from` - Current worktree/branch name
/// * `to` - New worktree/branch name
/// * `options` - Move options (force flag, etc.)
///
/// # Errors
///
/// Returns an error if:
/// - Source worktree doesn't exist
/// - Target already exists (worktree or branch)
/// - Source is detached HEAD
/// - Source is protected (unless force)
/// - Source is dirty (unless force)
/// - Source has unpushed commits (unless force)
/// - Directory move fails
pub fn move_worktree(
    repo: &git2::Repository,
    from: &str,
    to: &str,
    options: &MoveOptions,
) -> Result<WorktreeDescriptor> {
    // Find source worktree
    let source = find_worktree(repo, from)?;

    // Validate the move
    validate_move(repo, &source, to, options)?;

    // Execute the move
    let root = crate::workon_root(repo)?;
    let branch_name = source.branch()?.unwrap();
    let old_path = source.path().to_path_buf();
    let new_path = root.join(to);

    // Calculate worktree names (basename of branch names)
    let old_name = source.name().unwrap().to_string();
    let new_name = Path::new(to)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(WorktreeError::InvalidName)?
        .to_string();

    // Create parent directories for namespace changes
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Step 1: Rename the branch
    let mut branch = repo.find_branch(&branch_name, BranchType::Local)?;
    branch.rename(to, false)?;

    // Step 2: Move the directory (with rollback on failure)
    if let Err(e) = fs::rename(&old_path, &new_path) {
        // Attempt to rollback branch rename
        let _ = branch.rename(&branch_name, false);
        return Err(WorkonError::Io(e));
    }

    // Step 3: Rename worktree metadata directory if name changed
    let old_meta_dir = repo.path().join("worktrees").join(&old_name);
    let new_meta_dir = repo.path().join("worktrees").join(&new_name);
    if old_meta_dir != new_meta_dir && old_meta_dir.exists() {
        fs::rename(&old_meta_dir, &new_meta_dir)?;
    }
    if new_meta_dir.exists() {
        let new_gitdir = new_meta_dir.join("gitdir");
        let new_git = new_path.join(".git");

        fs::write(&new_gitdir, format!("{}\n", new_git.display()))?;
        fs::write(&new_git, format!("gitdir: {}\n", new_meta_dir.display()))?;
    }

    WorktreeDescriptor::new(repo, &new_name)
}

pub fn validate_move(
    repo: &git2::Repository,
    source: &WorktreeDescriptor,
    target_name: &str,
    options: &MoveOptions,
) -> Result<()> {
    // 1. Check if source is detached
    if source.is_detached()? {
        return Err(WorktreeError::CannotMoveDetached.into());
    }

    // 2. Check if target already exists (worktree name or branch name)
    for wt in get_worktrees(repo)? {
        if wt.name() == Some(target_name)
            || wt.branch().ok().flatten().as_deref() == Some(target_name)
        {
            return Err(WorktreeError::TargetExists {
                to: target_name.to_string(),
            }
            .into());
        }
    }

    // 3. Check if branch exists with target name
    if repo.find_branch(target_name, BranchType::Local).is_ok() {
        return Err(WorktreeError::TargetExists {
            to: target_name.to_string(),
        }
        .into());
    }

    // 4. Check if source is protected (unless --force)
    if !options.force {
        let config = WorkonConfig::new(repo)?;
        let branch_name = source.branch()?.unwrap();
        if config.is_protected(&branch_name) {
            return Err(WorktreeError::ProtectedBranchMove(branch_name).into());
        }
    }

    // 5. Check if dirty (unless --force)
    if !options.force && source.is_dirty()? {
        return Err(WorktreeError::DirtyWorktree.into());
    }

    // 6. Check if unpushed (unless --force)
    if !options.force && source.has_unpushed_commits()? {
        return Err(WorktreeError::UnpushedCommits.into());
    }

    Ok(())
}
