use git2::Repository;
use miette::{IntoDiagnostic, Result};

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
        let config = self.repo.config().into_diagnostic()?;
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
            let config = self.repo.config().into_diagnostic()?;
            config
                .get_string("workon.prFormat")
                .unwrap_or_else(|_| "pr-{number}".to_string())
        };

        // Validate format contains {number} placeholder
        if !format.contains("{number}") {
            return Err(miette::miette!(
                "workon.prFormat must contain {{number}} placeholder, got: {}",
                format
            ));
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

    /// Get the list of branch patterns to protect from pruning.
    ///
    /// Reads from multi-value workon.pruneProtectedBranches config.
    /// Patterns support simple glob matching (* and ?).
    /// Returns empty Vec if not configured.
    pub fn prune_protected_branches(&self) -> Result<Vec<String>> {
        self.read_multivar("workon.pruneProtectedBranches")
    }

    /// Helper to read multi-value config entries.
    ///
    /// Returns an empty Vec if the key doesn't exist.
    fn read_multivar(&self, key: &str) -> Result<Vec<String>> {
        let config = self.repo.config().into_diagnostic()?;
        let mut values = Vec::new();

        // Key doesn't exist, return empty vec
        if let Ok(mut entries) = config.multivar(key, None) {
            while let Some(entry) = entries.next() {
                let entry = entry.into_diagnostic()?;
                if let Some(value) = entry.value() {
                    values.push(value.to_string());
                }
            }
        }

        Ok(values)
    }
}
