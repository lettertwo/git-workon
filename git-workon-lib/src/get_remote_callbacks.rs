use git2::{Config, RemoteCallbacks};
use git2_credentials::CredentialHandler;

use crate::error::Result;
use crate::ssh_config::apply_identity_agent;

/// Build [`git2::RemoteCallbacks`] that handle credential prompts via the system
/// credential store (SSH keys, keychain, etc.).
///
/// Uses [`git2_credentials::CredentialHandler`] which respects `~/.gitconfig`
/// credential settings and SSH agent.
///
/// If `url` is provided, `~/.ssh/config` is consulted for an `IdentityAgent`
/// directive matching the URL's host. When found, `SSH_AUTH_SOCK` is updated
/// so that libgit2 connects to the configured agent (e.g. 1Password SSH agent).
pub fn get_remote_callbacks<'a>(url: Option<&str>) -> Result<RemoteCallbacks<'a>> {
    if let Some(url) = url {
        apply_identity_agent(url);
    }

    let mut callbacks = RemoteCallbacks::new();
    let git_config = Config::open_default()?;
    let mut credential_handler = CredentialHandler::new(git_config);

    callbacks.credentials(move |url, username, allowed| {
        credential_handler.try_next_credential(url, username, allowed)
    });
    Ok(callbacks)
}
