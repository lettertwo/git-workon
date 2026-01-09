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

    /// Repository-related errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Repo(#[from] RepoError),

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

    /// Pull request-related errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Pr(#[from] PrError),
}

/// Repository-specific errors
#[derive(Error, Diagnostic, Debug)]
pub enum RepoError {
    #[error("Not a bare repository at {0}")]
    #[diagnostic(
        code(workon::repo::not_bare),
        help("Workon commands must be run in bare repositories")
    )]
    NotBare(String),
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

    #[error("Not in a worktree directory")]
    #[diagnostic(
        code(workon::worktree::not_in_worktree),
        help("Run this command from within a worktree directory")
    )]
    NotInWorktree,

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

    #[error("Worktree '{to}' already exists")]
    #[diagnostic(
        code(workon::worktree::target_exists),
        help("Choose a different name or remove the existing worktree first")
    )]
    TargetExists { to: String },

    #[error("Cannot move detached HEAD worktree")]
    #[diagnostic(
        code(workon::worktree::move_detached),
        help("Detached HEAD worktrees have no branch to rename")
    )]
    CannotMoveDetached,

    #[error("Branch '{0}' is protected and cannot be renamed")]
    #[diagnostic(
        code(workon::worktree::protected_branch_move),
        help("Protected branches are configured in workon.pruneProtectedBranches. Use --force to override.")
    )]
    ProtectedBranchMove(String),

    #[error("Worktree is dirty (uncommitted changes)")]
    #[diagnostic(
        code(workon::worktree::dirty_worktree),
        help("Commit or stash changes, or use --force to override")
    )]
    DirtyWorktree,

    #[error("Worktree has unpushed commits")]
    #[diagnostic(
        code(workon::worktree::unpushed_commits),
        help("Push commits first, or use --force to override")
    )]
    UnpushedCommits,
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

/// Pull request-related errors
#[derive(Error, Diagnostic, Debug)]
pub enum PrError {
    #[error("Invalid PR reference: {input}")]
    #[diagnostic(
        code(workon::pr::invalid_reference),
        help("Use formats like #123, pr-123, or https://github.com/owner/repo/pull/123")
    )]
    InvalidReference { input: String },

    #[error("PR #{number} not found on remote {remote}")]
    #[diagnostic(
        code(workon::pr::not_found),
        help("Verify the PR number exists and you have access to the repository")
    )]
    PrNotFound { number: u32, remote: String },

    #[error("No git remote configured")]
    #[diagnostic(
        code(workon::pr::no_remote),
        help("Add a remote with: git remote add origin <url>")
    )]
    NoRemoteConfigured,

    #[error("Failed to fetch PR refs from {remote}: {message}")]
    #[diagnostic(
        code(workon::pr::fetch_failed),
        help("Check your network connection and repository access")
    )]
    FetchFailed { remote: String, message: String },
}
