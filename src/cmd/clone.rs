use std::path::PathBuf;

use anyhow::Result;

use log::debug;

use crate::{
    cli::Clone,
    util::{add_worktree, clone_bare_single_branch, convert_to_bare, get_default_branch_name},
};

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));

        // 1. git clone --single-branch <url>.git <path>/.bare
        let repo = clone_bare_single_branch(path, &self.url)?;

        // 2. $ echo "gitdir: ./.bare" > .git
        // 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
        let repo = convert_to_bare(repo)?;

        // 4. $ git fetch
        // 5. $ git worktree add --track <branch> origin/<branch>
        let default_branch = get_default_branch_name(&repo, repo.find_remote("origin").ok())?;
        add_worktree(&repo, &default_branch)?;

        debug!("Done");

        Ok(())
    }
}
