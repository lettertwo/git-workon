//! Doctor command for detecting and repairing workspace issues.
//!
//! Detect and repair workspace issues using git's native `git worktree repair` plus
//! additional workon-specific checks.
//!
//! ### Planned Detection:
//! - Orphaned worktrees (directory exists but not in git worktree list)
//! - Missing worktree directories (in git list but directory deleted)
//! - Broken git links (.git file pointing to non-existent location)
//! - Inconsistent administrative files
//! - Worktrees on stale/deleted branches
//!
//! ## Planned Flags:
//! - `--fix` - Automatically repair detected issues
//! - `--dry-run` - Preview fixes without applying

use crate::cli::Doctor;
use crate::Result;
use workon::WorktreeDescriptor;

use super::Run;

impl Run for Doctor {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        unimplemented!(
            "Doctor command is not yet implemented. \
             See git-workon/src/cmd/doctor.rs for implementation plan."
        )
    }
}
