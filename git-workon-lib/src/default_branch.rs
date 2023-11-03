use git2::{Direction, Remote, RemoteCallbacks, Repository};
use miette::{bail, ensure, IntoDiagnostic, Result};

use super::get_remote_callbacks;

pub struct DefaultBranch<'repo, 'cb> {
    repo: &'repo Repository,
    remote: Option<Remote<'repo>>,
    callbacks: Option<RemoteCallbacks<'cb>>,
}

impl<'repo, 'cb> DefaultBranch<'repo, 'cb> {
    pub fn new(repo: &'repo Repository) -> Self {
        Self {
            repo,
            remote: None,
            callbacks: None,
        }
    }

    pub fn remote(&mut self, remote: Remote<'repo>) -> &mut Self {
        self.remote = Some(remote);
        self
    }

    pub fn remote_callbacks(&mut self, cbs: RemoteCallbacks<'cb>) -> &mut Self {
        self.callbacks = Some(cbs);
        self
    }

    pub fn get_name(self) -> Result<String> {
        match self.remote {
            Some(mut remote) => {
                let mut cxn = remote
                    .connect_auth(Direction::Fetch, self.callbacks, None)
                    .into_diagnostic()?;
                ensure!(cxn.connected(), "Remote is not connected");

                match cxn.default_branch().into_diagnostic()?.as_str() {
                    Some(default_branch) => Ok(default_branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(default_branch)
                        .to_string()),
                    None => bail!(
                        "Could not determine default branch for remote {:?}",
                        cxn.remote().name()
                    ),
                }
            }
            None => {
                let config = self.repo.config().into_diagnostic()?;
                let defaultbranch = config.get_str("init.defaultbranch").unwrap_or("main");
                Ok(defaultbranch.to_string())
            }
        }
    }
}

pub fn get_default_branch_name(repo: &Repository, remote: Option<Remote>) -> Result<String> {
    let mut default_branch = DefaultBranch::new(repo);
    if let Some(remote) = remote {
        default_branch.remote(remote);
        default_branch.remote_callbacks(get_remote_callbacks().unwrap());
    }
    default_branch.get_name()
}
