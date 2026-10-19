//! `washboard` — command-line harness for the core library.
//!
//! Exercises everything the app does, without a UI, so it can be tested on Linux and run
//! against WSDLs that cannot leave a developer's machine. It works on the same project folders
//! as the app, through `washboard_core::project` and `wsdl` only: no formats or rules of its
//! own.
//!
//! Commands are noun-verb (`request send`, `server add`, `param set`). `-C/--project` selects the project
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

use clap::{Parser, Subcommand};
use washboard_core::xml;

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
    /// Manage parameter sets (the rows of the parameter grid).
    #[command(subcommand)]
    Set(SetCommand),
    /// Manage parameters (the columns) and their values in each set.
    #[command(subcommand)]
    Param(ParamCommand),
}

#[derive(Debug, Subcommand)]
enum SetCommand {
    /// List sets with their values; `*` marks the project's set.
    List,
    Add {
        name: String,
    },
    Rm {
        name: String,
    },
    Rename {
        name: String,
        new_name: String,
    },
}

#[derive(Debug, Subcommand)]
enum ParamCommand {
    List,
    /// Add a parameter, unset in every set.
    Add {
        name: String,
    },
    /// Delete a parameter and its value in every set.
    Rm {
        name: String,
    },
    /// Rename a parameter in every set; requests using the old name are not changed.
    Rename {
        name: String,
        new_name: String,
    },
    /// Set the parameter's value in a set (an empty value is a value).
    Set {
        set: String,
        name: String,
        /// Values such as `-5` are values, not options.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Remove the parameter's value from a set.
    Unset {
        set: String,
        name: String,
    },
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
        /// Save as a new request in the operation's folder (named `<Operation> <n>` unless
        /// --name) and print its name.
        #[arg(long)]
        save: bool,
        #[arg(long, requires = "save")]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum RequestCommand {
    /// List requests by path (`Folder/Name`) with their operation and last server. Every
    /// `<name>` argument also takes a path.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a request for an operation from its template, or from a file, in the
    /// operation's folder.
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
        /// Print it as it would be sent, with `${...}` placeholders filled in.
        #[arg(long)]
        resolved: bool,
        /// Parameter set to fill in from; defaults to the request's pin, else the project's.
        #[arg(long, requires = "resolved")]
        set: Option<String>,
    },
    Rename {
        name: String,
        new_name: String,
    },
    /// Copy a request as `<name> copy`; prints the new name.
    Duplicate {
        name: String,
    },
    /// Move a request into a folder (created if missing; `/` is the top level).
    Mv {
        name: String,
        folder: String,
    },
    /// Delete a request together with its history.
    Delete {
        name: String,
    },
    /// Re-indent a request's XML, as Format XML (⌃I) in the app does; prints nothing.
    Format {
        name: String,
        /// Spaces per level. The app's own setting is not read.
        #[arg(long, default_value_t = xml::DEFAULT_INDENT, value_parser = indent_width)]
        indent: usize,
        /// Write nothing; exit with 1 if the request is not formatted.
        #[arg(long)]
        check: bool,
    },
    /// Validate a request against the project's WSDL.
    Validate {
        name: String,
        /// Parameter set to fill in from; defaults to the request's pin, else the project's.
        #[arg(long)]
        set: Option<String>,
    },
    /// Validate and send; prints the status line to stderr and the body to stdout.
    Send {
        name: String,
        /// Server name; defaults to the request's pin, else the project's server.
        #[arg(long)]
        server: Option<String>,
        /// Parameter set to fill in from; defaults to the request's pin, else the project's.
        #[arg(long)]
        set: Option<String>,
        /// Send even if validation finds errors (to test how a server handles broken
        /// requests). The errors are still printed. Placeholders that cannot be filled in
        /// still block the send.
        #[arg(long)]
        skip_validation: bool,
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

#[derive(Debug, Subcommand)]
enum ServerCommand {
    List,
    Add {
        name: String,
        url: String,
        #[command(flatten)]
        opts: commands::server::Options,
        /// Skip certificate and hostname verification for this server.
        #[arg(long)]
        ignore_tls_errors: bool,
    },
    Edit {
        name: String,
        #[command(flatten)]
        edit: commands::server::Edit,
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
            RequestCommand::Show {
                name,
                resolved,
                set,
            } => request::show(dir, &name, resolved, set.as_deref()),
            RequestCommand::Rename { name, new_name } => request::rename(dir, &name, &new_name),
            RequestCommand::Duplicate { name } => request::duplicate(dir, &name),
            RequestCommand::Mv { name, folder } => request::mv(dir, &name, &folder),
            RequestCommand::Delete { name } => request::delete(dir, &name),
            RequestCommand::Format {
                name,
                indent,
                check,
            } => request::format(dir, &name, indent, check),
            RequestCommand::Validate { name, set } => {
                validation::command(dir, &name, set.as_deref())
            }
            RequestCommand::Send {
                name,
                server,
                set,
                skip_validation,
                fail_on_fault,
            } => send::run(
                dir,
                &name,
                server.as_deref(),
                set.as_deref(),
                skip_validation,
                fail_on_fault,
            ),
            RequestCommand::History { name, json, show } => history::run(dir, &name, json, show),
        },
        Command::Server(cmd) => match cmd {
            ServerCommand::List => server::list(dir),
            ServerCommand::Add {
                name,
                url,
                opts,
                ignore_tls_errors,
            } => server::add(dir, &name, &url, &opts, ignore_tls_errors),
            ServerCommand::Edit { name, edit } => server::edit(dir, &name, &edit),
            ServerCommand::Remove { name } => server::remove(dir, &name),
        },
        Command::Set(cmd) => match cmd {
            SetCommand::List => parameters::set_list(dir),
            SetCommand::Add { name } => parameters::set_add(dir, &name),
            SetCommand::Rm { name } => parameters::set_rm(dir, &name),
            SetCommand::Rename { name, new_name } => parameters::set_rename(dir, &name, &new_name),
        },
        Command::Param(cmd) => match cmd {
            ParamCommand::List => parameters::param_list(dir),
            ParamCommand::Add { name } => parameters::param_add(dir, &name),
            ParamCommand::Rm { name } => parameters::param_rm(dir, &name),
            ParamCommand::Rename { name, new_name } => {
                parameters::param_rename(dir, &name, &new_name)
            }
            ParamCommand::Set { set, name, value } => {
                parameters::param_value(dir, &set, &name, Some(&value))
            }
            ParamCommand::Unset { set, name } => parameters::param_value(dir, &set, &name, None),
        },
    }
}

/// `--indent`: a width in [`xml::INDENT_RANGE`], as the app's setting allows.
fn indent_width(arg: &str) -> Result<usize, String> {
    let width: usize = arg.parse().map_err(|e| format!("{e}"))?;
    let range = xml::INDENT_RANGE;
    if range.contains(&width) {
        return Ok(width);
    }
    Err(format!("{width} is not in {range:?}"))
}
