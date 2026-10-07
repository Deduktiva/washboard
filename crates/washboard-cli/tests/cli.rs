//! Runs the built `washboard` binary against copies of `fixtures/` in temp dirs. The only
//! network traffic goes to a plain-HTTP server thread on 127.0.0.1 started by the test.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::Value;
use tempfile::TempDir;
use washboard_core::project::Project;

const BIN: &str = env!("CARGO_BIN_EXE_washboard");

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for e in fs::read_dir(from).expect("read_dir") {
        let e = e.expect("entry");
        let dest = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), &dest).expect("copy");
        }
    }
}

struct Env {
    _tmp: TempDir,
    /// Copy of the fixture folder.
    src: PathBuf,
    /// The project folder (not created yet).
    project: PathBuf,
}

fn env(fixture: &str) -> Env {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    copy_dir(&fixtures().join(fixture), &src);
    let project = tmp.path().join("My Project");
    Env {
        _tmp: tmp,
        src,
        project,
    }
}

fn cmd(args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .env_remove("WASHBOARD_PASSWORD")
        .stdin(Stdio::null());
    c
}

fn run(args: &[&str]) -> Output {
    cmd(args).output().expect("run washboard")
}

/// Runs a command on the env's project (`-C`).
fn wb(e: &Env, args: &[&str]) -> Output {
    let mut all = vec!["-C", e.project.to_str().expect("utf-8 path")];
    all.extend_from_slice(args);
    run(&all)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[track_caller]
fn ok(o: Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        stdout(&o),
        stderr(&o)
    );
    stdout(&o)
}

#[track_caller]
fn fails(o: &Output, code: i32, needle: &str) {
    assert_eq!(o.status.code(), Some(code), "stderr: {}", stderr(o));
    assert!(
        stderr(o).contains(needle),
        "expected {needle:?} in stderr: {}",
        stderr(o)
    );
}

fn customer() -> Env {
    let e = env("customer");
    let wsdl = e.src.join("CustomerService.wsdl");
    let xsd = e.src.join("xsd");
    let binding = e.src.join("CustomerBinding.wsdl");
    let out = ok(run(&[
        "project",
        "new",
        e.project.to_str().expect("utf-8"),
        "--wsdl",
        wsdl.to_str().expect("utf-8"),
        "--xsd-dir",
        binding.to_str().expect("utf-8"),
        "--xsd-dir",
        xsd.to_str().expect("utf-8"),
    ]));
    assert!(out.contains("created project \"My Project\""), "{out}");
    assert!(
        out.contains("https://customer.example.invalid/ws/customer"),
        "{out}"
    );
    e
}

fn legacy() -> Env {
    let e = env("legacy-rpc");
    let wsdl = e.src.join("Legacy.wsdl");
    ok(run(&[
        "project",
        "new",
        e.project.to_str().expect("utf-8"),
        "--wsdl",
        wsdl.to_str().expect("utf-8"),
        "--name",
        "Legacy",
    ]));
    e
}

#[test]
fn new_project_copies_files_and_lists_unsupported_operations() {
    let e = customer();
    assert!(e.project.join("wsdl/CustomerService.wsdl").is_file());
    assert!(e.project.join("wsdl/xsd/common/party.xsd").is_file());

    let list = ok(wb(&e, &["operation", "list"]));
    assert!(list.contains("CustomerBinding (SOAP 1.1)"), "{list}");
    assert!(list.contains("CreateOrder"), "{list}");
    assert!(
        list.contains("GetCustomer              unsupported: SOAP 1.2 is not supported"),
        "{list}"
    );

    let json: Value =
        serde_json::from_str(&ok(wb(&e, &["operation", "list", "--json"]))).expect("json");
    let ops = json.as_array().expect("array");
    assert_eq!(ops.len(), 3);
    let unsupported: Vec<&Value> = ops.iter().filter(|o| o["supported"] == false).collect();
    assert_eq!(unsupported.len(), 1);
    assert_eq!(unsupported[0]["binding"], "CustomerBinding12");
    assert_eq!(unsupported[0]["protocol"], "soap12");

    let show = ok(wb(&e, &["project", "show"]));
    assert!(show.contains("name:        My Project"), "{show}");
    assert!(show.contains("2 supported, 1 unsupported"), "{show}");
}

#[test]
fn new_project_refuses_unresolved_imports() {
    let e = env("customer");
    let wsdl = e.src.join("CustomerService.wsdl");
    let o = run(&[
        "project",
        "new",
        e.project.to_str().expect("utf-8"),
        "--wsdl",
        wsdl.to_str().expect("utf-8"),
    ]);
    fails(&o, 2, "import check found errors");
    assert!(!e.project.exists());
}

