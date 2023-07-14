use std::path::Path;

use anyhow::{Ok, Result};
use git2::Repository;

pub fn workon_root(repo: &Repository) -> Result<&Path> {
    Ok(repo.path().parent().unwrap())
}
