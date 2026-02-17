//! Post-creation hook execution.
//!
//! This module executes user-configured commands automatically after worktree creation,
//! providing a simpler alternative to git's native `post-checkout` hook.
//!
//! ## Design: Hybrid Approach
//!
//! Git's native `post-checkout` hook fires on `git worktree add`, but has limitations:
//! - Only one script per hook (requires manual multiplexing for multiple behaviors)
//! - Fires for ALL checkouts, requiring conditional logic to detect worktree creation
//! - Requires shell scripting knowledge
//!
//! The `workon.postCreateHook` config provides a simpler alternative:
//! - Only runs for `git workon new/init/clone` (explicit, no detection needed)
//! - No scripting required: `git config --add workon.postCreateHook "npm install"`
//! - Doesn't conflict with existing post-checkout hooks
//! - Multi-value config allows multiple commands to run sequentially
//!
//! Both approaches work together:
//! 1. Git's post-checkout runs first (standard git behavior)
//! 2. Then workon.postCreateHook commands run (if configured)
//! 3. `--no-hooks` flag skips both (respects user intent)
//!
//! ## Environment Variables
//!
//! Hooks receive these environment variables:
//! - `WORKON_WORKTREE_PATH` - Absolute path to the new worktree
//! - `WORKON_BRANCH_NAME` - Branch name (if not detached HEAD)
//! - `WORKON_BASE_BRANCH` - Base branch used for creation (if applicable)
//!
//! ## Example Usage
//!
//! ```bash
//! # Simple setup command
//! git config --add workon.postCreateHook "npm install"
//!
//! # Multiple hooks run sequentially
//! git config --add workon.postCreateHook "cargo build"
//! git config --add workon.postCreateHook "cp ../.env .env"
//! ```
//!
//! ## Security Considerations
//!
//! Hooks execute arbitrary commands from config. Users should:
//! - Only set hooks in trusted repositories
//! - Review project config before cloning untrusted repositories
//! - Use `--no-hooks` flag when working with untrusted code
//!
//! ## Git Native Alternative
//!
//! For power users who prefer git's native hooks, use `.git/hooks/post-checkout`:
//!
//! ```bash
//! #!/bin/bash
//! # .git/hooks/post-checkout
//! # Detects worktree creation by checking if previous HEAD is all zeros
//! if [ "$1" = "0000000000000000000000000000000000000000" ]; then
//!     echo "New worktree created at $PWD"
//!     npm install
//! fi
//! ```
//!
//! ## Timeout Protection
//!
//! Hooks are subject to a configurable timeout (`workon.hookTimeout`, default 300s).
//! If a hook exceeds the timeout, it is killed and an error is returned.
//! Set `workon.hookTimeout` to `0` to disable the timeout.

use std::env;
use std::process::Command;
use std::thread;
use std::time::Instant;

use log::debug;
use miette::{IntoDiagnostic, Result};
use workon::{WorkonConfig, WorktreeDescriptor};

/// Execute post-creation hooks configured in workon.postCreateHook
///
/// Hooks are executed sequentially in the worktree directory with environment variables set.
/// If a hook fails, an error is returned but the worktree remains valid.
pub fn execute_post_create_hooks(
    worktree: &WorktreeDescriptor,
    base_branch: Option<&str>,
    config: &WorkonConfig,
) -> Result<()> {
    let hooks = config.post_create_hooks()?;

    if hooks.is_empty() {
        debug!("No post-create hooks configured");
        return Ok(());
    }

    debug!("Found {} post-create hook(s)", hooks.len());

    for (i, hook_cmd) in hooks.iter().enumerate() {
        eprintln!("Running hook {}/{}: {}", i + 1, hooks.len(), hook_cmd);

        // Set up environment variables for the hook
        debug!("Setting WORKON_WORKTREE_PATH={}", worktree.path().display());
        env::set_var("WORKON_WORKTREE_PATH", worktree.path());

        if let Ok(Some(branch)) = worktree.branch() {
            debug!("Setting WORKON_BRANCH_NAME={}", branch);
            env::set_var("WORKON_BRANCH_NAME", branch);
        }

        if let Some(base) = base_branch {
            debug!("Setting WORKON_BASE_BRANCH={}", base);
            env::set_var("WORKON_BASE_BRANCH", base);
        }

        debug!(
            "Executing in working directory: {}",
            worktree.path().display()
        );

        // Execute using shell (platform-dependent)
        let mut child = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", hook_cmd])
                .current_dir(worktree.path())
                .spawn()
        } else {
            Command::new("sh")
                .args(["-c", hook_cmd])
                .current_dir(worktree.path())
                .spawn()
        }
        .into_diagnostic()?;

        let timeout = config.hook_timeout()?;

        if timeout.is_zero() {
            // No timeout - blocking wait
            let status = child.wait().into_diagnostic()?;
            if !status.success() {
                return Err(miette::miette!(
                    "Hook failed with exit code: {:?}",
                    status.code()
                ));
            }
        } else {
            let start = Instant::now();
            loop {
                match child.try_wait().into_diagnostic()? {
                    Some(status) if status.success() => break,
                    Some(status) => {
                        return Err(miette::miette!(
                            "Hook failed with exit code: {:?}",
                            status.code()
                        ));
                    }
                    None if start.elapsed() >= timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(miette::miette!(
                            "Hook timed out after {}s: {}",
                            timeout.as_secs(),
                            hook_cmd
                        ));
                    }
                    None => thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
        }

        eprintln!("✓ Hook completed successfully");
    }

    Ok(())
}
