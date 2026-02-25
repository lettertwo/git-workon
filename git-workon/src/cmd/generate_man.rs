use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::GenerateMan;

use super::Run;

impl Run for GenerateMan {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        print!(
            "{}",
            include_str!(concat!(env!("OUT_DIR"), "/git-workon.1"))
        );
        Ok(None)
    }
}
