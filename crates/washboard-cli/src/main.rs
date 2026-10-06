//! `washboard` — command-line harness for the core library.
//!
//! Exercises everything the app does, without a UI, so it can be tested on Linux and run
//! against WSDLs that cannot leave a developer's machine.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "washboard",
    version,
    about = "Washboard SOAP client, command-line harness"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a structural report of a WSDL and its imports (counts and flags, no contents).
    Inspect {
        wsdl: PathBuf,
        /// Directories searched for referenced XSD/WSDL files.
        #[arg(long = "xsd-dir")]
        xsd_dirs: Vec<PathBuf>,
    },
    /// Create a project folder from a WSDL and supporting files.
    NewProject {
        dir: PathBuf,
        #[arg(long)]
        wsdl: PathBuf,
        #[arg(long = "xsd-dir")]
        xsd_dirs: Vec<PathBuf>,
    },
    /// List operations and requests of a project.
    List { project: PathBuf },
    /// Validate a request file of a project.
    Validate { project: PathBuf, request: String },
    /// Print a generated request template for an operation.
    Template { project: PathBuf, operation: String },
    /// Validate and send a request to one of the project's servers.
    Send {
        project: PathBuf,
        request: String,
        #[arg(long)]
        server: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Inspect { .. } => "inspect",
        Command::NewProject { .. } => "new-project",
        Command::List { .. } => "list",
        Command::Validate { .. } => "validate",
        Command::Template { .. } => "template",
        Command::Send { .. } => "send",
    };
    eprintln!("washboard {name}: not implemented yet");
    ExitCode::from(2)
}
