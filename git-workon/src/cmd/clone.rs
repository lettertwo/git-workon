use std::path::PathBuf;

use miette::Result;
use workon::{add_worktree, clone, get_default_branch_name, WorktreeDescriptor};

use crate::cli::Clone;

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let path = self.path.clone().unwrap_or_else(|| {
            PathBuf::from(
                self.url
                    .trim_end_matches('/')
                    .split('/')
                    .next_back()
                    .unwrap_or(".")
                    .trim_end_matches(".git"),
            )
        });

        let repo = clone(path, &self.url)?;
        let default_branch = get_default_branch_name(&repo, repo.find_remote("origin").ok())?;
        add_worktree(&repo, &default_branch).map(Some)
    }
}
