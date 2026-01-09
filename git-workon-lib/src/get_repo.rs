use git2::Repository;
use std::{env, path::PathBuf};

use crate::{error::Result, RepoError};

pub fn get_repo(path: Option<PathBuf>) -> Result<Repository> {
    let path = match path {
        Some(p) => p,
        None => env::current_dir()?,
    };

    let mut repo = Repository::discover(path)?;

    if repo.is_worktree() {
        repo = Repository::discover(repo.commondir())?;
    }

    match repo.is_bare() {
        true => Ok(repo),
        false => Err(RepoError::NotBare(repo.path().display().to_string()).into()),
    }
}
