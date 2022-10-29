use anyhow::{bail, Result};

use crate::cli::Switch;

use super::Run;

impl Run for Switch {
    fn run(&self) -> Result<()> {
        bail!("worktree name={:?} not implemented!", self.name);
    }
}
