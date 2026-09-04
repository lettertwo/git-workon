//! Whole-file operations (trap 3): the staging verbs a hunk patch cannot express, because a
//! hunk patch always has BOTH a pre-image and a post-image to diff between. Creations,
//! deletions, and untracked files each have only one side — synthesizing a hunk patch for them
//! either has no preimage to apply against (untracked: git rejects it) or stages an EMPTY BLOB
//! instead of removing the file (deleted: git happily accepts a patch that deletes every line
//! of a tracked file, but that isn't the same operation as removing the index entry). `ops.rs`
//! routes those statuses here instead of through `synthesis`/`apply`.

use std::path::Path;

use git2::Repository;

use crate::error::ApplyError;

/// Stage `path`: `index.add_path` when the working-tree copy exists (covers Added, Untracked,
/// and Modified — an ordinary content update), `index.remove_path` when it doesn't (a real
/// deletion: `git rm`'s effect, not a hunk patch that would stage an empty blob).
///
/// The choice is made by checking the working tree on disk, not by trusting a `FileStatus`
/// passed in by the caller — the two can only usefully agree once the check runs, so the check
/// is the source of truth (trap 3's core fix).
///
/// The presence check uses `symlink_metadata` (lstat), NOT `Path::exists` (which follows
/// symlinks and reports `false` for a broken one). An untracked BROKEN symlink is still a real
/// working-tree entry that `git add` stages (as the link text, like any other symlink) —
/// `Path::exists` would silently take the `remove_path` branch instead, a no-op that leaves the
/// symlink unstaged with no error.
pub fn stage_file(repo: &Repository, path: &str) -> Result<(), ApplyError> {
    let workdir = repo
        .workdir()
        .expect("stage_file requires a repository with a working directory");
    let mut index = repo.index()?;
    if workdir.join(path).symlink_metadata().is_ok() {
        index.add_path(Path::new(path))?;
    } else {
        index.remove_path(Path::new(path))?;
    }
    index.write()?;
    Ok(())
}

/// Unstage `path`: reset its index entry back to `HEAD`. `reset_default` handles the
/// staged-new-file case natively — when `path` has no `HEAD` entry, the index entry is removed
/// outright and the file becomes untracked again, exactly like `git reset HEAD -- path` on a
/// newly-added file.
pub fn unstage_file(repo: &Repository, path: &str) -> Result<(), ApplyError> {
    let head = repo.head()?.peel(git2::ObjectType::Commit)?;
    repo.reset_default(Some(&head), [path])?;
    Ok(())
}

/// Discard `path`'s working-tree changes: check out the INDEX'S copy over the working tree
/// copy, without touching the index (`update_index(false)` — a discard must not also stage
/// anything). This is `git restore <path>`'s effect, NOT `git checkout HEAD -- <path>`'s: on a
/// partially staged file (`HEAD` = A, index = B, workdir = C), discarding must revert the
/// workdir to what's staged (B), not blow past it to HEAD (A) and silently wipe the staged
/// work. `checkout_head` restores from `HEAD` and was wrong for exactly this reason — it's only
/// correct by coincidence when nothing is staged (index == HEAD).
pub fn discard_file(repo: &Repository, path: &str) -> Result<(), ApplyError> {
    let mut opts = git2::build::CheckoutBuilder::new();
    opts.path(path).force().update_index(false);
    repo.checkout_index(None, Some(&mut opts))?;
    Ok(())
}

/// Discard an untracked file: there is no `HEAD`/index copy to check out, so "discard" means
/// deleting the working-tree file outright.
pub fn clean_untracked(repo: &Repository, path: &str) -> Result<(), ApplyError> {
    let workdir = repo
        .workdir()
        .expect("clean_untracked requires a repository with a working directory");
    std::fs::remove_file(workdir.join(path)).map_err(|source| ApplyError::Io {
        path: path.to_string(),
        source,
    })
}
