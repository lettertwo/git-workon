use git2::{Config, ConfigLevel, RemoteCallbacks, Repository};
use git2_credentials::CredentialHandler;

use crate::error::Result;
use crate::ssh_config::apply_identity_agent;

/// Build [`git2::RemoteCallbacks`] using the given repo's config to drive
/// credential resolution. Mirrors `git fetch`/`git push` precedence:
/// local `.git/config` > worktree config > global > XDG > system.
pub fn get_remote_callbacks<'a>(
    repo: &Repository,
    url: Option<&str>,
) -> Result<RemoteCallbacks<'a>> {
    build_callbacks(repo.config()?, url)
}

/// Build [`git2::RemoteCallbacks`] for operations that run before a repo
/// exists (e.g. clone). Mirrors `git clone` precedence (global + XDG + system),
/// tolerating a missing `~/.gitconfig`.
pub fn get_remote_callbacks_default<'a>(url: Option<&str>) -> Result<RemoteCallbacks<'a>> {
    build_callbacks(open_default_config_lenient()?, url)
}

fn build_callbacks<'a>(config: Config, url: Option<&str>) -> Result<RemoteCallbacks<'a>> {
    if let Some(url) = url {
        apply_identity_agent(url);
    }
    let mut callbacks = RemoteCallbacks::new();
    let mut credential_handler = CredentialHandler::new(config);
    callbacks.credentials(move |url, username, allowed| {
        credential_handler.try_next_credential(url, username, allowed)
    });
    Ok(callbacks)
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
