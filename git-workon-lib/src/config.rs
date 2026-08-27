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
//! - **workon.prFormat** - Format string for PR-based worktree names (string, default: "pr-{number}")
//! - **workon.postCreateHook** - Commands to run after worktree creation (multi-value, default: [])
//! - **workon.hookTimeout** - Timeout in seconds for hook execution (integer, default: 300, 0 = no timeout)
//! - **workon.copyPattern** - Glob patterns for automatic file copying (multi-value, default: [])
//! - **workon.copyExclude** - Patterns to exclude from copying (multi-value, default: [])
//! - **workon.copyIncludeIgnored** - Include git-ignored files when copying (bool, default: true)
//! - **workon.autoCopy** - Enable automatic file copying in new command (bool, default: false)
//! - **workon.pruneProtectedBranches** - Branches protected from pruning (multi-value, default: [])
//! - **workon.pruneGone** - Treat gone-upstream worktrees as prune candidates by default (bool, default: false)
//! - **workon.pruneFetch** - Fetch from tracked remotes before evaluating gone status (bool, default: false)
//! - **workon.stackModel** - Active stack model: "auto", "graphite", "git", or "none" (string, default: "auto")
//! - **workon.stackWorktreeGranularity** - Worktree granularity for stacked diffs: "stack" (string, default: "stack")
//! - **workon.stackAutoTrack** - Auto-register new branches with the active stack tool after
//!   `workon new` (bool, default: true)
//! - **workon.gtAutoTrack** - Deprecated alias for `workon.stackAutoTrack`, read only when the
//!   latter is unset (bool, default: true)
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
//!   autoCopy = true
//!   pruneProtectedBranches = main
//!   pruneProtectedBranches = develop
//!   pruneProtectedBranches = release/*
//!   prFormat = pr-{number}
//! ```

use std::time::Duration;

use git2::Repository;

