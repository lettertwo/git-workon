mod init;

use anyhow::{bail, Result};

use crate::cli::{Cmd, Clone, CopyUntracked, List, New, Prune, Switch};

pub trait Run {
    fn run(&self) -> Result<()>;
}

impl Run for Cmd {
    fn run(&self) -> Result<()> {
        match self {
            Cmd::Clone(cmd) => cmd.run(),
            Cmd::CopyUntracked(cmd) => cmd.run(),
            Cmd::Init(cmd) => cmd.run(),
            Cmd::List(cmd) => cmd.run(),
            Cmd::New(cmd) => cmd.run(),
            Cmd::Prune(cmd) => cmd.run(),
            Cmd::Switch(cmd) => cmd.run(),
        }
    }
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

impl Run for CopyUntracked {
    fn run(&self) -> Result<()> {
        bail!(
            "copyuntracked from={} to={} not implemented!",
            self.from,
            self.to
        );
    }
}

impl Run for List {
    fn run(&self) -> Result<()> {
        bail!("list not implemented!");
    }
}

impl Run for New {
    fn run(&self) -> Result<()> {
        bail!("new name={:?} not implemented!", self.name);
    }
}

impl Run for Prune {
    fn run(&self) -> Result<()> {
        bail!("prune not implemented!");
    }
}

impl Run for Switch {
    fn run(&self) -> Result<()> {
        bail!("worktree name={:?} not implemented!", self.name);
    }
}
