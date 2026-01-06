use git2::{Config, RemoteCallbacks};
use git2_credentials::CredentialHandler;

use crate::error::Result;

pub fn get_remote_callbacks<'a>() -> Result<RemoteCallbacks<'a>> {
    let mut callbacks = RemoteCallbacks::new();
    let git_config = Config::open_default()?;
    let mut credential_handler = CredentialHandler::new(git_config);

    callbacks.credentials(move |url, username, allowed| {
        credential_handler.try_next_credential(url, username, allowed)
    });
    Ok(callbacks)
}