use crate::error::{ConfigError, Result, StackError};
use crate::stack::{Granularity, StackModel};

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

    /// Get whether to include git-ignored files when copying.
    ///
    /// Precedence: CLI override > workon.copyIncludeIgnored config > true
    ///
    /// Ignored files (e.g., `.env.local`, `node_modules/`) are included by default
    /// since they are the primary use case for copying between worktrees.
    /// Set `workon.copyIncludeIgnored = false` to opt out.
    pub fn copy_include_ignored(&self, cli_override: Option<bool>) -> Result<bool> {
        if let Some(override_val) = cli_override {
            return Ok(override_val);
        }

        let config = self.repo.config()?;
        match config.get_bool("workon.copyIncludeIgnored") {
            Ok(val) => Ok(val),
            Err(_) => Ok(true),
        }
    }

    /// Get whether to automatically copy local files when creating new worktrees.
    ///
    /// Precedence: CLI override > workon.autoCopy config > false
    ///
    /// When enabled, files matching workon.copyPattern (excluding workon.copyExclude)
    /// will be automatically copied from the base worktree to the new worktree.
    pub fn auto_copy(&self, cli_override: Option<bool>) -> Result<bool> {
        if let Some(override_val) = cli_override {
            return Ok(override_val);
        }

        let config = self.repo.config()?;
        match config.get_bool("workon.autoCopy") {
            Ok(val) => Ok(val),
            Err(_) => Ok(false),
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

    /// Get whether to include gone-upstream worktrees as prune candidates by default.
    ///
    /// Precedence: CLI override > workon.pruneGone config > false
    ///
    /// When true, `prune` treats worktrees with a gone upstream tracking branch as
    /// eligible for removal without requiring `--gone`. Equivalent to always passing
    /// `--gone`.
    pub fn prune_gone(&self, cli_override: Option<bool>) -> Result<bool> {
        if let Some(override_val) = cli_override {
            return Ok(override_val);
        }
        let config = self.repo.config()?;
        match config.get_bool("workon.pruneGone") {
            Ok(val) => Ok(val),
            Err(_) => Ok(false),
        }
    }

    /// Get whether to run a prune-fetch before evaluating gone-upstream status.
    ///
    /// Precedence: CLI override > workon.pruneFetch config > false
    ///
    /// When true, `prune` fetches from all remotes tracked by worktree branches
    /// (with `--prune`, deleting stale remote-tracking refs) before evaluating
    /// gone-upstream status. This makes `--gone` accurate even when local refs
    /// are stale. Equivalent to always passing `--fetch`.
    pub fn prune_fetch(&self, cli_override: Option<bool>) -> Result<bool> {
        if let Some(override_val) = cli_override {
            return Ok(override_val);
        }
        let config = self.repo.config()?;
        match config.get_bool("workon.pruneFetch") {
            Ok(val) => Ok(val),
            Err(_) => Ok(false),
        }
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

    /// Get the active stack model.
    ///
    /// Precedence: CLI override > workon.stackModel config > auto-detect.
    ///
    /// Auto-detection: returns `Graphite` when the repo has been `gt init`-ed
    /// (`.graphite_repo_config` or `.graphite_metadata.db` exists), else `GhStack` when a
    /// gh-stack file is present, else `None`. Graphite wins when both are present — see
    /// [`StackModel::detect`].
    ///
    /// Accepted config values: `"graphite"`, `"gh-stack"`, `"git"`, `"none"`, `"auto"`
    /// (re-runs detection). `"git"` opts into metadata-less git-inference
    /// ([`StackModel::Git`]) explicitly — it is never the result of `"auto"`. `"ghstack"`
    /// (no hyphen) is a *different* tool (Meta's Phabricator-style stacker) and is rejected
    /// as unsupported rather than treated as a typo for `"gh-stack"`. Anything else returns
    /// an error.
    pub fn stack_model(&self, cli_override: Option<&str>) -> Result<StackModel> {
        let raw = if let Some(val) = cli_override {
            Some(val.to_string())
        } else {
            let config = self.repo.config()?;
            config.get_string("workon.stackModel").ok()
        };

        match raw.as_deref() {
            None | Some("auto") => Ok(StackModel::detect(self.repo)),
            Some("none") => Ok(StackModel::None),
            Some("graphite") => Ok(StackModel::Graphite),
            Some("gh-stack") => Ok(StackModel::GhStack),
            Some("git") => Ok(StackModel::Git),
            Some(other) if matches!(other, "branchless" | "sapling" | "spr" | "ghstack") => {
                Err(StackError::UnsupportedModel {
                    model: other.to_string(),
                }
                .into())
            }
            Some(other) => Err(StackError::UnknownModel {
                value: other.to_string(),
            }
            .into()),
        }
    }

    /// Get the worktree granularity for stacked diff workflows.
    ///
    /// Precedence: CLI override > workon.stackWorktreeGranularity config > `Stack`.
    ///
    /// Only `"stack"` is implemented in v1. `"diff"` (one worktree per branch) is planned.
    pub fn stack_worktree_granularity(&self, cli_override: Option<&str>) -> Result<Granularity> {
        let raw = if let Some(val) = cli_override {
            Some(val.to_string())
        } else {
            let config = self.repo.config()?;
            config.get_string("workon.stackWorktreeGranularity").ok()
        };

        match raw.as_deref() {
            None | Some("stack") => Ok(Granularity::Stack),
            Some("diff") => Err(StackError::UnsupportedGranularity.into()),
            Some(other) => Err(StackError::UnknownGranularity {
                value: other.to_string(),
            }
            .into()),
        }
    }

    /// Get whether to automatically register new branches with the active stack tool
    /// (Graphite's `gt track`, gh-stack's canonical-file append) after `workon new`.
    ///
    /// Precedence: CLI override > `workon.stackAutoTrack` > `workon.gtAutoTrack` (deprecated,
    /// read only when `stackAutoTrack` is unset) > `true`.
    ///
    /// Failures in the registration itself are non-fatal warnings, not errors — see
    /// `workon new`'s hook.
    pub fn stack_auto_track(&self, cli_override: Option<bool>) -> Result<bool> {
        if let Some(val) = cli_override {
            return Ok(val);
        }
        let config = self.repo.config()?;
        if let Ok(val) = config.get_bool("workon.stackAutoTrack") {
            return Ok(val);
        }
        match config.get_bool("workon.gtAutoTrack") {
            Ok(val) => Ok(val),
            Err(_) => Ok(true),
        }
    }

    /// Deprecated alias for [`stack_auto_track`](Self::stack_auto_track), kept for one release
    /// so existing callers of `workon.gtAutoTrack` don't break. New code should call
    /// `stack_auto_track` directly; this wrapper carries the same precedence.
    pub fn gt_auto_track(&self, cli_override: Option<bool>) -> Result<bool> {
        self.stack_auto_track(cli_override)
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
                if let Ok(value) = entry.value() {
                    values.push(value.to_string());
                }
            }
        }

        Ok(values)
    }
}
