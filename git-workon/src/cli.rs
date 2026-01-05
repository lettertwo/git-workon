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
    CopyUntracked(CopyUntracked),
    Find(Find),
    Init(Init),
    List(List),
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
    #[arg(long, help = "Use configured patterns from workon.copyPattern")]
    pub auto: bool,
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
pub struct List {}

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
}

/// Prune stale worktrees.
#[derive(Debug, Args)]
pub struct Prune {
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
