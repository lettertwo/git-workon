use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};

/// A git plugin for managing worktrees
#[derive(Debug, Parser)]
#[clap(
    about,
    long_about = "An opinionated git worktree workflow for managing multiple branches simultaneously.\n\ngit-workon clones repositories as bare repos with a worktrees-first layout, then provides commands for creating, finding, and cleaning up worktrees — so switching between branches is just `cd`, not `git stash && git checkout`.",
    author,
    bin_name = env!("CARGO_PKG_NAME"),
    propagate_version = true,
    version,
)]
pub struct Cli {
    #[clap(flatten)]
    pub verbose: Verbosity<InfoLevel>,
    #[arg(long, global = true, help = "Output results as JSON")]
    pub json: bool,
    #[arg(long, global = true, help = "Disable color output")]
    pub no_color: bool,
    #[arg(
        long,
        global = true,
        help = "Disable stack-aware behavior for this invocation"
    )]
    pub no_stack: bool,
    #[command(subcommand)]
    pub command: Option<Cmd>,
    #[clap(flatten)]
    pub find: Find,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Clone(Clone),
    Copy(Copy),
    /// Detect and repair workspace issues
    #[command(visible_alias = "check")]
    Doctor(Doctor),
    Find(Find),
    Init(Init),
    #[command(visible_alias = "ls")]
    List(List),
    #[command(visible_alias = "mv")]
    Move(Move),
    New(New),
    Prune(Prune),
    ShellInit(ShellInit),
    #[command(name = "_complete", hide = true)]
    Complete(Complete),
    /// Output the man page to stdout (hidden).
    #[command(name = "generate-man", hide = true)]
    GenerateMan(GenerateMan),
    /// Check out a branch inside an existing stack home worktree (hidden, resolver-produced).
    #[command(hide = true)]
    Checkout(Checkout),
}

/// Perform a bare clone of a repository and create an initial worktree.
#[derive(Debug, Args)]
pub struct Clone {
    pub url: String,
    pub path: Option<PathBuf>,
    #[arg(long, help = "Skip post-create hooks")]
    pub no_hooks: bool,
}

/// Copy local (git-unmanaged) files from one worktree to another.
///
/// Copies files that git doesn't track — ignored files (build artifacts, local config,
/// secrets) and untracked-but-not-staged files. Ignored files are included by default
/// since they are the primary use case; use --no-include-ignored to skip them.
///
/// If possible, copying will be done using `clonefile` (`man clonefile`),
/// which is a copy-on-write optimization over a potentially much slower copy operation.
#[derive(Debug, Args)]
pub struct Copy {
    pub from: String,
    /// Destination worktree name. Defaults to the current worktree when omitted.
    pub to: Option<String>,
    #[arg(short, long, help = "Override patterns for one-off copy")]
    pub pattern: Option<String>,
    #[arg(
        short = 'x',
        long,
        help = "Exclude files matching pattern (additive with config)"
    )]
    pub exclude: Vec<String>,
    #[arg(short, long, help = "Overwrite existing files in destination")]
    pub force: bool,
    #[arg(long, help = "Skip git-ignored files (e.g., .env.local, node_modules)")]
    pub no_include_ignored: bool,
}

/// Create a new bare repository and an initial worktree.
#[derive(Debug, Args)]
pub struct Init {
    pub path: Option<PathBuf>,
    #[arg(long, help = "Skip post-create hooks")]
    pub no_hooks: bool,
}

/// List worktrees.
#[derive(Debug, Args)]
pub struct List {
    #[clap(skip)]
    #[allow(dead_code)]
    pub json: bool,
    #[clap(skip)]
    #[allow(dead_code)]
    pub no_stack: bool,

    #[arg(long, help = "Show only worktrees with uncommitted changes")]
    pub dirty: bool,

    #[arg(long, help = "Show only worktrees without uncommitted changes")]
    pub clean: bool,

    #[arg(long, help = "Show only worktrees with unpushed commits")]
    pub ahead: bool,

    #[arg(long, help = "Show only worktrees behind their upstream")]
    pub behind: bool,

    #[arg(long, help = "Show only worktrees whose upstream branch is deleted")]
    pub gone: bool,
}

/// Rename a worktree and its branch atomically.
///
/// Usage:
///   git workon move <to>           # Rename current worktree
///   git workon move <from> <to>    # Rename specific worktree
#[derive(Debug, Args)]
pub struct Move {
    /// Worktree name(s): either [to] or [from] [to]
    #[arg(num_args = 1..=2, required = true)]
    pub names: Vec<String>,

    #[arg(short = 'n', long, help = "Preview changes without executing")]
    pub dry_run: bool,

    #[arg(
        short,
        long,
        help = "Override all safety checks (dirty, unpushed, protected)"
    )]
    pub force: bool,
}

/// Create a new worktree.
#[derive(Debug, Args)]
pub struct New {
    #[clap(skip)]
    #[allow(dead_code)]
    pub no_stack: bool,
    pub name: Option<String>,
    #[arg(short, long, help = "Base branch to branch from")]
    pub base: Option<String>,
    #[arg(
        short = 'B',
        long,
        help = "Branch to attach (uses positional name as worktree directory)"
    )]
    pub branch: Option<String>,
    #[arg(short, long, help = "Create an orphan branch with no parent commits")]
    pub orphan: bool,
    #[arg(short, long, help = "Detach HEAD in the new working tree")]
    pub detach: bool,
    #[arg(long, help = "Skip post-create hooks")]
    pub no_hooks: bool,
    #[arg(
        long = "copy",
        overrides_with = "no_copy",
        help = "Copy local files from base worktree using configured patterns"
    )]
    pub copy: bool,
    #[arg(
        long = "no-copy",
        overrides_with = "copy",
        help = "Do not copy local files (overrides config)"
    )]
    pub no_copy: bool,
    #[arg(long, help = "Skip git-ignored files when copying")]
    pub no_copy_ignored: bool,
    #[arg(long, help = "Disable interactive mode (for testing/scripting)")]
    pub no_interactive: bool,
    #[arg(long, help = "Lock the worktree after creation")]
    pub lock: bool,
}

