use auth_git2::GitAuthenticator;
use git2::{Config, ConfigLevel, RemoteCallbacks, Repository};

use crate::error::Result;
use crate::ssh_config::apply_identity_agent;

/// Holds the credential authenticator and git config needed to build
/// [`git2::RemoteCallbacks`].
///
/// auth-git2's credential closure borrows both the authenticator and the config,
/// so this struct keeps them alive while the callbacks are in use. Call
/// [`RemoteAuth::callbacks`] to obtain borrowing callbacks tied to this holder's
/// lifetime.
pub struct RemoteAuth {
    authenticator: GitAuthenticator,
    config: Config,
}

impl RemoteAuth {
    /// Build credential callbacks that borrow this holder.
    pub fn callbacks(&self) -> RemoteCallbacks<'_> {
        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(self.authenticator.credentials(&self.config));
        callbacks
    }
}

/// Build a [`RemoteAuth`] using the given repo's config to drive credential
/// resolution. Mirrors `git fetch`/`git push` precedence:
/// local `.git/config` > worktree config > global > XDG > system.
pub fn get_remote_callbacks(repo: &Repository, url: Option<&str>) -> Result<RemoteAuth> {
    build_auth(repo.config()?, url)
}

/// Build a [`RemoteAuth`] for operations that run before a repo exists (e.g.
/// clone). Mirrors `git clone` precedence (global + XDG + system), tolerating
/// a missing `~/.gitconfig`.
pub fn get_remote_callbacks_default(url: Option<&str>) -> Result<RemoteAuth> {
    build_auth(open_default_config_lenient()?, url)
}

fn build_auth(config: Config, url: Option<&str>) -> Result<RemoteAuth> {
    if let Some(url) = url {
        apply_identity_agent(url);
    }
    Ok(RemoteAuth {
        authenticator: GitAuthenticator::default(),
        config,
    })
}

/// Like `Config::open_default()`, but tolerates a missing `~/.gitconfig`.
///
/// libgit2's `git_config_open_default` unconditionally stats `$HOME/.gitconfig`
/// and errors on ENOENT, even when XDG (`~/.config/git/config`) or system
/// configs exist. Probe each level individually so users without a global config
/// still pick up credential helpers configured elsewhere.
fn open_default_config_lenient() -> std::result::Result<Config, git2::Error> {
    if let Ok(c) = Config::open_default() {
        return Ok(c);
    }
    let mut config = Config::new()?;
    if let Ok(path) = Config::find_global() {
        let _ = config.add_file(&path, ConfigLevel::Global, false);
    }
    if let Ok(path) = Config::find_xdg() {
        let _ = config.add_file(&path, ConfigLevel::XDG, false);
    }
    if let Ok(path) = Config::find_system() {
        let _ = config.add_file(&path, ConfigLevel::System, false);
    }
    Ok(config)
}