#[test]
fn legacy_rpc_lists_encoded_as_unsupported_and_templates_the_wrapper() {
    let e = legacy();
    let list = ok(wb(&e, &["operation", "list"]));
    assert!(list.contains("LegacyBinding (SOAP 1.1)"), "{list}");
    assert!(list.contains("Lookup                   rpc"), "{list}");
    assert!(
        list.contains("unsupported: use=\"encoded\" is not supported"),
        "{list}"
    );
    // Only one supported `Lookup`, so the short name resolves.
    let t = ok(wb(&e, &["operation", "template", "Lookup"]));
    assert!(t.contains("<soapenv:Envelope"), "{t}");
    assert!(t.contains(":Lookup"), "{t}");
    assert!(!t.contains("Header"), "{t}");
    let o = wb(
        &e,
        &["operation", "template", "LegacyEncodedBinding#Lookup"],
    );
    fails(&o, 2, "not supported");
    assert_eq!(ok(wb(&e, &["request", "new", "Lookup"])).trim(), "Lookup 1");
}

#[test]
fn templates_and_request_management() {
    let e = customer();
    let t = ok(wb(&e, &["operation", "template", "GetCustomer"]));
    assert!(t.starts_with("<soapenv:Envelope"), "{t}");
    assert!(t.contains("<soapenv:Header>"), "{t}");
    assert!(t.contains("RequestContext"), "{t}");
    // Printing writes nothing.
    assert_eq!(
        fs::read_dir(e.project.join("requests"))
            .expect("dir")
            .count(),
        0
    );

    let save = |args: &[&str]| ok(wb(&e, args)).trim().to_owned();
    assert_eq!(
        save(&["operation", "template", "GetCustomer", "--save"]),
        "GetCustomer 1"
    );
    assert_eq!(
        save(&[
            "operation",
            "template",
            "CustomerBinding#GetCustomer",
            "--save"
        ]),
        "GetCustomer 2"
    );
    assert_eq!(
        save(&[
            "operation",
            "template",
            "CreateOrder",
            "--save",
            "--name",
            "Order"
        ]),
        "Order"
    );
    let saved = fs::read_to_string(e.project.join("requests/GetCustomer 1.xml")).expect("file");
    assert_eq!(saved, t);
    assert_eq!(ok(wb(&e, &["request", "show", "GetCustomer 1"])), t);

    ok(wb(&e, &["request", "rename", "GetCustomer 2", "Second"]));
    assert!(e.project.join("requests/Second.xml").is_file());
    assert_eq!(save(&["request", "duplicate", "Second"]), "Second copy");
    ok(wb(&e, &["request", "delete", "Second"]));
    assert!(!e.project.join("requests/Second.xml").exists());
    fails(
        &wb(&e, &["request", "show", "Second"]),
        2,
        "no request named",
    );

    let from = e.src.join("requests/valid-create-order.xml");
    assert_eq!(
        save(&[
            "request",
            "new",
            "CreateOrder",
            "--from",
            from.to_str().expect("utf-8")
        ]),
        "CreateOrder 1"
    );

    let json: Value =
        serde_json::from_str(&ok(wb(&e, &["request", "list", "--json"]))).expect("json");
    let names: Vec<&str> = json
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        ["GetCustomer 1", "Second copy", "Order", "CreateOrder 1"]
    );
    assert_eq!(
        json[0]["operation"],
        "{urn:example:customer:service}CustomerBinding#GetCustomer"
    );

    fails(
        &wb(&e, &["request", "rename", "Order", "CreateOrder 1"]),
        2,
        "already exists",
    );
    fails(
        &wb(&e, &["request", "validate", "Order"]),
        2,
        "not available yet",
    );
}

