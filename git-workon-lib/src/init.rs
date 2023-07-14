use std::path::PathBuf;

use anyhow::Result;
use git2::Repository;
use log::debug;

use crate::{add_worktree, convert_to_bare, empty_commit, get_default_branch_name};

pub fn init(path: PathBuf) -> Result<Repository> {
    debug!("initializing bare repository at {}", path.display());

    let mut repo = Repository::init(&path)?;

    // 2. Add an initial (empty) commit. We need this to create a valid HEAD.
    empty_commit(&repo)?;

    // 3. git config core.bare true
    repo = convert_to_bare(repo)?;

    // 4. git worktree add --track <branch> origin/<branch>
    let default_branch = get_default_branch_name(&repo, None)?;
    add_worktree(&repo, &default_branch)?;

    Ok(repo)
}
