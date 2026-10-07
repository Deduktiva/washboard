//! `inspect`: the structural report (PLAN §5.1). Counts and flags only, so the output can be
//! shared from machines holding confidential WSDLs; import diagnostics are not printed because
//! they contain file names and namespaces.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use washboard_core::wsdl::{self, Sources};

pub fn run(entry: &Path, extra: &[PathBuf], json: bool) -> anyhow::Result<ExitCode> {
    let sources = Sources::from_disk(entry, extra)?;
    let w = wsdl::load(&sources);
    if json {
        println!("{}", serde_json::to_string_pretty(&w.report)?);
    } else {
        print!("{}", w.report);
    }
    Ok(ExitCode::SUCCESS)
}
