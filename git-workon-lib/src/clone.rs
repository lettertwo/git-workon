use std::path::PathBuf;

use git2::{build::RepoBuilder, FetchOptions, Repository};
use log::debug;
use miette::{IntoDiagnostic, Result};

use crate::{add_worktree, convert_to_bare, get_default_branch_name, get_remote_callbacks};

pub fn clone(path: PathBuf, url: &str) -> Result<Repository> {
    debug!("path {}", path.display());
    let path = if path.ends_with(".bare") {
        debug!("ended with .bare!");
        path
    } else {
        debug!("didn't end with .bare!");
        path.join(".bare")
    };

    debug!("final path {}", path.display());

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(get_remote_callbacks()?);

    let mut builder = RepoBuilder::new();
    builder.bare(true);
    builder.fetch_options(fetch_options);
    builder.remote_create(|repo, name, url| {
        debug!("Creating remote {} at {}", name, url);
        let remote = repo.remote(name, url)?;

        match get_default_branch_name(repo, Some(remote)) {
            Ok(default_branch) => {
                debug!("Default branch: {}", default_branch);
                repo.remote_add_fetch(
                    name,
                    format!(
                        "+refs/heads/{default_branch}:refs/remotes/origin/{default_branch}",
                        default_branch = default_branch
                    )
                    .as_str(),
                )?;
                repo.find_remote(name)
            }
            Err(_) => {
                debug!("No default branch found");
                repo.remote(name, url)
            }
        }
    });

    debug!("Cloning {} into {}", url, path.display());

    // 1. git clone --single-branch <url>.git <path>/.bare
    let repo = builder.clone(&url, &path).into_diagnostic()?;
    // 2. $ echo "gitdir: ./.bare" > .git
    // 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
    let repo = convert_to_bare(repo)?;
    // 4. $ git fetch
    // 5. $ git worktree add --track <branch> origin/<branch>
    let default_branch = get_default_branch_name(&repo, repo.find_remote("origin").ok())?;
    add_worktree(&repo, &default_branch)?;

    Ok(repo)
}
