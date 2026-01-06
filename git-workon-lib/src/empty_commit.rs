use git2::{Oid, Repository};

use crate::{error::Result, WorktreeError};

pub fn empty_commit(repo: &Repository) -> Result<Oid> {
    let sig = repo.signature()?;
    let tree = repo.find_tree({
        let mut index = repo.index()?;
        index.write_tree()?
    })?;

    if !tree.is_empty() {
        return Err(WorktreeError::NonEmptyIndex.into());
    }

    Ok(repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])?)
}
