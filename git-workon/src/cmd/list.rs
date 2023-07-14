use anyhow::{bail, Result};

use crate::cli::List;

use super::Run;

impl Run for List {
    fn run(&self) -> Result<()> {
        bail!("list not implemented!");
    }
}
