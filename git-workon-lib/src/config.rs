//! Configuration system for git-workon.
//!
//! This module provides the foundation for all git-workon configuration through git's
//! native config system (.git/config, ~/.gitconfig, /etc/gitconfig).
//!
//! **Multi-value support**: Git config naturally supports multi-value entries, perfect for
//! patterns, hooks, and other list-based configuration:
//!
//! ```bash
//! git config --add workon.copyPattern '.env*'
//! git config --add workon.copyPattern '.vscode/'
//! git config --get-all workon.copyPattern
//! ```
//!
//! **Precedence**: CLI arguments > local config (.git/config) > global config (~/.gitconfig) > defaults
//!
//! ## Configuration Keys
//!
//! This module supports the following configuration keys:
//!
//! - **workon.defaultBranch** - Default base branch for new worktrees (string, default: None)
//! - **workon.postCreateHook** - Commands to run after worktree creation (multi-value, default: [])
//! - **workon.copyPattern** - Glob patterns for automatic file copying (multi-value, default: [])
//! - **workon.copyExclude** - Patterns to exclude from copying (multi-value, default: [])
//! - **workon.autoCopyUntracked** - Enable automatic file copying in new command (bool, default: false)
//! - **workon.pruneProtectedBranches** - Branches protected from pruning (multi-value, default: [])
//! - **workon.prFormat** - Format string for PR-based worktree names (string, default: "pr-{number}")
//! - **workon.hookTimeout** - Timeout in seconds for hook execution (integer, default: 300, 0 = no timeout)
//!
//! ## Example Configuration
//!
//! ```gitconfig
//! # Global config (~/.gitconfig) - personal preferences
//! [workon]
//!   defaultBranch = main
//!
//! # Per-repo config (.git/config) - project-specific
//! [workon]
//!   postCreateHook = npm install
//!   postCreateHook = cp ../.env .env
//!   copyPattern = .env.local
//!   copyPattern = .vscode/
//!   copyExclude = .env.production
//!   autoCopyUntracked = true
//!   pruneProtectedBranches = main
//!   pruneProtectedBranches = develop
//!   pruneProtectedBranches = release/*
//!   prFormat = pr-{number}
//! ```

use std::time::Duration;

use git2::Repository;

use crate::error::{ConfigError, Result};

/// Configuration reader for workon settings stored in git config.
///
/// This struct provides access to workon-specific configuration keys,
/// handling precedence between CLI arguments, local config, and global config.
pub struct WorkonConfig<'repo> {
    repo: &'repo Repository,
}

impl<'repo> WorkonConfig<'repo> {
    /// Create a new config reader for the given repository.
    ///
    /// This opens the repository's git config, which automatically handles
    /// precedence: local config (.git/config) > global config (~/.gitconfig) > system config.
    pub fn new(repo: &'repo Repository) -> Result<Self> {
        Ok(Self { repo })
    }

