mod init;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use self::init::Init;

pub trait Run {
    fn run(&self) -> Result<()>;
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Clone(Clone),
    CopyUntracked(CopyUntracked),
    Init(Init),
    List(List),
    New(New),
    Prune(Prune),
    Worktree(Worktree),
}

impl Cmd {
    pub fn run(&self) -> Result<()> {
        match self {
            Cmd::Clone(cmd) => cmd.run(),
            Cmd::CopyUntracked(cmd) => cmd.run(),
            Cmd::Init(cmd) => cmd.run(),
            Cmd::List(cmd) => cmd.run(),
            Cmd::New(cmd) => cmd.run(),
            Cmd::Prune(cmd) => cmd.run(),
            Cmd::Worktree(cmd) => cmd.run(),
        }
    }
}

/// Perform a bare clone of a repository and create an initial worktree.
#[derive(Debug, Args)]
pub struct Clone {
    pub url: String,
}

impl Run for Clone {
    fn run(&self) -> Result<()> {
        // 1. git clone --bare --single-branch <atlassian-url>.git .bare
        // 2. $ echo "gitdir: ./.bare" > .git
        // 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
        // 4. $ git fetch
        // 5. $ git worktree add --track main origin/main
        bail!("Clone Not implemented");
    }
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

impl Run for CopyUntracked {
    fn run(&self) -> Result<()> {
        bail!(
            "copyuntracked from={} to={} not implemented!",
            self.from,
            self.to
        );
    }
}

/// List worktrees.
#[derive(Debug, Args)]
pub struct List {}

impl Run for List {
    fn run(&self) -> Result<()> {
        bail!("list not implemented!");
    }
}

/// Create a new worktree.
#[derive(Debug, Args)]
pub struct New {
    pub name: Option<String>,
}

impl Run for New {
    fn run(&self) -> Result<()> {
        bail!("new name={:?} not implemented!", self.name);
    }
}

/// Prune stale worktrees.
#[derive(Debug, Args)]
pub struct Prune {}

impl Run for Prune {
    fn run(&self) -> Result<()> {
        bail!("prune not implemented!");
    }
}

/// Select a worktree to work on.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Worktree {
    /// The name of the worktree to work on.
    pub name: Option<String>,
}

impl Run for Worktree {
    fn run(&self) -> Result<()> {
        bail!("worktree name={:?} not implemented!", self.name);
    }
}