#[test]
fn servers() {
    let e = customer();
    ok(wb(
        &e,
        &["server", "add", "Test", "https://test.invalid/ws"],
    ));
    ok(wb(
        &e,
        &[
            "server",
            "add",
            "Prod",
            "https://prod.invalid/ws",
            "--timeout",
            "5",
        ],
    ));
    fails(
        &wb(&e, &["server", "add", "Test", "https://other.invalid/"]),
        2,
        "already exists",
    );
    ok(wb(
        &e,
        &[
            "server",
            "edit",
            "Test",
            "--url",
            "https://test2.invalid/ws",
            "--username",
            "alice",
            "--ignore-tls-errors",
            "true",
        ],
    ));
    let list = ok(wb(&e, &["server", "list"]));
    assert!(
        list.contains("https://test2.invalid/ws  (basic auth as alice, TLS errors ignored)"),
        "{list}"
    );
    assert!(list.contains("timeout 5s"), "{list}");
    ok(wb(&e, &["server", "remove", "Prod"]));
    let list = ok(wb(&e, &["server", "list"]));
    assert!(!list.contains("Prod"), "{list}");
    fails(&wb(&e, &["server", "remove", "Prod"]), 2, "no server named");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn password_stdin_is_refused_without_a_secret_store() {
    let e = customer();
    let o = wb(
        &e,
        &[
            "server",
            "add",
            "Auth",
            "http://127.0.0.1:9/",
            "--username",
            "u",
            "--password-stdin",
        ],
    );
    fails(&o, 2, "cannot be stored on this platform");
    assert!(!ok(wb(&e, &["server", "list"])).contains("Auth"));
}

/// What the test server saw of one request.
struct Seen {
    head: String,
    body: String,
}

/// Serves `responses` in order, one per connection, then stops.
fn serve(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://{}/ws/customer",
        listener.local_addr().expect("addr")
    );
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (status, body) in responses {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read") == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let len = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let mut req_body = vec![0; len];
            reader.read_exact(&mut req_body).expect("body");
            let reason = if status == 200 { "OK" } else { "Error" };
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/xml; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write");
            let _ = tx.send(Seen {
                head,
                body: String::from_utf8_lossy(&req_body).into_owned(),
            });
        }
    });
    (url, rx)
}

const OK_RESPONSE: &str = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><m:GetCustomerResponse xmlns:m="urn:example:customer:messages"/></soapenv:Body></soapenv:Envelope>
"#;

const FAULT_RESPONSE: &str = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><soapenv:Fault><faultcode>soapenv:Server</faultcode><faultstring>customer locked</faultstring></soapenv:Fault></soapenv:Body></soapenv:Envelope>
"#;

