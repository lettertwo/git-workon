use anyhow::{bail, Result};

use crate::cli::CopyUntracked;

use super::Run;

impl Run for CopyUntracked {
    fn run(&self) -> Result<()> {
        bail!(
            "copyuntracked from={} to={} not implemented!",
            self.from,
            self.to
        );
    }
}
