use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::CopyUntracked;

use super::Run;

impl Run for CopyUntracked {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        unimplemented!(
            "copyuntracked from={} to={} not implemented!",
            self.from,
            self.to
        );
    }
}
