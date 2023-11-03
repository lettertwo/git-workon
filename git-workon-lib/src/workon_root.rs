use std::path::Path;

use git2::Repository;
use miette::Result;

pub fn workon_root(repo: &Repository) -> Result<&Path> {
    Ok(repo.path().parent().unwrap())
}
