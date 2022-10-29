mod clone;
mod copy_untracked;
mod init;
mod list;
mod new;
mod prune;
mod switch;

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
