use std::path::PathBuf;

use log::debug;
use miette::Result;
use workon::clone;

use crate::cli::Clone;

use super::Run;

impl Run for Clone {
    fn run(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| {
            PathBuf::from(
                self.url
                    .trim_end_matches('/')
                    .split('/')
                    .last()
                    .unwrap_or(".")
                    .trim_end_matches(".git"),
            )
        });
        clone(path, &self.url)?;
        debug!("Done");
        Ok(())
    }
}
