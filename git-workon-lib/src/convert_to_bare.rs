use std::fs::{rename, write};

use git2::Repository;
use log::debug;

use crate::error::Result;
use crate::workon_root;

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
