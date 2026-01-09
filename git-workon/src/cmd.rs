mod clone;
mod copy_untracked;
mod find;
mod init;
mod list;
mod r#move; // r#move because "move" is a reserved keyword
mod new;
mod prune;

use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::Cmd;

pub trait Run {
    fn run(&self) -> Result<Option<WorktreeDescriptor>>;
}

impl Run for Cmd {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        match self {
            Cmd::Clone(cmd) => cmd.run(),
            Cmd::CopyUntracked(cmd) => cmd.run(),
            Cmd::Find(cmd) => cmd.run(),
            Cmd::Init(cmd) => cmd.run(),
            Cmd::List(cmd) => cmd.run(),
            Cmd::Move(cmd) => cmd.run(),
            Cmd::New(cmd) => cmd.run(),
            Cmd::Prune(cmd) => cmd.run(),
        }
    }
}
