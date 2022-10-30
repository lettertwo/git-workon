use anyhow::{bail, Result};

use crate::cli::Clone;

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<()> {
        // 1. git clone --bare --single-branch <atlassian-url>.git .bare
        // 2. $ echo "gitdir: ./.bare" > .git
        // 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
        // 4. $ git fetch
        // 5. $ git worktree add --track main origin/main
        bail!("Clone Not implemented");
    }
}
