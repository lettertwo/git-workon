use anyhow::{bail, Ok, Result};
use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use log::{debug, info};

// use std::ffi::OsStr;
// use std::ffi::OsString;
// use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[clap(flatten)]
    verbose: Verbosity<InfoLevel>,
    #[command(subcommand)]
    command: Option<Commands>,
    #[clap(flatten)]
    worktree: Worktree,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct Worktree {
    /// The name of the worktree to work on.
    name: Option<String>,
}

// TODO: Default subcommand? https://github.com/clap-rs/clap/issues/975#issuecomment-1012701551
#[derive(Debug, Subcommand)]
enum Commands {
    /// Select a worktree to work on.
    Worktree(Worktree),

    /// List worktrees.
    List {},

    /// Create a new worktree.
    New {
        name: Option<String>,
    },

    /// Prune stale worktrees.
    Prune {},

    ///
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
    CopyUntracked {
        from: String,
        to: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .init();

    debug!("This is a debug message");

    match &cli.command {
        Some(Commands::List {}) => {
            info!("Listing worktrees");
        }
        Some(Commands::New { name }) => {
            info!("Creating new worktree: {:?}", name);
        }
        Some(Commands::Prune {}) => {
            info!("Pruning stale worktrees");
        }
        Some(Commands::Worktree(name)) => {
            info!("Worktree {:?}", name);
        }
        Some(Commands::CopyUntracked { from, to }) => {
            info!("CopyUntracked {:?} -> {:?}", from, to);
        }
        None => {
            bail!("No subcommand was used. Try --help for more information.")
        }
    }

    Ok(())
}
