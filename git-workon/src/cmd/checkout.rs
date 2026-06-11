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
//! 3. On conflict, prompts "Leave changes behind (shelve) and continue?" unless
//!    `no_interactive` is set (propagated from `--json`), in which case it bails
//!    with a structured `CheckoutError::Conflict`.
//! 4. On "Leave": creates a labeled stash for the current branch, retries checkout.
//! 5. After a successful HEAD move, attempts to restore any stash previously left
//!    for `T` in `W` (restore-on-return, gated on `!no_stack`).
//! 6. Returns `Ok(Some(host_wt))` so `main` prints `W`'s path and the shell `cd`s there.

use dialoguer::Confirm;
use miette::{bail, IntoDiagnostic, Report, Result};
use workon::WorktreeDescriptor;

use crate::cli::Checkout;
use crate::cmd::Run;
use crate::output;

impl Run for Checkout {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = workon::get_repo(None).into_diagnostic()?;
        let host_wt = workon::find_worktree(&repo, &self.host_worktree).into_diagnostic()?;

        match workon::checkout_branch_in_worktree(&host_wt, &self.branch).into_diagnostic()? {
            workon::CheckoutOutcome::Clean => {}
            workon::CheckoutOutcome::Conflict { paths } => {
                if self.no_interactive {
                    return Err(Report::from(workon::CheckoutError::Conflict {
                        branch: self.branch.clone(),
                        path: paths.first().cloned().unwrap_or_default(),
                    }));
                }

                let path_list = paths.join(", ");
                let confirmed = Confirm::new()
                    .with_prompt(format!(
                        "Checkout of '{}' conflicts with changes in: {}.\nLeave changes behind (shelve) and continue?",
                        self.branch, path_list
                    ))
                    .default(false)
                    .interact()
                    .into_diagnostic()?;

                if !confirmed {
                    return Err(Report::from(workon::CheckoutError::Aborted));
                }

                // Stash the conflicting changes labeled for the current branch so
                // returning to it later can restore them.
                let host_branch = host_wt
                    .branch()
                    .into_diagnostic()?
                    .unwrap_or_else(|| self.host_worktree.clone());
                let mut wt_repo = git2::Repository::open(host_wt.path()).into_diagnostic()?;
                workon::create_labeled_stash(&mut wt_repo, &host_branch, &self.host_worktree)
                    .into_diagnostic()?;

                // Retry now that the working tree is clean.
                match workon::checkout_branch_in_worktree(&host_wt, &self.branch)
                    .into_diagnostic()?
                {
                    workon::CheckoutOutcome::Clean => {}
                    workon::CheckoutOutcome::Conflict { paths } => {
                        bail!(
                            "checkout still conflicts after shelving changes: {}",
                            paths.join(", ")
                        );
                    }
                }
            }
        }

        // Restore-on-return: if T had shelved changes in W from a previous visit,
        // apply them now. Gated on stack mode being active.
        if !self.no_stack {
            let mut wt_repo = git2::Repository::open(host_wt.path()).into_diagnostic()?;
            match workon::apply_labeled_stash(&mut wt_repo, &self.branch, &self.host_worktree)
                .into_diagnostic()?
            {
                workon::StashRestore::Applied => {
                    output::info(&format!("restored shelved changes for '{}'", self.branch));
                }
                workon::StashRestore::Conflict => {
                    output::warn(&format!(
                        "conflicts while restoring shelved changes for '{}' (entry kept in stash)",
                        self.branch
                    ));
                }
                workon::StashRestore::NotFound => {}
            }
        }

        Ok(Some(host_wt))
    }
}
