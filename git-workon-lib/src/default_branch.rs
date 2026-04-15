use git2::{Direction, Remote, RemoteCallbacks, Repository};
use log::debug;

use crate::error::{DefaultBranchError, Result};
use crate::get_remote_callbacks;

/// Builder for resolving the default branch name of a repository or remote.
pub struct DefaultBranch<'repo, 'cb> {
    repo: &'repo Repository,
    remote: Option<Remote<'repo>>,
    callbacks: Option<RemoteCallbacks<'cb>>,
}

impl<'repo, 'cb> DefaultBranch<'repo, 'cb> {
    /// Create a new builder for the given repository.
    pub fn new(repo: &'repo Repository) -> Self {
        Self {
            repo,
            remote: None,
            callbacks: None,
        }
    }

    /// Set the remote to query for its default branch.
    pub fn remote(&mut self, remote: Remote<'repo>) -> &mut Self {
        self.remote = Some(remote);
        self
    }

    /// Set credential callbacks for the remote connection.
    pub fn remote_callbacks(&mut self, cbs: RemoteCallbacks<'cb>) -> &mut Self {
        self.callbacks = Some(cbs);
        self
    }

    /// Resolve the default branch name.
    ///
    /// If a remote was set, queries the remote for its HEAD ref.
    /// Otherwise reads `init.defaultbranch` from git config (defaulting to `"main"`).
    pub fn get_name(self) -> Result<String> {
        match self.remote {
            Some(mut remote) => {
                let mut cxn = remote.connect_auth(Direction::Fetch, self.callbacks, None)?;

                if !cxn.connected() {
                    return Err(DefaultBranchError::NotConnected.into());
                }

                match cxn.default_branch()?.as_str() {
                    Some(default_branch) => Ok(default_branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(default_branch)
                        .to_string()),
                    None => Err(DefaultBranchError::NoRemoteDefault {
                        remote: cxn.remote().name().map(|s| s.to_string()),
                    }
                    .into()),
                }
            }
            None => {
                let config = self.repo.config()?;
                let defaultbranch = config.get_str("init.defaultbranch").unwrap_or("main");
                Ok(defaultbranch.to_string())
            }
        }
    }
}

/// Convenience wrapper around [`DefaultBranch`].
///
/// Queries the remote for its default branch if one is provided, otherwise
/// falls back to `init.defaultbranch` config (defaulting to `"main"`).
pub fn get_default_branch_name(repo: &Repository, remote: Option<Remote>) -> Result<String> {
    let mut default_branch = DefaultBranch::new(repo);
    if let Some(remote) = remote {
        let url = remote.url().map(str::to_string);
        default_branch.remote(remote);
        default_branch.remote_callbacks(get_remote_callbacks(url.as_deref()).unwrap());
    }
    default_branch.get_name().or_else(|_| {
        debug!("Failed to read default branch from remote, trying git config");
        let branch = repo
            .config()
            .ok()
            .and_then(|config| config.get_string("init.defaultbranch").ok())
            .unwrap_or_else(|| {
                debug!("No init.defaultbranch config, falling back to 'main'");
                "main".to_string()
            });
        Ok(branch)
    })
}

/// Get the default branch name for a repository, validated to exist.
///
/// This function:
/// 1. Checks the `init.defaultBranch` config
/// 2. Falls back to "main" if it exists
/// 3. Falls back to "master" if it exists
/// 4. Returns an error if none exist
pub fn get_default_branch(repo: &Repository) -> Result<String> {
    // Check init.defaultBranch config
    if let Ok(config) = repo.config() {
        if let Ok(default_branch) = config.get_string("init.defaultBranch") {
            // Verify the configured branch exists
            if repo
                .find_branch(&default_branch, git2::BranchType::Local)
                .is_ok()
            {
                return Ok(default_branch);
            }
        }
    }

    // Fall back to "main" if it exists
    if repo.find_branch("main", git2::BranchType::Local).is_ok() {
        return Ok("main".to_string());
    }

    // Fall back to "master" if it exists
    if repo.find_branch("master", git2::BranchType::Local).is_ok() {
        return Ok("master".to_string());
    }

    Err(DefaultBranchError::NoDefaultBranch.into())
}
