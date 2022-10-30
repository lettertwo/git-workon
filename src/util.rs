use std::{
    fs::{rename, write},
    path::Path,
};

use anyhow::{ensure, Ok, Result};
use git2::{Oid, Repository, Worktree, WorktreeAddOptions};

// TODO: Figure out how to make these atomic, i.e., undo anything that was done on failure.

pub fn empty_commit(repo: &Repository) -> Result<Oid> {
    let sig = repo.signature()?;
    let tree = repo.find_tree({
        let mut index = repo.index()?;
        index.write_tree()?
    })?;
    ensure!(tree.is_empty(), "Expected an empty index!");
    Ok(repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])?)
}

pub fn workon_root(repo: &Repository) -> Result<&Path> {
    Ok(repo.path().parent().unwrap())
}

pub fn convert_to_bare(repo: Repository) -> Result<Repository> {
    // git config core.bare true
    let mut config = repo.config()?;
    config.set_bool("core.bare", true)?;
    let root = workon_root(&repo)?;
    // mv .git .bare
    rename(repo.path(), root.join(".bare"))?;
    // create a git-link file: `echo "gitdir: ./.bare" > .git`
    write(root.join(".git"), "gitdir: ./.bare")?;
    Ok(Repository::open(root.join(".bare"))?)
}

pub fn add_default_worktree(repo: &Repository) -> Result<Worktree> {
    // git worktree add `defaultbranch`
    let config = repo.config()?;
    let defaultbranch = config.get_str("init.defaultbranch").unwrap_or("main");
    let reference = repo
        .find_branch(&defaultbranch, git2::BranchType::Local)?
        .into_reference();
    let root = workon_root(repo)?;
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    Ok(repo.worktree(
        &defaultbranch,
        root.join(&defaultbranch).as_path(),
        Some(&opts),
    )?)
}
