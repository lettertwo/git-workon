use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::Prune;

use super::Run;

// Cleanup:
//
//   `git worktree remove ../lettertwo/some-thing \
//     && git branch -d lettertwo/some-thing`
//
// Cleanup remote:
//
//   `git worktree remove ../bdo/browser-reporter \
//     && git branch -d bdo/browser-reporter \
//     && git push --delete origin bdo/browser-reporter`

impl Run for Prune {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        unimplemented!("prune not implemented!");
    }
}
