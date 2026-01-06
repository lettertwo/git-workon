use std::path::PathBuf;

use git2::Repository;
use log::debug;

use crate::error::Result;
use crate::{convert_to_bare, empty_commit};

pub fn init(path: PathBuf) -> Result<Repository> {
    debug!("initializing bare repository at {}", path.display());

    let repo = Repository::init(&path)?;

    // 2. Add an initial (empty) commit. We need this to create a valid HEAD.
    empty_commit(&repo)?;

    // 3. git config core.bare true
    convert_to_bare(repo)
}
