use std::fs::{rename, write};

use git2::Repository;
use log::debug;

use crate::error::Result;
use crate::workon_root;

/// Convert a standard git repository into the worktrees layout.
///
/// Performs the following steps:
/// 1. Sets `core.bare = true` in git config
/// 2. Renames `.git` to `.bare`
/// 3. Writes a `.git` link file containing `gitdir: ./.bare`
/// 4. Configures `origin`'s fetch refspec to mirror all branches
///
/// Returns the re-opened bare repository.
pub fn convert_to_bare(mut repo: Repository) -> Result<Repository> {
    debug!("Converting to bare repository");
    // git config core.bare true
    let mut config = repo.config()?;
    config.set_bool("core.bare", true)?;
    let root = workon_root(&repo)?;
    // mv .git .bare
    rename(repo.path(), root.join(".bare"))?;
    // create a git-link file: `echo "gitdir: ./.bare" > .git`
    write(root.join(".git"), "gitdir: ./.bare")?;

    repo = Repository::open(root.join(".bare"))?;
    repo.remote_add_fetch("origin", "+refs/heads/*:refs/remotes/origin/*")?;

    Ok(repo)
}
