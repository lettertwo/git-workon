use std::path::PathBuf;

use git2::{build::RepoBuilder, FetchOptions, Repository};
use log::debug;

use crate::error::Result;
use crate::{convert_to_bare, get_default_branch_name, get_remote_callbacks_default};

/// Options for [`clone`].
pub struct CloneOptions {
    /// Called during the network transfer with `(received_objects, total_objects, received_bytes)`.
    pub on_transfer_progress: Box<dyn FnMut(usize, usize, usize)>,
}

impl Default for CloneOptions {
    fn default() -> Self {
        Self {
            on_transfer_progress: Box::new(|_, _, _| {}),
        }
    }
}

/// Clone a remote repository into the worktrees layout.
///
/// The repository is cloned as a bare repo at `<path>/.bare` and a `.git` link
/// file is written at `<path>/.git` so that standard git tooling continues to
/// work. The fetch refspec for `origin` is configured to fetch all branches.
///
/// If `path` already ends with `.bare` it is used as-is; otherwise `.bare` is
/// appended.
pub fn clone(path: PathBuf, url: &str, options: CloneOptions) -> Result<Repository> {
    let CloneOptions {
        mut on_transfer_progress,
    } = options;
    debug!("path {}", path.display());
    let path = if path.ends_with(".bare") {
        debug!("ended with .bare!");
        path
    } else {
        debug!("didn't end with .bare!");
        path.join(".bare")
    };

    debug!("final path {}", path.display());

    let auth = get_remote_callbacks_default(Some(url))?;
    let mut callbacks = auth.callbacks();
    callbacks.transfer_progress(move |progress| {
        on_transfer_progress(
            progress.received_objects(),
            progress.total_objects(),
            progress.received_bytes(),
        );
        true
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

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
    let repo = builder.clone(url, &path)?;
    // 2. $ echo "gitdir: ./.bare" > .git
    // 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
    convert_to_bare(repo)
}
