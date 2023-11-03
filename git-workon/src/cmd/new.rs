use miette::{bail, Result};

use crate::cli::New;

use super::Run;

impl Run for New {
    fn run(&self) -> Result<()> {
        bail!("new name={:?} not implemented!", self.name);
    }
}
