use miette::Result;
use workon::{get_repo, get_worktrees};

use crate::cli::List;

use super::Run;

impl Run for List {
    fn run(&self) -> Result<()> {
        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;
        for worktree in &worktrees {
            println!("{}", worktree);
        }

        Ok(())
    }
}
