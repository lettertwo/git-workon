use miette::Result;

use crate::cli::New;
use workon::{add_worktree, BranchType, get_repo, WorktreeDescriptor};

use super::Run;

// Ability to easily create a worktree with namespcaing.
// Also see: https://lists.mcs.anl.gov/pipermail/petsc-dev/2021-May/027436.html
//
// The anatomy of the command is:
//
//   `git worktree add --track -b <branch> ../<path> <remote>/<remote-branch>`
//
// we want `<branch>` to exactly match `<remote-branch>`
// We want `<path>` to exactly match `<branch>`
//
// Use case: checking out an existing branch
//
//   `git worktree add --track -b bdo/browser-reporter ../bdo/browser-reporter origin/bdo/browser-reporter`
//
// Use case: creating a new branch
// In this case, we aren't tracking a remote (yet?)
//
//   `git worktree add -b lettertwo/some-thing ../lettertwo/some-thing`
//
// Hooks: on creation, we will often want to copy artifacts from the base worktree (e.g., node_modules, build dirs)
// One approach to this is the `copyuntracked` util that can (perhaps interactively?) copy over
// any untracked or git ignored files. It would be nice if this script was also SCM-aware, in that it could
// suggest rebuilds, or re-running install, etc, if the base artifacts are much older than the new worktree HEAD.

impl Run for New {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let name = match &self.name {
            Some(name) => name,
            None => {
                unimplemented!("Interactive new not implemented!");
            }
        };
        let repo = get_repo(None)?;
        let branch_type = if self.orphan {
            BranchType::Orphan
        } else if self.detach {
            BranchType::Detached
        } else {
            BranchType::Normal
        };
        add_worktree(&repo, name, branch_type).map(Some)
    }
}
