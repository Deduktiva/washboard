//! Only `washboard_core::http` may talk to the network (PLAN §6, CLAUDE.md "Network").
//!
//! `deny.toml` bans known HTTP stacks and telemetry crates by name; these tests check *where*
//! the network-capable crates we do use come from:
//! - in the dependency graph, every network-capable crate is reached only through
//!   `washboard-core`'s dependency on `ureq`;
//! - in the source, socket and HTTP APIs appear only under `washboard-core/src/http/`.
//!
//! The graph is checked for the host platform, so the Linux and macOS CI jobs each cover
//! their own platform's dependencies (e.g. `security-framework` only on macOS).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Crates that open sockets or speak HTTP/TLS. Any of them in the normal (non-dev) dependency
/// closure of anything but `ureq` means a second way onto the network. `security-framework`
/// is missing on purpose: core uses it directly for the Keychain, and `native-tls` uses it for
/// TLS on macOS, which this check sees through `native-tls`.
const NETWORK_CRATES: &[&str] = &[
    "ureq",
    "ureq-proto",
    "native-tls",
    "openssl",
    "openssl-sys",
    "schannel",
    "rustls",
    "http",
    "httparse",
    "socket2",
    "mio",
    "tokio",
    "hyper",
    "reqwest",
    "curl",
    "curl-sys",
];

/// The one allowed path onto the network: this workspace crate's direct dependency on this
/// crate.
const ALLOWED_EDGE: (&str, &str) = ("washboard-core", "ureq");

/// Identifiers that mean socket or HTTP access in Rust source.
const NETWORK_APIS: &[&str] = &[
    "ureq",
    "native_tls",
    "std::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "ToSocketAddrs",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn host_triple() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = Command::new(rustc)
        .arg("-vV")
        .current_dir(workspace_root())
        .output()
        .expect("run rustc -vV");
    let text = String::from_utf8(out.stdout).expect("rustc output is UTF-8");
    text.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("rustc -vV prints the host triple")
        .to_owned()
}

fn metadata() -> Value {
    // `--offline`: everything this graph needs was fetched to build this test.
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .args(["--filter-platform", &host_triple()])
        .current_dir(workspace_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON")
}

/// Package id → (name, ids of its normal dependencies). Dev and build dependencies don't end up
/// in the binaries; test servers (e.g. `rustls` in core's dev-dependencies) are fine.
fn normal_graph(meta: &Value) -> HashMap<String, (String, Vec<String>)> {
    let names: HashMap<&str, &str> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().expect("id"),
                p["name"].as_str().expect("name"),
            )
        })
        .collect();
    meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve.nodes")
        .iter()
        .map(|node| {
            let id = node["id"].as_str().expect("node id");
            let deps = node["deps"]
                .as_array()
                .expect("node deps")
                .iter()
                .filter(|d| {
                    d["dep_kinds"]
                        .as_array()
                        .expect("dep_kinds")
                        .iter()
                        .any(|k| k["kind"].is_null())
                })
                .map(|d| d["pkg"].as_str().expect("dep pkg").to_owned())
                .collect();
            (id.to_owned(), (names[id].to_owned(), deps))
        })
        .collect()
}

/// Network-capable crates in the normal-dependency closure of `id`, including `id` itself.
fn network_crates_below(graph: &HashMap<String, (String, Vec<String>)>, id: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![id.to_owned()];
    let mut found = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let (name, deps) = &graph[&id];
        if NETWORK_CRATES.contains(&name.as_str()) {
            found.insert(name.clone());
        }
        stack.extend(deps.iter().cloned());
    }
    found.into_iter().collect()
}

#[test]
fn network_crates_only_come_through_core_ureq() {
    let meta = metadata();
    let graph = normal_graph(&meta);
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .map(|m| m.as_str().expect("member id"))
        .collect();

    let mut violations = Vec::new();
    let mut saw_allowed_edge = false;
    for member in &members {
        let (member_name, deps) = &graph[*member];
        for dep in deps {
            let dep_name = &graph[dep].0;
            if members.contains(&dep.as_str()) {
                // Workspace crates are checked on their own.
                continue;
            }
            if (member_name.as_str(), dep_name.as_str()) == ALLOWED_EDGE {
                saw_allowed_edge = true;
                continue;
            }
            let found = network_crates_below(&graph, dep);
            if !found.is_empty() {
                violations.push(format!("{member_name} -> {dep_name} pulls in {found:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "network-capable crates outside washboard-core's ureq dependency \
         (only washboard_core::http may use the network, PLAN §6):\n{}",
        violations.join("\n")
    );
    // If ureq is renamed or replaced, this test has to be revisited rather than pass vacuously.
    assert!(
        saw_allowed_edge,
        "washboard-core no longer depends on ureq; update this test"
    );
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn network_apis_only_in_core_http() {
    let root = workspace_root();
    let allowed = root.join("crates/washboard-core/src/http");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("read crates/") {
        let krate = entry.expect("dir entry").path();
        // Integration tests (`tests/`) may run local servers; they never ship.
        rust_sources(&krate.join("src"), &mut files);
        let build = krate.join("build.rs");
        if build.exists() {
            files.push(build);
        }
    }
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut violations = Vec::new();
    for file in files.iter().filter(|f| !f.starts_with(&allowed)) {
        let text = std::fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            for api in NETWORK_APIS.iter().filter(|api| line.contains(*api)) {
                let shown = file.strip_prefix(&root).unwrap_or(file).display();
                violations.push(format!("{shown}:{}: {api}", n + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "socket/HTTP APIs outside crates/washboard-core/src/http (PLAN §6):\n{}",
        violations.join("\n")
    );
}
