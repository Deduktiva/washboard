//! `server add|list|edit|remove`. Servers are addressed by name, so names must be unique
//! for the command-line tool (the app does not require it; duplicates are reported).

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::bail;
use washboard_core::http::DEFAULT_TIMEOUT;
use washboard_core::model::{Auth, Server, ServerId};
use washboard_core::project::Project;

use crate::passwords::{self, CAN_STORE, PASSWORD_ENV};
use crate::support;

#[derive(Debug)]
pub struct Options {
    pub username: Option<String>,
    pub password_stdin: bool,
    pub timeout: Option<u64>,
}

#[derive(Debug)]
pub struct Edit {
    pub new_name: Option<String>,
    pub url: Option<String>,
    pub opts: Options,
    pub no_auth: bool,
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
    check_name_free(&project, name, None)?;
    let server = Server {
        id: ServerId::new(),
        name: checked_name(name)?,
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
        check_name_free(&project, n, Some(server.id))?;
        server.name = checked_name(n)?;
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

fn checked_name(name: &str) -> anyhow::Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("server name may not be empty");
    }
    Ok(n.to_owned())
}

fn check_name_free(project: &Project, name: &str, except: Option<ServerId>) -> anyhow::Result<()> {
    let name = name.trim();
    if project
        .servers()?
        .iter()
        .any(|s| s.name == name && Some(s.id) != except)
    {
        bail!("a server named {name:?} already exists");
    }
    Ok(())
}
