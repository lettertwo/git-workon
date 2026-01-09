use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};

/// Testing
#[derive(Debug, Parser)]
#[clap(
    about,
    author,
    bin_name = env!("CARGO_PKG_NAME"),
    propagate_version = true,
    version,
)]
pub struct Cli {
    #[clap(flatten)]
    pub verbose: Verbosity<InfoLevel>,
    #[command(subcommand)]
    pub command: Option<Cmd>,
    #[clap(flatten)]
    pub find: Find,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Clone(Clone),
    /// Manage git-workon configuration interactively
    #[command(visible_alias = "cfg")]
    Config(Config),
    CopyUntracked(CopyUntracked),
    /// Detect and repair workspace issues
    #[command(visible_alias = "check")]
    Doctor(Doctor),
    Find(Find),
    Init(Init),
    List(List),
    Move(Move),
    New(New),
    Prune(Prune),
}

/// Perform a bare clone of a repository and create an initial worktree.
#[derive(Debug, Args)]
pub struct Clone {
    pub url: String,
    pub path: Option<PathBuf>,
    #[arg(long, help = "Skip post-create hooks")]
    pub no_hooks: bool,
}

/// Copy any untracked files in <from> to <to>.
///
/// Untracked files are files that are ignored by git, or files that are not in the git index.
///
/// This util is a useful complement to a git worktree workflow. Git worktrees provide
/// a mechanism for maintaining multiple branches of a repository simultaneously, without
/// having to switch between branches (and using stash to keep WIP stuff around, etc).
/// See `man git-worktree` for more information.
///
/// However, one drawback of this approach vs. the traditional branch workflow is that any untracked
/// artifacts in the working directory, such as installed node modules, build caches, etc.,
/// have to be recreated or otherwise manually copied over when creating a new worktree.
///
/// That's where `copyuntracked` comes in!
///
/// If possible, copying will be done using `clonefile` (`man clonefile`),
/// which is a copy-on-write optimization over a potentially much slower copy operation.
#[derive(Debug, Args)]
pub struct CopyUntracked {
    pub from: String,
    pub to: String,
    #[arg(short, long, help = "Override patterns for one-off copy")]
    pub pattern: Option<String>,
    #[arg(short, long, help = "Overwrite existing files in destination")]
    pub force: bool,
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
    pub name: Option<String>,
    #[arg(short, long, help = "Base branch to branch from")]
    pub base: Option<String>,
    #[arg(short, long, help = "Create an orphan branch with no parent commits")]
    pub orphan: bool,
    #[arg(short, long, help = "Detach HEAD in the new working tree")]
    pub detach: bool,
    #[arg(long, help = "Skip post-create hooks")]
    pub no_hooks: bool,
    #[arg(
        long = "copy-untracked",
        overrides_with = "no_copy_untracked",
        help = "Copy untracked files from base worktree using configured patterns"
    )]
    pub copy_untracked: bool,
    #[arg(
        long = "no-copy-untracked",
        overrides_with = "copy_untracked",
        help = "Do not copy untracked files (overrides config)"
    )]
    pub no_copy_untracked: bool,
    #[arg(long, help = "Disable interactive mode (for testing/scripting)")]
    pub no_interactive: bool,
}

/// Prune stale worktrees.
#[derive(Debug, Args)]
pub struct Prune {
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
    #[arg(long, help = "Allow pruning worktrees with unpushed commits")]
    pub allow_unpushed: bool,
}

/// Find a worktree to work on.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Find {
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
}

/// Manage git-workon configuration interactively.
#[derive(Debug, Args)]
pub struct Config {
    /// Scope for configuration (global or local)
    #[arg(long, value_enum, default_value = "local")]
    pub scope: ConfigScope,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ConfigScope {
    Global,
    Local,
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
