use miette::{bail, Result};

use crate::cli::Switch;

use super::Run;

impl Run for Switch {
    fn run(&self) -> Result<()> {
        unimplemented!("switch {:?} not implemented!", self.name);
    }
}
