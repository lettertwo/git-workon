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
    pub command: Cmd,
    #[clap(flatten)]
    pub switch: Switch,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Clone(Clone),
    CopyUntracked(CopyUntracked),
    Init(Init),
    List(List),
    New(New),
    Prune(Prune),
    Switch(Switch),
}

/// Perform a bare clone of a repository and create an initial worktree.
#[derive(Debug, Args)]
pub struct Clone {
    pub url: String,
    pub path: Option<PathBuf>,
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
}

/// Create a new bare repository and an initial worktree.
#[derive(Debug, Args)]
pub struct Init {
    pub path: Option<PathBuf>,
}

/// List worktrees.
#[derive(Debug, Args)]
pub struct List {}

/// Create a new worktree.
#[derive(Debug, Args)]
pub struct New {
    pub name: Option<String>,
}

/// Prune stale worktrees.
#[derive(Debug, Args)]
pub struct Prune {}

/// Select a worktree to work on.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Switch {
    /// The name of the worktree to work on.
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
