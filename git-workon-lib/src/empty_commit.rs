use git2::{Oid, Repository};
use miette::{ensure, IntoDiagnostic, Result};

pub fn empty_commit(repo: &Repository) -> Result<Oid> {
    let sig = repo.signature().into_diagnostic()?;
    let tree = repo
        .find_tree({
            let mut index = repo.index().into_diagnostic()?;
            index.write_tree().into_diagnostic()?
        })
        .into_diagnostic()?;
    ensure!(tree.is_empty(), "Expected an empty index!");
    Ok(repo
        .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
        .into_diagnostic()?)
}
