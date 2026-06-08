//! Checkout command — in-place branch checkout inside a stack home worktree.
//!
//! This command is hidden and produced solely by the resolver (`main.rs`) when
//! `workon <T>` resolves to [`workon::Resolution::Checkout`]. It is never typed
//! directly by users.
//!
//! ## What it does
//!
//! 1. Finds the host worktree `W` by name.
//! 2. Calls `workon::checkout_branch_in_worktree(W, T)` to move HEAD inside `W`.
//! 3. On conflict, returns a hard error (interactive stash-and-retry is added in PR-3).
//! 4. Returns `Ok(Some(host_wt))` so `main` prints `W`'s path and the shell `cd`s there.

use miette::{bail, IntoDiagnostic, Result};
use workon::WorktreeDescriptor;

use crate::cli::Checkout;
use crate::cmd::Run;

impl Run for Checkout {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = workon::get_repo(None).into_diagnostic()?;

        let host_wt = workon::find_worktree(&repo, &self.host_worktree).into_diagnostic()?;

        match workon::checkout_branch_in_worktree(&host_wt, &self.branch).into_diagnostic()? {
            workon::CheckoutOutcome::Clean => {}
            workon::CheckoutOutcome::Conflict { paths } => {
                let path_list = paths.join(", ");
                bail!(
                    "checkout of '{}' conflicts with uncommitted changes in: {}\n\
                     Commit or stash those changes, then re-run.",
                    self.branch,
                    path_list
                );
            }
        }

        Ok(Some(host_wt))
    }
}
