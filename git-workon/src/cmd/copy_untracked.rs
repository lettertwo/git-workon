use miette::{Result, WrapErr};
use workon::{get_repo, workon_root, WorkonConfig, WorktreeDescriptor};

use crate::cli::CopyUntracked;
use crate::copy::copy_files;

use super::Run;

impl Run for CopyUntracked {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let config = WorkonConfig::new(&repo)?;

        // Get worktree root directory
        let root = workon_root(&repo)?;

        // Resolve worktree paths from names
        let from_path = root.join(&self.from);
        let to_path = root.join(&self.to);

        // Verify both worktrees exist
        if !from_path.exists() {
            return Err(miette::miette!(
                "Source worktree '{}' does not exist at {:?}",
                self.from,
                from_path
            ));
        }
        if !to_path.exists() {
            return Err(miette::miette!(
                "Destination worktree '{}' does not exist at {:?}",
                self.to,
                to_path
            ));
        }

        // Determine patterns: --pattern > --auto > default "*"
        let patterns = determine_patterns(self, &config)?;
        let excludes = config.copy_excludes()?;

        // Copy files
        let copied = copy_files(&from_path, &to_path, &patterns, &excludes, self.force).wrap_err(
            format!("Failed to copy files from '{}' to '{}'", self.from, self.to),
        )?;

        // Print results
        for file in &copied {
            println!("Copied: {}", file.display());
        }
        println!("\nCopied {} file(s)", copied.len());

        // Return the destination worktree descriptor
        Ok(Some(WorktreeDescriptor::new(&repo, &self.to)?))
    }
}

/// Determine which patterns to use for copying
///
/// Priority: --pattern flag > --auto (config) > default "*"
fn determine_patterns(cmd: &CopyUntracked, config: &WorkonConfig) -> Result<Vec<String>> {
    // If --pattern is specified, use it (overrides everything)
    if let Some(pattern) = &cmd.pattern {
        return Ok(vec![pattern.clone()]);
    }

    // If --auto is specified, use config patterns
    if cmd.auto {
        let patterns = config.copy_patterns()?;
        if !patterns.is_empty() {
            return Ok(patterns);
        }
        // If auto is set but no patterns configured, return error
        return Err(miette::miette!(
            "No copy patterns configured. Set workon.copyPattern or use --pattern"
        ));
    }

    // Default: copy everything recursively
    Ok(vec!["**/*".to_string()])
}