impl New {
    /// Construct a minimal `New` for auto-attach or branch creation.
    ///
    /// Used by the resolver when routing a bare `workon <name>` to `Cmd::New`.
    /// All flags default to off; callers can override individual fields after
    /// construction (e.g. `new.no_stack = true`).
    #[allow(dead_code)] // used by the main binary; build.rs imports cli.rs for completions
    pub fn attach(name: impl Into<String>) -> Self {
        Self {
            no_stack: false,
            name: Some(name.into()),
            base: None,
            branch: None,
            orphan: false,
            detach: false,
            no_hooks: false,
            copy: false,
            no_copy: false,
            no_copy_ignored: false,
            no_interactive: false,
            lock: false,
        }
    }
}

/// In-place checkout — move HEAD within a stack home worktree to `branch`.
///
/// Hidden; produced by the resolver when `workon <T>` resolves to
/// [`Resolution::Checkout`]. Never typed directly by users.
#[derive(Debug, Clone, Args)]
#[command(hide = true)]
pub struct Checkout {
    /// The target branch to check out.
    pub branch: String,
    /// The host worktree name (not path) where the checkout will happen.
    pub host_worktree: String,
    /// Propagated from the global `--no-stack` flag; suppresses stack-aware
    /// restore-on-return behavior added in PR-3.
    #[clap(skip)]
    #[allow(dead_code)]
    pub no_stack: bool,
    /// Bail on conflict instead of prompting (propagated from `--json`; also
    /// accepted directly for scripting and testing).
    #[arg(long)]
    pub no_interactive: bool,
}

/// Prune stale worktrees.
#[derive(Debug, Args)]
pub struct Prune {
    #[clap(skip)]
    #[allow(dead_code)]
    pub json: bool,

    /// Specific worktree names to prune
    pub names: Vec<String>,
    #[arg(
        short = 'n',
        long,
        help = "Show what would be pruned without actually removing anything"
    )]
    pub dry_run: bool,
    #[arg(short, long, help = "Skip confirmation prompts")]
    pub yes: bool,
    #[arg(
        long,
        help = "Also prune worktrees where the remote tracking branch is gone"
    )]
    pub gone: bool,
    #[arg(
        long,
        value_name = "BRANCH",
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = false,
        help = "Also prune worktrees merged into BRANCH (or default branch)"
    )]
    pub merged: Option<String>,
    #[arg(
        long,
        help = "Allow pruning worktrees with uncommitted changes (dirty working tree)"
    )]
    pub allow_dirty: bool,
    #[arg(
        long,
        alias = "allow-unpushed",
        help = "Allow pruning worktrees with unmerged commits"
    )]
    pub allow_unmerged: bool,
    #[arg(
        short,
        long,
        help = "Override all safety checks (protection, default branch, dirty, unmerged, locked)"
    )]
    pub force: bool,
    #[arg(long, help = "Keep local branch refs when pruning worktrees")]
    pub keep_branch: bool,
    #[arg(long, help = "Include locked worktrees when pruning")]
    pub include_locked: bool,
}

/// Find a worktree to work on.
#[derive(Debug, Clone, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Find {
    #[clap(skip)]
    #[allow(dead_code)]
    pub no_stack: bool,
    /// A partial name of a worktree.
    pub name: Option<String>,

    #[arg(long, help = "Show only worktrees with uncommitted changes")]
    pub dirty: bool,

    #[arg(long, help = "Show only worktrees without uncommitted changes")]
    pub clean: bool,

    #[arg(long, help = "Show only worktrees with unpushed commits")]
    pub ahead: bool,

    #[arg(long, help = "Show only worktrees behind their upstream")]
    pub behind: bool,

    #[arg(long, help = "Show only worktrees whose upstream branch is deleted")]
    pub gone: bool,

    #[arg(
        short = 'n',
        long,
        help = "Force a fresh worktree instead of reusing a stack home"
    )]
    pub new: bool,

    #[arg(long, help = "Disable interactive mode (for testing/scripting)")]
    pub no_interactive: bool,
}

/// Detect and repair workspace issues.
#[derive(Debug, Args)]
pub struct Doctor {
    /// Automatically fix detected issues
    #[arg(long)]
    pub fix: bool,

    /// Preview fixes without applying them
    #[arg(long)]
    pub dry_run: bool,

    #[clap(skip)]
    #[allow(dead_code)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

/// Generate shell integration script (wrapper function + completions).
#[derive(Debug, Args)]
pub struct ShellInit {
    /// Shell to generate init script for (auto-detected from $SHELL if not specified)
    pub shell: Option<Shell>,
    /// Name for the wrapper function
    #[arg(long, default_value = "workon")]
    pub cmd: String,
}

/// Output the man page to stdout.
#[derive(Debug, Args)]
pub struct GenerateMan {}

/// List worktree names for shell completion (hidden).
#[derive(Debug, Args)]
pub struct Complete {
    /// 0-based index of the word being completed
    #[arg(long, default_value_t = 0)]
    pub index: usize,
    /// Command line words (after the wrapper command name)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert()
    }
}
