use std::{
    fs::{rename, write},
    path::PathBuf,
};

use anyhow::{Result, Ok};
use git2::{Repository, WorktreeAddOptions};

use crate::cli::Init;

use super::Run;

impl Run for Init {
    fn run(&self) -> Result<()> {
        // TODO: Figure out how to make this atomic, i.e., undo anything that was done on failure.

        // 1. git init
        let path = self.name.clone().unwrap_or_else(|| PathBuf::from("."));
        let repo = Repository::init(&path)?;

        // 2. Add an initial (empty) commit. We need this to create a valid HEAD.
        {
            let sig = repo.signature()?;
            let tree = repo.find_tree({
                let mut index = repo.index()?;
                index.write_tree()?
            })?;
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])?;
        }

        // 3. git config core.bare true
        let mut config = repo.config()?;
        config.set_bool("core.bare", true)?;

        // 4. mv .git .bare
        rename(repo.path(), path.join(".bare"))?;

        // 5. create a git-link file: `echo "gitdir: ./.bare" > .git`
        write(path.join(".git"), "gitdir: ./.bare")?;

        // 6. git worktree add `defaultbranch`
        let defaultbranch = config.get_str("init.defaultbranch").unwrap_or("main");
        let repo = Repository::open(path.join(".bare"))?;
        let reference = repo
            .find_branch(&defaultbranch, git2::BranchType::Local)?
            .into_reference();
        let mut opts = WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree(
            &defaultbranch,
            path.join(&defaultbranch).as_path(),
            Some(&opts),
        )?;

        Ok(())
    }
}
