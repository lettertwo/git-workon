use std::path::PathBuf;

use miette::Result;
use workon::{add_worktree, get_default_branch_name, init, BranchType, WorktreeDescriptor};

use crate::cli::Init;

use super::Run;

impl Run for Init {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));
        let repo = init(path)?;
        let default_branch = get_default_branch_name(&repo, None)?;
        add_worktree(&repo, &default_branch, BranchType::default()).map(Some)
    }
}
