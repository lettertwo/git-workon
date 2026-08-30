use std::path::PathBuf;

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

    /// File copy errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Copy(#[from] CopyError),

    /// Stacked diff workflow errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Stack(#[from] StackError),

    /// In-place checkout errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Checkout(#[from] CheckoutError),

    /// Prune-related errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Prune(#[from] PruneError),

    /// Changeset assembly errors
    #[error(transparent)]
    #[diagnostic(forward(0))]
    Changeset(#[from] ChangesetError),
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

    #[error("Worktree admin name '{name}' is already in use by {path}")]
    #[diagnostic(
        code(workon::worktree::name_conflict),
        help("Remove or rename the existing worktree, or run 'git workon doctor' to find a stale admin name")
    )]
    WorktreeNameConflict { name: String, path: String },
}

/// Configuration-related errors
#[derive(Error, Diagnostic, Debug)]
pub enum ConfigError {
    #[error("Invalid PR format: '{format}' - {reason}")]
    #[diagnostic(
        code(workon::config::invalid_pr_format),
        help("Valid placeholders: {{number}}, {{title}}, {{author}}, {{branch}}")
    )]
    InvalidPrFormat { format: String, reason: String },

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

/// Stacked diff workflow errors
#[derive(Error, Diagnostic, Debug)]
pub enum StackError {
    #[error("Stack model '{model}' is not yet supported")]
    #[diagnostic(
        code(workon::stack::unsupported_model),
        help(
            "'graphite' and 'gh-stack' are implemented in this version. \
             Support for branchless and sapling is planned. 'ghstack' (Meta's \
             Phabricator-style stacker) is a different tool and is not planned."
        )
    )]
    UnsupportedModel { model: String },

    #[error("Unknown stack model '{value}'")]
    #[diagnostic(
        code(workon::stack::unknown_model),
        help("Valid values: graphite, gh-stack, git, none, auto")
    )]
    UnknownModel { value: String },

    #[error("Worktree granularity 'diff' is not yet implemented")]
    #[diagnostic(
        code(workon::stack::unsupported_granularity),
        help(
            "Only 'stack' (one worktree per stack) is supported in this version. \
             'diff' (one worktree per branch) is planned."
        )
    )]
    UnsupportedGranularity,

    #[error("Unknown worktree granularity '{value}'")]
    #[diagnostic(code(workon::stack::unknown_granularity), help("Valid values: stack"))]
    UnknownGranularity { value: String },

    #[error("Graphite CLI ('gt') is not installed or not in PATH")]
    #[diagnostic(
        code(workon::stack::gt_not_installed),
        help(
            "Install Graphite: https://graphite.dev/cli \
             Or set workon.stackModel = none to disable stack support."
        )
    )]
    GtNotInstalled,

    #[error("Graphite command failed: {stderr}")]
    #[diagnostic(code(workon::stack::gt_command_failed))]
    GtCommandFailed { stderr: String },

    #[error("Failed to parse Graphite metadata: {message}")]
    #[diagnostic(code(workon::stack::gt_parse_failed))]
    GtParseFailed { message: String },

    #[error("Repository is not Graphite-managed (no .graphite_repo_config)")]
    #[diagnostic(
        code(workon::stack::not_a_graphite_repo),
        help("Run 'gt init' in this repository, or unset workon.stackModel.")
    )]
    NotAGraphiteRepo,

    #[error("Branch '{branch}' exists in stack metadata but its local ref was deleted")]
    #[diagnostic(
        code(workon::stack::deleted_branch_node),
        help(
            "The branch was tracked by Graphite but its local ref no longer exists. \
             Run 'gt branch checkout {branch}' to restore it, or \
             'gt branch delete {branch}' to remove it from the stack."
        )
    )]
    DeletedBranchNode { branch: String },

    #[error("gh-stack file '{}' uses schema version {version}, which this version of git-workon does not support", path.display())]
    #[diagnostic(
        code(workon::stack::gh_stack_schema_unsupported),
        help("Upgrade git-workon, or downgrade the gh-stack extension that wrote this file.")
    )]
    GhStackSchemaUnsupported { path: PathBuf, version: u64 },

    #[error("Failed to parse gh-stack file '{}': {message}", path.display())]
    #[diagnostic(code(workon::stack::gh_stack_parse_failed))]
    GhStackParseFailed { path: PathBuf, message: String },

    #[error("Timed out waiting for gh-stack lock '{}'", path.display())]
    #[diagnostic(
        code(workon::stack::gh_stack_locked),
        help("Another `gh stack` or `git-workon` process may be mid-write; try again shortly.")
    )]
    GhStackLocked { path: PathBuf },

    #[error("Failed to write gh-stack file '{}': {message}", path.display())]
    #[diagnostic(code(workon::stack::gh_stack_write_failed))]
    GhStackWriteFailed { path: PathBuf, message: String },

    #[error("Failed to link gh-stack file '{}': {message}", path.display())]
    #[diagnostic(code(workon::stack::gh_stack_link_failed))]
    GhStackLinkFailed { path: PathBuf, message: String },

    #[error("Branch '{branch}' exists in stack metadata but its local ref was deleted")]
    #[diagnostic(
        code(workon::stack::deleted_stack_node),
        help(
            "The branch was tracked by your stack tool but its local ref no longer exists. \
             Recreate the branch, or remove it from the stack metadata."
        )
    )]
    DeletedStackNode { branch: String },
}

