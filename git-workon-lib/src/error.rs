use miette::Diagnostic;
use thiserror::Error;

/// Result type alias using WorkonError
pub type Result<T> = std::result::Result<T, WorkonError>;

/// Main error type for the workon library
#[derive(Error, Diagnostic, Debug)]
pub enum WorkonError {
    /// Git operation failed
    #[error(transparent)]
    #[diagnostic(code(workon::git_error))]
    Git(#[from] git2::Error),

    /// I/O operation failed
    #[error(transparent)]
    #[diagnostic(code(workon::io_error))]
    Io(#[from] std::io::Error),

    /// Worktree-related errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Worktree(#[from] WorktreeError),

    /// Configuration-related errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Config(#[from] ConfigError),

    /// Default branch detection errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    DefaultBranch(#[from] DefaultBranchError),
}

/// Worktree-specific errors
#[derive(Error, Diagnostic, Debug)]
pub enum WorktreeError {
    #[error("Invalid .git file format")]
    #[diagnostic(
        code(workon::worktree::invalid_git_file),
        help("The .git file should contain 'gitdir: <path>' pointing to the git directory")
    )]
    InvalidGitFile,

    #[error("Could not find worktree '{0}'")]
    #[diagnostic(
        code(workon::worktree::not_found),
        help("Use 'git workon list' to see available worktrees")
    )]
    NotFound(String),

    #[error("Could not determine branch target")]
    #[diagnostic(
        code(workon::worktree::no_branch_target),
        help("The branch may be in an invalid state")
    )]
    NoBranchTarget,

    #[error("Could not get current branch target")]
    #[diagnostic(code(workon::worktree::no_current_branch_target))]
    NoCurrentBranchTarget,

    #[error("Could not get local branch target")]
    #[diagnostic(code(workon::worktree::no_local_branch_target))]
    NoLocalBranchTarget,

    #[error("Worktree path has no parent directory")]
    #[diagnostic(
        code(workon::worktree::no_parent),
        help("Cannot create parent directories for worktree path")
    )]
    NoParent,

    #[error("Invalid worktree name: contains invalid UTF-8")]
    #[diagnostic(
        code(workon::worktree::invalid_name),
        help("Worktree names must be valid UTF-8 strings")
    )]
    InvalidName,

    #[error("Expected an empty index!")]
    #[diagnostic(code(workon::worktree::non_empty_index))]
    NonEmptyIndex,
}

/// Configuration-related errors
#[derive(Error, Diagnostic, Debug)]
pub enum ConfigError {
    #[error("Invalid PR format: '{format}' - must contain {{number}} placeholder")]
    #[diagnostic(
        code(workon::config::invalid_pr_format),
        help("Use a format like 'pr-{{number}}' that includes the {{number}} placeholder")
    )]
    InvalidPrFormat { format: String },

    #[error("Config entry has no value")]
    #[diagnostic(code(workon::config::no_value))]
    NoValue,
}

/// Default branch detection errors
#[derive(Error, Diagnostic, Debug)]
pub enum DefaultBranchError {
    #[error("Could not determine default branch for remote {remote:?}")]
    #[diagnostic(
        code(workon::default_branch::no_remote_default),
        help("The remote may not have a default branch configured")
    )]
    NoRemoteDefault { remote: Option<String> },

    #[error("Remote is not connected")]
    #[diagnostic(
        code(workon::default_branch::not_connected),
        help("Failed to establish connection to remote repository")
    )]
    NotConnected,

    #[error("Could not determine default branch: neither 'main' nor 'master' exist, and init.defaultBranch is not configured")]
    #[diagnostic(
        code(workon::default_branch::no_default_branch),
        help("Set init.defaultBranch in your git config, or create a 'main' or 'master' branch")
    )]
    NoDefaultBranch,
}
