use git2::Repository;
use miette::{IntoDiagnostic, Result};
use std::{env, path::PathBuf};

pub fn get_repo(path: Option<PathBuf>) -> Result<Repository> {
    if let Some(path) = path {
        return Repository::discover(path).into_diagnostic();
    }

    let cwd = env::current_dir().into_diagnostic()?;
    Repository::discover(cwd).into_diagnostic()
}
