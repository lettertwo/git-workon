use git2::Repository;
use std::{env, path::PathBuf};

use crate::error::Result;

pub fn get_repo(path: Option<PathBuf>) -> Result<Repository> {
    if let Some(path) = path {
        return Ok(Repository::discover(path)?);
    }

    let cwd = env::current_dir()?;
    Ok(Repository::discover(cwd)?)
}