/// Changeset assembly errors
#[derive(Error, Diagnostic, Debug)]
pub enum ChangesetError {
    #[error("Branch '{branch}' has no resolvable local ref")]
    #[diagnostic(
        code(workon::changeset::unresolvable_branch),
        help("The branch may have been deleted while stack metadata lingered; sync your stack tool (e.g. `gt sync`) or re-create the branch")
    )]
    UnresolvableBranch { branch: String },

    #[error(
        "Recorded parent revision '{revision}' for branch '{branch}' does not resolve to a commit"
    )]
    #[diagnostic(
        code(workon::changeset::invalid_parent_revision),
        help("Stack metadata may be corrupt or copied from another clone; resync your stack tool (e.g. `gt restack`) to rewrite it")
    )]
    InvalidParentRevision { branch: String, revision: String },

    #[error("Branch '{branch}' has no upstream to infer changesets from")]
    #[diagnostic(
        code(workon::changeset::no_upstream),
        help(
            "Set an upstream (git branch --set-upstream-to=<remote>/<branch>) or use a stack tool"
        )
    )]
    NoUpstream { branch: String },
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

    #[error("gh CLI is not installed or not in PATH")]
    #[diagnostic(
        code(workon::pr::gh_not_installed),
        help("Install gh CLI: https://cli.github.com/")
    )]
    GhNotInstalled,

    #[error("Failed to fetch PR metadata from gh: {message}")]
    #[diagnostic(
        code(workon::pr::gh_fetch_failed),
        help("Check your network connection and GitHub authentication (gh auth status)")
    )]
    GhFetchFailed { message: String },

    #[error("Invalid JSON output from gh CLI: {message}")]
    #[diagnostic(
        code(workon::pr::gh_json_parse_failed),
        help("This may indicate a gh CLI version incompatibility")
    )]
    GhJsonParseFailed { message: String },

    #[error("Fork repository missing owner information")]
    #[diagnostic(
        code(workon::pr::missing_fork_owner),
        help("This PR may be from a deleted fork")
    )]
    MissingForkOwner,
}

/// In-place checkout errors
#[derive(Error, Diagnostic, Debug)]
pub enum CheckoutError {
    /// A git2 error during checkout
    #[error(transparent)]
    #[diagnostic(code(workon::checkout::git_error))]
    Git(#[from] git2::Error),

    /// Branch not found in the host worktree
    #[error("Branch '{branch}' not found in the worktree")]
    #[diagnostic(
        code(workon::checkout::branch_not_found),
        help("Ensure the branch exists locally before checking it out in place")
    )]
    BranchNotFound { branch: String },

    /// Checkout conflicts with uncommitted changes in the working tree
    #[error("Checkout of '{branch}' conflicts with uncommitted changes in {path}")]
    #[diagnostic(
        code(workon::checkout::conflict),
        help("Stash or commit changes first, or use the interactive prompt to shelve them")
    )]
    Conflict { branch: String, path: String },

    /// User aborted an interactive checkout
    #[error("Checkout aborted")]
    #[diagnostic(code(workon::checkout::aborted))]
    Aborted,
}

/// Prune-related errors
#[derive(Error, Diagnostic, Debug)]
pub enum PruneError {
    #[error("worktree(s) not found: {}", .names.join(", "))]
    #[diagnostic(
        code(workon::prune::names_not_found),
        help("Use 'git workon list' to see available worktrees")
    )]
    NamesNotFound { names: Vec<String> },
}

/// File copy errors
#[derive(Error, Diagnostic, Debug)]
pub enum CopyError {
    #[error("Invalid glob pattern '{pattern}'")]
    #[diagnostic(
        code(workon::copy::invalid_glob_pattern),
        help("Check glob pattern syntax: *, **, ?, [...]")
    )]
    InvalidGlobPattern {
        pattern: String,
        #[source]
        source: glob::PatternError,
    },

    #[error("Path is not valid UTF-8: {}", path.display())]
    #[diagnostic(code(workon::copy::invalid_path))]
    InvalidPath { path: PathBuf },

    #[error("Failed to read glob entry")]
    #[diagnostic(code(workon::copy::glob_error))]
    GlobEntry(#[from] glob::GlobError),

    #[error("Failed to copy '{}' to '{}'", src.display(), dest.display())]
    #[diagnostic(code(workon::copy::copy_failed))]
    CopyFailed {
        src: PathBuf,
        dest: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to open repository at '{}'", path.display())]
    #[diagnostic(
        code(workon::copy::repo_open_error),
        help("Ensure the path is a valid git repository")
    )]
    RepoOpen {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
}
