use std::path::PathBuf;

use log::debug;
use miette::Result;
use workon::init;

use crate::cli::Init;

use super::Run;

impl Run for Init {
    fn run(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));
        init(path)?;
        debug!("Done");
        Ok(())
    }
}
