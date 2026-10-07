//! `washboard` — command-line harness for the core library.
//!
//! Exercises everything the app does, without a UI, so it can be tested on Linux and run
//! against WSDLs that cannot leave a developer's machine. It works on the same project folders
//! as the app, through `washboard_core::project` and `wsdl` only: no formats or rules of its
//! own.
//!
//! Commands are noun-verb (`request send`, `server add`). `-C/--project` selects the project
//! folder (default: the current directory) for everything except `inspect` and `project new`.
//!
//! Exit codes: 0 ok; 1 command-level failure (send transport error, SOAP fault with
//! `--fail-on-fault`); 2 usage, I/O and project errors (clap uses 2 for usage errors too).

mod commands;
mod passwords;
mod support;
mod validation;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "washboard",
    version,
    about = "Washboard SOAP client, command-line harness"
)]
struct Cli {
    /// Project folder (default: the current directory). Ignored by `inspect` and
    /// `project new`.
    #[arg(short = 'C', long = "project", global = true, value_name = "DIR")]
    project: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a structural report of a WSDL and its imports (counts and flags, no names).
    Inspect {
        wsdl: PathBuf,
        /// Files or directories searched for referenced XSD/WSDL files.
        #[arg(long = "xsd-dir")]
        xsd_dirs: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Create a project, replace its WSDL, show a summary.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// List the WSDL's operations, generate request templates.
    #[command(subcommand)]
    Operation(OperationCommand),
    /// Manage, validate and send requests.
    #[command(subcommand)]
    Request(RequestCommand),
    /// Manage servers.
    #[command(subcommand)]
    Server(ServerCommand),
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Create a project folder from a WSDL and supporting files.
    New {
        /// The project folder; created if missing, must be empty if it exists.
        dir: PathBuf,
        #[arg(long)]
        wsdl: PathBuf,
        /// Files or directories searched for referenced XSD/WSDL files.
        #[arg(long = "xsd-dir")]
        xsd_dirs: Vec<PathBuf>,
        /// Project name; defaults to the folder name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Replace the project's WSDL set (the previous set is kept in wsdl/.previous).
    ReplaceWsdl {
        #[arg(long)]
        wsdl: PathBuf,
        #[arg(long = "xsd-dir")]
        xsd_dirs: Vec<PathBuf>,
    },
    /// Name, entry WSDL, import-check summary and counts.
    Show,
}

#[derive(Debug, Subcommand)]
enum OperationCommand {
    /// List operations, unsupported ones included and marked.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print a generated request for an operation, or save it as a new request.
    Template {
        /// `Operation`, `Binding#Operation` or `{namespace}Binding#Operation`.
        operation: String,
        /// Save as a new request (named `<Operation> <n>` unless --name) and print its name.
        #[arg(long)]
        save: bool,
        #[arg(long, requires = "save")]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum RequestCommand {
    /// List requests with their operation and last server.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a request for an operation from its template, or from a file.
    New {
        /// `Operation`, `Binding#Operation` or `{namespace}Binding#Operation`.
        operation: String,
        /// Defaults to `<Operation> <n>` with the lowest free n.
        #[arg(long)]
        name: Option<String>,
        /// Take the content from this file (`-` reads stdin) instead of the template.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Print the request's XML.
    Show {
        name: String,
    },
    Rename {
        name: String,
        new_name: String,
    },
    /// Copy a request as `<name> copy`; prints the new name.
    Duplicate {
        name: String,
    },
    /// Delete a request together with its history.
    Delete {
        name: String,
    },
    /// Validate a request against the project's WSDL.
    Validate {
        name: String,
    },
    /// Validate and send; prints the status line to stderr and the body to stdout.
    Send {
        name: String,
        /// Server name; defaults to the request's last server.
        #[arg(long)]
        server: Option<String>,
        /// Exit with 1 when the response is a SOAP fault.
        #[arg(long)]
        fail_on_fault: bool,
    },
    /// Show the send history, newest first.
    History {
        name: String,
        #[arg(long)]
        json: bool,
        /// Print the stored request and response of entry N (1 = newest).
        #[arg(long, value_name = "N")]
        show: Option<usize>,
    },
}

#[derive(Debug, Args)]
struct ServerOptions {
    /// Basic-auth user name.
    #[arg(long)]
    username: Option<String>,
    /// Read the basic-auth password from the first line of stdin (macOS only: stored in the
    /// Keychain).
    #[arg(long)]
    password_stdin: bool,
    /// Request timeout in seconds.
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,
}

impl ServerOptions {
    fn into_options(self) -> commands::server::Options {
        commands::server::Options {
            username: self.username,
            password_stdin: self.password_stdin,
            timeout: self.timeout,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    List,
    Add {
        name: String,
        url: String,
        #[command(flatten)]
        opts: ServerOptions,
        /// Skip certificate and hostname verification for this server.
        #[arg(long)]
        ignore_tls_errors: bool,
    },
    Edit {
        name: String,
        #[arg(long = "name", value_name = "NEW_NAME")]
        new_name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[command(flatten)]
        opts: ServerOptions,
        /// Switch to no authentication (removes the stored password).
        #[arg(long, conflicts_with_all = ["username", "password_stdin"])]
        no_auth: bool,
        #[arg(long, value_name = "BOOL")]
        ignore_tls_errors: Option<bool>,
    },
    Remove {
        name: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dir = cli.project.unwrap_or_else(|| PathBuf::from("."));
    match run(&dir, cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("washboard: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(dir: &Path, command: Command) -> anyhow::Result<ExitCode> {
    use commands::*;
    match command {
        Command::Inspect {
            wsdl,
            xsd_dirs,
            json,
        } => inspect::run(&wsdl, &xsd_dirs, json),
        Command::Project(cmd) => match cmd {
            ProjectCommand::New {
                dir,
                wsdl,
                xsd_dirs,
                name,
            } => project::new(&dir, &wsdl, &xsd_dirs, name.as_deref()),
            ProjectCommand::ReplaceWsdl { wsdl, xsd_dirs } => {
                project::replace_wsdl(dir, &wsdl, &xsd_dirs)
            }
            ProjectCommand::Show => project::show(dir),
        },
        Command::Operation(cmd) => match cmd {
            OperationCommand::List { json } => operation::list(dir, json),
            OperationCommand::Template {
                operation,
                save,
                name,
            } => operation::template(dir, &operation, save, name.as_deref()),
        },
        Command::Request(cmd) => match cmd {
            RequestCommand::List { json } => request::list(dir, json),
            RequestCommand::New {
                operation,
                name,
                from,
            } => request::new(dir, &operation, name.as_deref(), from.as_deref()),
            RequestCommand::Show { name } => request::show(dir, &name),
            RequestCommand::Rename { name, new_name } => request::rename(dir, &name, &new_name),
            RequestCommand::Duplicate { name } => request::duplicate(dir, &name),
            RequestCommand::Delete { name } => request::delete(dir, &name),
            RequestCommand::Validate { name } => validation::command(dir, &name),
            RequestCommand::Send {
                name,
                server,
                fail_on_fault,
            } => send::run(dir, &name, server.as_deref(), fail_on_fault),
            RequestCommand::History { name, json, show } => history::run(dir, &name, json, show),
        },
        Command::Server(cmd) => match cmd {
            ServerCommand::List => server::list(dir),
            ServerCommand::Add {
                name,
                url,
                opts,
                ignore_tls_errors,
            } => server::add(dir, &name, &url, &opts.into_options(), ignore_tls_errors),
            ServerCommand::Edit {
                name,
                new_name,
                url,
                opts,
                no_auth,
                ignore_tls_errors,
            } => server::edit(
                dir,
                &name,
                &server::Edit {
                    new_name,
                    url,
                    opts: opts.into_options(),
                    no_auth,
                    ignore_tls_errors,
                },
            ),
            ServerCommand::Remove { name } => server::remove(dir, &name),
        },
    }
}
