//! `server add|list|edit|remove`. Servers are addressed by name. The project refuses blank
//! and duplicate names and URLs sending would refuse; a project written before it did may
//! still hold duplicates, which `find_server` reports.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::bail;
use clap::Args;
use washboard_core::http::DEFAULT_TIMEOUT;
use washboard_core::model::{Auth, Server, ServerId};
use washboard_core::project::Project;

use crate::passwords::{self, CAN_STORE, PASSWORD_ENV};
use crate::support;

#[derive(Debug, Args)]
pub struct Options {
    /// Basic-auth user name.
    #[arg(long)]
    pub username: Option<String>,
    /// Read the basic-auth password from the first line of stdin (macOS only: stored in the
    /// Keychain).
    #[arg(long)]
    pub password_stdin: bool,
    /// Request timeout in seconds.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Args)]
// The derived group would be named `Edit`, clashing with the `server edit` variant's own group.
#[group(skip)]
pub struct Edit {
    #[arg(long = "name", value_name = "NEW_NAME")]
    pub new_name: Option<String>,
    #[arg(long)]
    pub url: Option<String>,
    #[command(flatten)]
    pub opts: Options,
    /// Switch to no authentication (removes the stored password).
    #[arg(long, conflicts_with_all = ["username", "password_stdin"])]
    pub no_auth: bool,
    #[arg(long, value_name = "BOOL")]
    pub ignore_tls_errors: Option<bool>,
}

pub fn add(
    dir: &Path,
    name: &str,
    url: &str,
    opts: &Options,
    ignore_tls_errors: bool,
) -> anyhow::Result<ExitCode> {
    check_password_flags(opts)?;
    let mut project = support::open_write(dir)?;
    let server = Server {
        id: ServerId::new(),
        name: name.trim().to_owned(),
        url: url.to_owned(),
        ignore_tls_errors,
        auth: match &opts.username {
            Some(u) => Auth::Basic {
                username: u.clone(),
            },
            None => Auth::None,
        },
        timeout: opts.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs),
    };
    // Before the password prompt, so a refused server does not ask for one.
    project.check_server(&server)?;
    let password = obtain_password(&server, opts)?;
    project.add_server(&server)?;
    store_password(&project, &server, password.as_deref())?;
    Ok(ExitCode::SUCCESS)
}

pub fn list(dir: &Path) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    for s in project.servers()? {
        let mut flags = Vec::new();
        if let Auth::Basic { username } = &s.auth {
            flags.push(format!("basic auth as {username}"));
        }
        if s.ignore_tls_errors {
            flags.push("TLS errors ignored".to_owned());
        }
        if s.timeout != DEFAULT_TIMEOUT {
            flags.push(format!("timeout {}s", s.timeout.as_secs()));
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!("  ({})", flags.join(", "))
        };
        println!("{:<20} {}{flags}", s.name, s.url);
    }
    Ok(ExitCode::SUCCESS)
}

pub fn edit(dir: &Path, name: &str, e: &Edit) -> anyhow::Result<ExitCode> {
    check_password_flags(&e.opts)?;
    let mut project = support::open_write(dir)?;
    let mut server = support::find_server(&project, name)?;
    if let Some(n) = &e.new_name {
        server.name = n.trim().to_owned();
    }
    if let Some(u) = &e.url {
        server.url = u.clone();
    }
    if let Some(t) = e.opts.timeout {
        server.timeout = Duration::from_secs(t);
    }
    if let Some(i) = e.ignore_tls_errors {
        server.ignore_tls_errors = i;
    }
    if e.no_auth {
        server.auth = Auth::None;
    } else if let Some(u) = &e.opts.username {
        server.auth = Auth::Basic {
            username: u.clone(),
        };
    }
    project.check_server(&server)?;
    let password = if e.opts.password_stdin || e.opts.username.is_some() {
        obtain_password(&server, &e.opts)?
    } else {
        None
    };
    let store = passwords::store();
    project.update_server(&server, store.as_ref())?;
    store_password(&project, &server, password.as_deref())?;
    Ok(ExitCode::SUCCESS)
}

pub fn remove(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let server = support::find_server(&project, name)?;
    let store = passwords::store();
    project.delete_server(server.id, store.as_ref())?;
    Ok(ExitCode::SUCCESS)
}

/// Refused before anything changes: there is nowhere to keep the password on this platform.
fn check_password_flags(opts: &Options) -> anyhow::Result<()> {
    if opts.password_stdin && !CAN_STORE {
        bail!(
            "passwords cannot be stored on this platform; set {PASSWORD_ENV} when sending, or \
             enter it at the prompt"
        );
    }
    Ok(())
}

/// Asks for the password of a basic-auth server before anything is written, so a failed read
/// leaves the project unchanged. `None`: nothing to store.
fn obtain_password(server: &Server, opts: &Options) -> anyhow::Result<Option<String>> {
    let Auth::Basic { username } = &server.auth else {
        if opts.password_stdin {
            bail!("--password-stdin needs basic auth (--username)");
        }
        return Ok(None);
    };
    if !CAN_STORE {
        eprintln!(
            "note: the password is not stored on this platform; set {PASSWORD_ENV} when \
             sending, or enter it at the prompt"
        );
        return Ok(None);
    }
    passwords::obtain_for_storage(server, username, opts.password_stdin)
}

fn store_password(
    project: &Project,
    server: &Server,
    password: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(p) = password {
        project.set_server_password(server.id, Some(p), passwords::store().as_ref())?;
    }
    Ok(())
}
