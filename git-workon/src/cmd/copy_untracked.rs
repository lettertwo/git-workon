use miette::{bail, Result};

use crate::cli::CopyUntracked;

use super::Run;

impl Run for CopyUntracked {
    fn run(&self) -> Result<()> {
        unimplemented!(
            "copyuntracked from={} to={} not implemented!",
            self.from,
            self.to
        );
    }
}