    /// Get the default branch to use when creating new worktrees.
    ///
    /// Precedence: CLI override > workon.defaultBranch config > None
    ///
    /// Returns None if not configured. Callers can fall back to init.defaultBranch or "main".
    pub fn default_branch(&self, cli_override: Option<&str>) -> Result<Option<String>> {
        // CLI takes precedence
        if let Some(override_val) = cli_override {
            return Ok(Some(override_val.to_string()));
        }

        // Read from git config
        let config = self.repo.config()?;
        match config.get_string("workon.defaultBranch") {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None), // Not configured
        }
    }

    /// Get the format string for PR-based worktree names.
    ///
    /// Precedence: CLI override > workon.prFormat config > "pr-{number}"
    ///
    /// The format string must contain `{number}` placeholder for the PR number.
    /// Returns an error if the format is invalid.
    pub fn pr_format(&self, cli_override: Option<&str>) -> Result<String> {
        let format = if let Some(override_val) = cli_override {
            override_val.to_string()
        } else {
            let config = self.repo.config()?;
            config
                .get_string("workon.prFormat")
                .unwrap_or_else(|_| "pr-{number}".to_string())
        };

        // Validate format contains {number} placeholder
        if !format.contains("{number}") {
            return Err(ConfigError::InvalidPrFormat {
                format: format.clone(),
                reason: "Format must contain {number} placeholder".to_string(),
            }
            .into());
        }

        // Valid placeholders: {number}, {title}, {author}, {branch}
        let valid_placeholders = ["{number}", "{title}", "{author}", "{branch}"];
        let mut remaining = format.clone();
        for placeholder in &valid_placeholders {
            remaining = remaining.replace(placeholder, "");
        }

        // Check for invalid placeholders (anything still matching {.*})
        if remaining.contains('{') {
            return Err(ConfigError::InvalidPrFormat {
                format: format.clone(),
                reason: format!(
                    "Invalid placeholder found. Valid placeholders: {}",
                    valid_placeholders.join(", ")
                ),
            }
            .into());
        }

        Ok(format)
    }

    /// Get the list of post-create hook commands to run after worktree creation.
    ///
    /// Reads from multi-value workon.postCreateHook config.
    /// Returns empty Vec if not configured.
    pub fn post_create_hooks(&self) -> Result<Vec<String>> {
        self.read_multivar("workon.postCreateHook")
    }

    /// Get the list of glob patterns for files to copy between worktrees.
    ///
    /// Reads from multi-value workon.copyPattern config.
    /// Returns empty Vec if not configured.
    pub fn copy_patterns(&self) -> Result<Vec<String>> {
        self.read_multivar("workon.copyPattern")
    }

    /// Get the list of glob patterns for files to exclude from copying.
    ///
    /// Reads from multi-value workon.copyExclude config.
    /// Returns empty Vec if not configured.
    pub fn copy_excludes(&self) -> Result<Vec<String>> {
        self.read_multivar("workon.copyExclude")
    }

    /// Get whether to automatically copy untracked files when creating new worktrees.
    ///
    /// Precedence: CLI override > workon.autoCopyUntracked config > false
    ///
    /// When enabled, files matching workon.copyPattern (excluding workon.copyExclude)
    /// will be automatically copied from the base worktree to the new worktree.
    pub fn auto_copy_untracked(&self, cli_override: Option<bool>) -> Result<bool> {
        // CLI takes precedence
        if let Some(override_val) = cli_override {
            return Ok(override_val);
        }

        // Read from git config
        let config = self.repo.config()?;
        match config.get_bool("workon.autoCopyUntracked") {
            Ok(val) => Ok(val),
            Err(_) => Ok(false), // Default to false
        }
    }

    /// Get the list of branch patterns to protect from pruning.
    ///
    /// Reads from multi-value workon.pruneProtectedBranches config.
    /// Patterns support simple glob matching (* and ?).
    /// Returns empty Vec if not configured.
    pub fn prune_protected_branches(&self) -> Result<Vec<String>> {
        self.read_multivar("workon.pruneProtectedBranches")
    }

    /// Check if a given branch name is protected from pruning.
    ///
    /// Returns true if the branch name matches any of the protected patterns.
    pub fn is_protected(&self, branch_name: &str) -> bool {
        let patterns = match self.prune_protected_branches() {
            Ok(p) => p,
            Err(_) => return false,
        };
        // Same logic as prune command
        for pattern in patterns {
            if pattern == branch_name {
                return true;
            }
            if pattern == "*" {
                return true;
            }
            if let Some(prefix) = pattern.strip_suffix("/*") {
                if branch_name.starts_with(&format!("{}/", prefix)) {
                    return true;
                }
            }
        }
        false
    }

    /// Get the timeout duration for hook execution.
    ///
    /// Reads from workon.hookTimeout config (integer seconds).
    /// Default: 300 seconds (5 minutes). A value of 0 disables the timeout.
    pub fn hook_timeout(&self) -> Result<Duration> {
        let config = self.repo.config()?;
        let seconds = match config.get_i64("workon.hookTimeout") {
            Ok(val) => val.max(0) as u64,
            Err(_) => 300,
        };
        Ok(Duration::from_secs(seconds))
    }

    /// Helper to read multi-value config entries.
    ///
    /// Returns an empty Vec if the key doesn't exist.
    fn read_multivar(&self, key: &str) -> Result<Vec<String>> {
        let config = self.repo.config()?;
        let mut values = Vec::new();

        // Key doesn't exist, return empty vec
        if let Ok(mut entries) = config.multivar(key, None) {
            while let Some(entry) = entries.next() {
                let entry = entry?;
                if let Some(value) = entry.value() {
                    values.push(value.to_string());
                }
            }
        }

        Ok(values)
    }
}
