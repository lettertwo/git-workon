use std::path::PathBuf;

use anyhow::Result;
use log::debug;
use workon::clone;

use crate::cli::Clone;

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));
        clone(path, &self.url)?;
        debug!("Done");
        Ok(())
    }
}
