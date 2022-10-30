use std::path::PathBuf;

use anyhow::{Ok, Result};
use git2::Repository;

use crate::{
    cli::Init,
    util::{add_default_worktree, convert_to_bare, empty_commit},
};

use super::Run;

impl Run for Init {
    fn run(&self) -> Result<()> {
        // 1. git init
        let path = self.name.clone().unwrap_or_else(|| PathBuf::from("."));
        let mut repo = Repository::init(&path)?;

        // 2. Add an initial (empty) commit. We need this to create a valid HEAD.
        empty_commit(&repo)?;

        // 3. git config core.bare true
        repo = convert_to_bare(repo)?;

        // 4. git worktree add `defaultbranch`
        add_default_worktree(&repo)?;

        Ok(())
    }
}