#[test]
fn send_records_history_and_last_server() {
    let e = customer();
    let (url, seen) = serve(vec![
        (200, OK_RESPONSE),
        (500, FAULT_RESPONSE),
        (500, FAULT_RESPONSE),
        (200, OK_RESPONSE),
    ]);
    ok(wb(&e, &["server", "add", "Local", &url]));
    ok(wb(&e, &["server", "add", "Other", "http://127.0.0.1:9/"]));
    ok(wb(&e, &["operation", "template", "GetCustomer", "--save"]));
    let req = "GetCustomer 1";

    // Validation is not available yet, so sending needs the explicit flag.
    fails(&wb(&e, &["request", "send", req]), 2, "--skip-validation");

    let o = wb(
        &e,
        &[
            "request",
            "send",
            req,
            "--server",
            "Local",
            "--skip-validation",
        ],
    );
    assert_eq!(ok(o.clone_output()), OK_RESPONSE);
    assert!(stderr(&o).contains("HTTP/1.1 200 OK"), "{}", stderr(&o));
    let s = seen.recv().expect("request seen");
    assert!(
        s.head.starts_with("POST /ws/customer HTTP/1.1"),
        "{}",
        s.head
    );
    assert!(
        s.head
            .to_ascii_lowercase()
            .contains("soapaction: \"urn:example:customer:service/getcustomer\""),
        "{}",
        s.head
    );
    let sent = fs::read_to_string(e.project.join(format!("requests/{req}.xml"))).expect("file");
    assert_eq!(s.body, sent);

    // The last server is remembered, so --server is no longer needed.
    let json: Value =
        serde_json::from_str(&ok(wb(&e, &["request", "list", "--json"]))).expect("json");
    assert_eq!(json[0]["last_server"], "Local");

    // A SOAP fault is a normal response unless --fail-on-fault.
    let o = wb(&e, &["request", "send", req, "--skip-validation"]);
    assert_eq!(ok(o.clone_output()), FAULT_RESPONSE);
    assert!(
        stderr(&o).contains("SOAP fault: soapenv:Server: customer locked"),
        "{}",
        stderr(&o)
    );
    let o = wb(
        &e,
        &[
            "request",
            "send",
            req,
            "--skip-validation",
            "--fail-on-fault",
        ],
    );
    fails(&o, 1, "customer locked");

    // Transport errors exit with 1 and are recorded too.
    let o = wb(
        &e,
        &[
            "request",
            "send",
            req,
            "--server",
            "Other",
            "--skip-validation",
        ],
    );
    fails(&o, 1, "127.0.0.1:9");

    let hist: Value =
        serde_json::from_str(&ok(wb(&e, &["request", "history", req, "--json"]))).expect("json");
    let hist = hist.as_array().expect("array");
    assert_eq!(hist.len(), 4);
    assert_eq!(hist[0]["server"], "Other");
    assert!(hist[0]["error"].is_string());
    assert_eq!(hist[1]["soap_fault"], true);
    assert_eq!(hist[1]["http_status"], 500);
    assert_eq!(hist[3]["soap_fault"], false);
    assert_eq!(hist[3]["http_status"], 200);

    let text = ok(wb(&e, &["request", "history", req]));
    assert_eq!(text.lines().count(), 4, "{text}");
    assert!(text.contains("500 SOAP fault"), "{text}");
    let shown = ok(wb(&e, &["request", "history", req, "--show", "2"]));
    assert!(shown.contains("customer locked"), "{shown}");
    assert!(shown.contains(&sent), "{shown}");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn basic_auth_password_comes_from_the_environment() {
    let e = customer();
    let (url, seen) = serve(vec![(200, OK_RESPONSE)]);
    ok(wb(
        &e,
        &["server", "add", "Local", &url, "--username", "alice"],
    ));
    ok(wb(&e, &["operation", "template", "CreateOrder", "--save"]));
    // No terminal and no variable: refused before anything is sent.
    fails(
        &wb(
            &e,
            &["request", "send", "CreateOrder 1", "--skip-validation"],
        ),
        2,
        "WASHBOARD_PASSWORD",
    );
    let o = cmd(&[
        "-C",
        e.project.to_str().expect("utf-8"),
        "request",
        "send",
        "CreateOrder 1",
        "--skip-validation",
    ])
    .env("WASHBOARD_PASSWORD", "secret")
    .output()
    .expect("run");
    ok(o);
    let s = seen.recv().expect("seen");
    // base64("alice:secret")
    assert!(
        s.head.contains("YWxpY2U6c2VjcmV0"),
        "no basic auth header: {}",
        s.head
    );
    // CreateOrder has no soapAction: sent as an empty quoted string.
    assert!(
        s.head.to_ascii_lowercase().contains("soapaction: \"\""),
        "{}",
        s.head
    );
}

#[test]
fn writing_commands_fail_while_the_project_is_open_elsewhere() {
    let e = customer();
    ok(wb(&e, &["server", "add", "Test", "https://test.invalid/"]));
    ok(wb(&e, &["operation", "template", "GetCustomer", "--save"]));

    // Another handle (the app, in real life) holds the folder lock.
    let held = Project::open(&e.project).expect("open");

    for args in [
        &["server", "add", "X", "https://x.invalid/"][..],
        &["operation", "template", "GetCustomer", "--save"],
        &["request", "rename", "GetCustomer 1", "Renamed"],
        &["request", "send", "GetCustomer 1", "--skip-validation"],
    ] {
        fails(&wb(&e, args), 2, "already open");
    }
    for args in [
        &["operation", "list"][..],
        &["operation", "template", "GetCustomer"],
        &["request", "list", "--json"],
        &["request", "show", "GetCustomer 1"],
        &["request", "history", "GetCustomer 1"],
        &["server", "list"],
        &["project", "show"],
    ] {
        ok(wb(&e, args));
    }
    drop(held);
    ok(wb(&e, &["request", "rename", "GetCustomer 1", "Renamed"]));
}

#[test]
fn project_defaults_to_the_current_directory() {
    let e = customer();
    let o = cmd(&["request", "list"])
        .current_dir(&e.project)
        .output()
        .expect("run");
    assert_eq!(ok(o).trim(), "no requests");
    let o = cmd(&["request", "list"])
        .current_dir(&e.src)
        .output()
        .expect("run");
    fails(&o, 2, "not a Washboard project");
}

#[test]
fn inspect_reports_counts_without_names() {
    let e = env("customer");
    let wsdl = e.src.join("CustomerService.wsdl");
    let src = e.src.to_str().expect("utf-8");
    let text = ok(run(&[
        "inspect",
        wsdl.to_str().expect("utf-8"),
        "--xsd-dir",
        src,
    ]));
    assert!(text.contains("files supplied"), "{text}");
    for secret in ["urn:example", "Customer", "party.xsd"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let json: Value = serde_json::from_str(&ok(run(&[
        "inspect",
        wsdl.to_str().expect("utf-8"),
        "--xsd-dir",
        src,
        "--json",
    ])))
    .expect("json");
    assert_eq!(json["files_used"], 6);
    assert_eq!(json["operations"], 3);
    assert_eq!(json["operations_supported"], 2);
    assert_eq!(json["bindings_soap12"], 1);
    assert_eq!(json["refs_unresolved"], 0);
}

#[test]
fn usage_errors_exit_2() {
    let o = run(&["request"]);
    assert_eq!(o.status.code(), Some(2));
    let o = run(&["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
}

/// `Output` is not `Clone`; tests need both the exit status check and stderr.
trait CloneOutput {
    fn clone_output(&self) -> Output;
}

impl CloneOutput for Output {
    fn clone_output(&self) -> Output {
        Output {
            status: self.status,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        }
    }
}
