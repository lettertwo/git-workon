use std::path::PathBuf;

use git2::Repository;
use log::debug;

use crate::error::Result;
use crate::{convert_to_bare, empty_commit};

/// Initialise a new bare repository at `path` in the worktrees layout.
///
/// Creates a bare repository, adds an initial empty commit so that HEAD is
/// valid, then converts to the worktrees layout (see [`convert_to_bare`]).
pub fn init(path: PathBuf) -> Result<Repository> {
    debug!("initializing bare repository at {}", path.display());

    let repo = Repository::init(&path)?;

    // 2. Add an initial (empty) commit. We need this to create a valid HEAD.
    empty_commit(&repo)?;

    // 3. git config core.bare true
    convert_to_bare(repo)
}
