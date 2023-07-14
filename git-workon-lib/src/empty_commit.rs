use anyhow::{ensure, Ok, Result};
use git2::{Oid, Repository};

pub fn empty_commit(repo: &Repository) -> Result<Oid> {
    let sig = repo.signature()?;
    let tree = repo.find_tree({
        let mut index = repo.index()?;
        index.write_tree()?
    })?;
    ensure!(tree.is_empty(), "Expected an empty index!");
    Ok(repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])?)
}
