use std::fs::{rename, write};

use git2::Repository;
use log::debug;
use miette::{IntoDiagnostic, Result};

use super::workon_root;

pub fn convert_to_bare(mut repo: Repository) -> Result<Repository> {
    debug!("Converting to bare repository");
    // git config core.bare true
    let mut config = repo.config().into_diagnostic()?;
    config.set_bool("core.bare", true).into_diagnostic()?;
    let root = workon_root(&repo)?;
    // mv .git .bare
    rename(repo.path(), root.join(".bare")).into_diagnostic()?;
    // create a git-link file: `echo "gitdir: ./.bare" > .git`
    write(root.join(".git"), "gitdir: ./.bare").into_diagnostic()?;

    repo = Repository::open(root.join(".bare")).into_diagnostic()?;
    repo.remote_add_fetch("origin", "+refs/heads/*:refs/remotes/origin/*")
        .into_diagnostic()?;

    Ok(repo)
}
