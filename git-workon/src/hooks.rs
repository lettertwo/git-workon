use std::env;
use std::process::Command;

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
        return Ok(());
    }

    for (i, hook_cmd) in hooks.iter().enumerate() {
        eprintln!("Running hook {}/{}: {}", i + 1, hooks.len(), hook_cmd);

        // Set up environment variables for the hook
        env::set_var("WORKON_WORKTREE_PATH", worktree.path());

        if let Ok(Some(branch)) = worktree.branch() {
            env::set_var("WORKON_BRANCH_NAME", branch);
        }

        if let Some(base) = base_branch {
            env::set_var("WORKON_BASE_BRANCH", base);
        }

        // Execute using shell (platform-dependent)
        let result = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", hook_cmd])
                .current_dir(worktree.path())
                .status()
        } else {
            Command::new("sh")
                .args(["-c", hook_cmd])
                .current_dir(worktree.path())
                .status()
        };

        match result.into_diagnostic()? {
            status if status.success() => {
                eprintln!("✓ Hook completed successfully");
            }
            status => {
                return Err(miette::miette!(
                    "Hook failed with exit code: {:?}",
                    status.code()
                ));
            }
        }
    }

    Ok(())
}
