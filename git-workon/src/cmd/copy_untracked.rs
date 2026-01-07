use miette::{Result, WrapErr};
use workon::{copy_files, get_repo, workon_root, WorkonConfig, WorktreeDescriptor};

use crate::cli::CopyUntracked;

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

        // Determine patterns: --pattern flag > config > error
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
/// Priority: --pattern flag > config > default **/*
fn determine_patterns(cmd: &CopyUntracked, config: &WorkonConfig) -> Result<Vec<String>> {
    // If --pattern is specified, use it (overrides everything)
    if let Some(pattern) = &cmd.pattern {
        return Ok(vec![pattern.clone()]);
    }

    // Use config patterns if configured
    let patterns = config.copy_patterns()?;
    if !patterns.is_empty() {
        return Ok(patterns);
    }

    // Default: copy everything
    Ok(vec!["**/*".to_string()])
}
