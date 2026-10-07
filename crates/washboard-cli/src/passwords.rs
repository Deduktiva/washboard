//! Basic-auth passwords.
//!
//! On macOS they live in the Keychain, shared with the app (`KeychainSecretStore`). Elsewhere
//! there is no persistent store: the password comes from `WASHBOARD_PASSWORD` or a no-echo
//! prompt each time a request is sent, and `server add/edit --password-stdin` is refused.

use std::io::{self, BufRead, IsTerminal};

use anyhow::{Context, bail};
use washboard_core::model::Server;
use washboard_core::project::Project;
use washboard_core::secrets::SecretStore;

/// Environment variable consulted at send time when no stored password exists.
pub const PASSWORD_ENV: &str = "WASHBOARD_PASSWORD";

/// The store passed to core calls that take one (server update/delete).
#[cfg(target_os = "macos")]
pub fn store() -> Box<dyn SecretStore> {
    Box::new(washboard_core::secrets::KeychainSecretStore)
}

/// No persistent store on this platform; an empty in-memory store keeps the core calls
/// uniform (deleting from it is a no-op).
#[cfg(not(target_os = "macos"))]
pub fn store() -> Box<dyn SecretStore> {
    Box::new(washboard_core::secrets::MemorySecretStore::default())
}

/// Whether passwords given to `server add/edit` can be kept.
pub const CAN_STORE: bool = cfg!(target_os = "macos");

/// Reads the first line of stdin, without its line ending.
pub fn read_stdin() -> anyhow::Result<String> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("cannot read the password from stdin")?;
    let trimmed = line.trim_end_matches(['\r', '\n']);
    Ok(trimmed.to_owned())
}

fn prompt(server: &Server, username: &str) -> anyhow::Result<String> {
    if !io::stdin().is_terminal() {
        bail!(
            "server {:?} needs a password for {username:?}: set {PASSWORD_ENV} or run \
             interactively",
            server.name
        );
    }
    rpassword::prompt_password(format!("Password for {username} at {}: ", server.name))
        .context("cannot read the password")
}

/// The password for a server set at add/edit time: from stdin with `--password-stdin`, else a
/// prompt on a terminal (empty input stores nothing). Only called where [`CAN_STORE`].
pub fn obtain_for_storage(
    server: &Server,
    username: &str,
    from_stdin: bool,
) -> anyhow::Result<Option<String>> {
    let p = if from_stdin {
        read_stdin()?
    } else if io::stdin().is_terminal() {
        prompt(server, username)?
    } else {
        return Ok(None);
    };
    Ok((!p.is_empty()).then_some(p))
}

/// The password to send with a basic-auth request: the stored one, then
/// [`PASSWORD_ENV`], then a prompt.
pub fn for_send(project: &Project, server: &Server, username: &str) -> anyhow::Result<String> {
    if CAN_STORE && let Some(p) = project.server_password(server.id, store().as_ref())? {
        return Ok(p);
    }
    if let Ok(p) = std::env::var(PASSWORD_ENV) {
        return Ok(p);
    }
    prompt(server, username)
}
