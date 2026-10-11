//! Runs the built `washboard` binary against copies of `fixtures/` in temp dirs. The only
//! network traffic goes to a plain-HTTP server thread on 127.0.0.1 started by the test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;
use washboard_core::project::Project;
use washboard_test_support::{fixtures, server};

const BIN: &str = env!("CARGO_BIN_EXE_washboard");

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

/// Overwrites a request's file with a fixture request body, so that `send` has something
/// that validates (generated templates are full of `?` placeholders on purpose).
fn fill_from_fixture(e: &Env, request: &str, fixture: &str) {
    let text = fs::read_to_string(e.src.join("requests").join(fixture)).expect("fixture request");
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("<soapenv:Envelope"))
        .collect();
    fs::write(request_file(e, request), format!("{}\n", body.join("\n"))).expect("write request");
}

/// The file of the request named `name`, wherever its folder.
fn request_file(e: &Env, name: &str) -> PathBuf {
    let project = Project::open_read_only(&e.project).expect("open");
    let r = project
        .requests()
        .expect("requests")
        .into_iter()
        .find(|r| r.name == name)
        .expect("request");
    project.request_path(r.id).expect("path")
}

/// Runs a command on the env's project (`-C`).
fn wb(e: &Env, args: &[&str]) -> Output {
    let mut all = vec!["-C", path(&e.project)];
    all.extend_from_slice(args);
    run(&all)
}

/// A command's output line, such as the name of the request it saved.
fn line(e: &Env, args: &[&str]) -> String {
    ok(wb(e, args)).trim().to_owned()
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

fn path(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// `project new` on the env's project from the copied fixture's `wsdl`, with `args` after.
fn project_new(e: &Env, wsdl: &str, args: &[&str]) -> Output {
    let wsdl = e.src.join(wsdl);
    let mut all = vec!["project", "new", path(&e.project), "--wsdl", path(&wsdl)];
    all.extend_from_slice(args);
    run(&all)
}

fn customer() -> Env {
    let e = env("customer");
    let binding = e.src.join("CustomerBinding.wsdl");
    let xsd = e.src.join("xsd");
    let out = ok(project_new(
        &e,
        "CustomerService.wsdl",
        &["--xsd-dir", path(&binding), "--xsd-dir", path(&xsd)],
    ));
    assert!(out.contains("created project \"My Project\""), "{out}");
    assert!(
        out.contains("https://customer.example.invalid/ws/customer"),
        "{out}"
    );
    e
}

fn legacy() -> Env {
    let e = env("legacy-rpc");
    ok(project_new(&e, "Legacy.wsdl", &["--name", "Legacy"]));
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
    fs::remove_file(e.src.join("xsd/ext/audit.xsd")).expect("remove");
    let o = project_new(&e, "CustomerService.wsdl", &[]);
    fails(&o, 2, "import check found errors");
    let stderr = stderr(&o);
    assert!(
        stderr.contains("\"ext/audit.xsd\" was not supplied"),
        "{stderr}"
    );
    assert!(!e.project.exists());
}

#[test]
fn new_project_finds_imports_next_to_the_wsdl() {
    let e = env("customer");
    let o = project_new(&e, "CustomerService.wsdl", &[]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let stderr = stderr(&o);
    assert!(
        stderr.contains("found ") && stderr.contains("audit.xsd"),
        "{stderr}"
    );
    assert!(e.project.join("wsdl/xsd/ext/audit.xsd").is_file());
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
    assert_eq!(line(&e, &["request", "new", "Lookup"]), "Lookup 1");
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

    assert_eq!(
        line(&e, &["operation", "template", "GetCustomer", "--save"]),
        "GetCustomer 1"
    );
    assert_eq!(
        line(
            &e,
            &[
                "operation",
                "template",
                "CustomerBinding#GetCustomer",
                "--save"
            ]
        ),
        "GetCustomer 2"
    );
    assert_eq!(
        line(
            &e,
            &[
                "operation",
                "template",
                "CreateOrder",
                "--save",
                "--name",
                "Order"
            ]
        ),
        "Order"
    );
    // Saved into the operation's folder: one supported port (the other is SOAP 1.2).
    let saved =
        fs::read_to_string(e.project.join("requests/GetCustomer/GetCustomer 1.xml")).expect("file");
    assert_eq!(saved, t);
    assert!(e.project.join("requests/CreateOrder/Order.xml").is_file());
    assert_eq!(ok(wb(&e, &["request", "show", "GetCustomer 1"])), t);
    assert_eq!(
        ok(wb(&e, &["request", "show", "GetCustomer/GetCustomer 1"])),
        t
    );

    ok(wb(&e, &["request", "rename", "GetCustomer 2", "Second"]));
    assert!(e.project.join("requests/GetCustomer/Second.xml").is_file());
    assert_eq!(line(&e, &["request", "duplicate", "Second"]), "Second copy");
    assert!(
        e.project
            .join("requests/GetCustomer/Second copy.xml")
            .is_file()
    );
    ok(wb(&e, &["request", "delete", "GetCustomer/Second"]));
    assert!(!e.project.join("requests/GetCustomer/Second.xml").exists());
    fails(
        &wb(&e, &["request", "show", "Second"]),
        2,
        "no request named",
    );

    let from = e.src.join("requests/valid-create-order.xml");
    assert_eq!(
        line(
            &e,
            &["request", "new", "CreateOrder", "--from", path(&from)]
        ),
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
    assert_eq!(json[2]["folder"], "CreateOrder");
    assert_eq!(json[2]["path"], "CreateOrder/Order");
    assert_eq!(
        json[0]["operation"],
        "{urn:example:customer:service}CustomerBinding#GetCustomer"
    );

    // mv creates the folder; `/` is the top level. The text list prints paths.
    ok(wb(&e, &["request", "mv", "Second copy", "Kept/Old"]));
    assert!(
        e.project
            .join("requests/Kept/Old/Second copy.xml")
            .is_file()
    );
    let list = ok(wb(&e, &["request", "list"]));
    assert!(list.contains("Kept/Old/Second copy "), "{list}");
    assert!(list.contains("GetCustomer/GetCustomer 1 "), "{list}");
    ok(wb(&e, &["request", "mv", "Kept/Old/Second copy", "/"]));
    assert!(e.project.join("requests/Second copy.xml").is_file());

    // Two files of one name from Finder or git: a bare name asks for the path.
    fs::write(e.project.join("requests/Kept/Order.xml"), "<x/>").expect("write");
    let o = wb(&e, &["request", "delete", "Order"]);
    fails(&o, 2, "several requests are named \"Order\"");
    assert!(
        stderr(&o).contains("CreateOrder/Order, Kept/Order"),
        "{}",
        stderr(&o)
    );
    ok(wb(&e, &["request", "delete", "Kept/Order"]));

    fails(
        &wb(&e, &["request", "rename", "Order", "CreateOrder 1"]),
        2,
        "already exists",
    );
    // Validation: the generated template still has its `?` placeholders.
    let o = wb(&e, &["request", "validate", "Order"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("Order: 1 error"), "{}", stderr(&o));
    // rustc-style excerpt: location, the source line, the bad value underlined.
    assert!(stderr(&o).contains("  --> Order:14:24\n"), "{}", stderr(&o));
    assert!(
        stderr(&o).contains(
            "14 |           <com:country>?</com:country>\n   |                        ^\n"
        ),
        "{}",
        stderr(&o)
    );
    fill_from_fixture(&e, "Order", "valid-create-order.xml");
    assert_eq!(
        ok(wb(&e, &["request", "validate", "Order"])),
        "Order: valid\n"
    );
}

#[test]
fn format_reindents_checks_and_leaves_malformed_requests_alone() {
    let e = legacy();
    let name = ok(wb(&e, &["operation", "template", "Lookup", "--save"]));
    let name = name.trim();
    let file = request_file(&e, name);
    let generated = fs::read_to_string(&file).expect("file");
    // A generated request is already formatted at the default indent.
    ok(wb(&e, &["request", "format", name, "--check"]));
    fails(
        &wb(&e, &["request", "format", name, "--check", "--indent", "4"]),
        1,
        "not formatted",
    );
    assert_eq!(
        fs::read_to_string(&file).expect("file"),
        generated,
        "--check writes nothing"
    );

    fs::write(&file, "<a><b>x</b></a>").expect("write");
    assert_eq!(
        ok(wb(&e, &["request", "format", name, "--indent", "4"])),
        ""
    );
    assert_eq!(
        fs::read_to_string(&file).expect("file"),
        "<a>\n    <b>x</b>\n</a>\n"
    );
    ok(wb(
        &e,
        &["request", "format", name, "--indent", "4", "--check"],
    ));

    fs::write(&file, "<a><b></a>").expect("write");
    fails(
        &wb(&e, &["request", "format", name]),
        1,
        &format!("{name}:1:"),
    );
    assert_eq!(fs::read_to_string(&file).expect("file"), "<a><b></a>");

    for indent in ["0", "9", "tab"] {
        let o = wb(&e, &["request", "format", name, "--indent", indent]);
        assert_eq!(o.status.code(), Some(2), "--indent {indent}");
    }
}

#[test]
fn replace_wsdl_reports_operations_and_requests_that_no_longer_validate() {
    let e = customer();
    assert_eq!(
        line(
            &e,
            &[
                "operation",
                "template",
                "GetCustomer",
                "--save",
                "--name",
                "Good"
            ]
        ),
        "Good"
    );
    assert_eq!(
        line(
            &e,
            &[
                "operation",
                "template",
                "GetCustomer",
                "--save",
                "--name",
                "Bad"
            ]
        ),
        "Bad"
    );
    fill_from_fixture(&e, "Good", "valid-get-customer.xml");
    fill_from_fixture(&e, "Bad", "invalid-bad-simple-type.xml");
    let replace = |wsdl: &Path| {
        ok(wb(
            &e,
            &[
                "project",
                "replace-wsdl",
                "--wsdl",
                path(wsdl),
                "--xsd-dir",
                path(&e.src),
            ],
        ))
    };

    // The same WSDL again: nothing added or removed, only the broken request is named.
    let out = replace(&e.src.join("CustomerService.wsdl"));
    assert!(
        !out.contains("added:") && !out.contains("removed:"),
        "{out}"
    );
    assert!(out.contains("no longer valid: \"Bad\"\n"), "{out}");
    assert!(!out.contains("\"Good\""), "{out}");
    assert!(out.contains("request validate <name>"), "{out}");

    // A different service: its operations replace the old ones and both requests break.
    let good = fs::read_to_string(request_file(&e, "Good")).expect("file");
    let legacy = env("legacy-rpc");
    let out = replace(&legacy.src.join("Legacy.wsdl"));
    assert!(
        out.contains("  added:   ") && out.contains("Lookup"),
        "{out}"
    );
    assert!(
        out.contains("  removed: ") && out.contains("CreateOrder"),
        "{out}"
    );
    assert!(
        out.contains("no longer valid: \"Bad\"\n") && out.contains("no longer valid: \"Good\"\n"),
        "{out}"
    );
    // Requests are never rewritten.
    assert_eq!(
        fs::read_to_string(request_file(&e, "Good")).expect("file"),
        good
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
    // The project's rules, checked before anything is written.
    for (args, message) in [
        (
            &["server", "add", " ", "https://x.invalid/"][..],
            "name may not be empty",
        ),
        (&["server", "add", "X", ""], "URL may not be empty"),
        (&["server", "add", "X", "https://"], "not a valid URL"),
        (
            &["server", "add", "X", "ftp://x.invalid/"],
            "http:// or https://",
        ),
        (
            &["server", "edit", "Test", "--name", " Prod "],
            "\"Prod\" already exists",
        ),
        (
            &["server", "edit", "Test", "--url", "x.invalid"],
            "http:// or https://",
        ),
    ] {
        fails(&wb(&e, args), 2, message);
    }
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

const OK_RESPONSE: &str = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><m:GetCustomerResponse xmlns:m="urn:example:customer:messages"/></soapenv:Body></soapenv:Envelope>
"#;

const FAULT_RESPONSE: &str = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><soapenv:Fault><faultcode>soapenv:Server</faultcode><faultstring>customer locked</faultstring></soapenv:Fault></soapenv:Body></soapenv:Envelope>
"#;

#[test]
fn send_records_history_and_last_server() {
    let e = customer();
    let (url, seen) = server::serve(vec![
        ("200 OK", OK_RESPONSE),
        ("500 Internal Server Error", FAULT_RESPONSE),
        ("500 Internal Server Error", FAULT_RESPONSE),
        ("200 OK", OK_RESPONSE),
    ]);
    ok(wb(&e, &["server", "add", "Local", &url]));
    ok(wb(&e, &["server", "add", "Other", "http://127.0.0.1:9/"]));
    ok(wb(&e, &["operation", "template", "GetCustomer", "--save"]));
    let req = "GetCustomer 1";

    // An invalid request is never sent.
    fill_from_fixture(&e, req, "invalid-bad-simple-type.xml");
    let o = wb(&e, &["request", "send", req, "--server", "Local"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("not sent"), "{}", stderr(&o));
    fill_from_fixture(&e, req, "valid-get-customer.xml");

    let o = wb(&e, &["request", "send", req, "--server", "Local"]);
    assert_eq!(ok(o.clone()), OK_RESPONSE);
    assert!(stderr(&o).contains("HTTP/1.1 200 OK"), "{}", stderr(&o));
    let s = seen.recv().expect("request seen");
    assert!(s.head.starts_with("POST /soap HTTP/1.1"), "{}", s.head);
    assert!(
        s.head
            .to_ascii_lowercase()
            .contains("soapaction: \"urn:example:customer:service/getcustomer\""),
        "{}",
        s.head
    );
    let sent = fs::read_to_string(request_file(&e, req)).expect("file");
    assert_eq!(s.body, sent);

    // The last server is remembered, so --server is no longer needed.
    let json: Value =
        serde_json::from_str(&ok(wb(&e, &["request", "list", "--json"]))).expect("json");
    assert_eq!(json[0]["last_server"], "Local");

    // A SOAP fault is a normal response unless --fail-on-fault.
    let o = wb(&e, &["request", "send", req]);
    assert_eq!(ok(o.clone()), FAULT_RESPONSE);
    assert!(
        stderr(&o).contains("SOAP fault: soapenv:Server: customer locked"),
        "{}",
        stderr(&o)
    );
    let o = wb(&e, &["request", "send", req, "--fail-on-fault"]);
    fails(&o, 1, "customer locked");

    // Transport errors exit with 1 and are recorded too.
    let o = wb(&e, &["request", "send", req, "--server", "Other"]);
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
    assert!(shown.contains("\nPOST /"), "{shown}");
    assert!(shown.contains("\nsoapaction: "), "{shown}");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn basic_auth_password_comes_from_the_environment() {
    let e = customer();
    let (url, seen) = server::serve(vec![("200 OK", OK_RESPONSE)]);
    ok(wb(
        &e,
        &["server", "add", "Local", &url, "--username", "alice"],
    ));
    ok(wb(&e, &["operation", "template", "CreateOrder", "--save"]));
    fill_from_fixture(&e, "CreateOrder 1", "valid-create-order.xml");
    // No terminal and no variable: refused before anything is sent.
    fails(
        &wb(&e, &["request", "send", "CreateOrder 1"]),
        2,
        "WASHBOARD_PASSWORD",
    );
    let o = cmd(&["-C", path(&e.project), "request", "send", "CreateOrder 1"])
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
    let shown = ok(wb(
        &e,
        &["request", "history", "CreateOrder 1", "--show", "1"],
    ));
    assert!(shown.contains("authorization: Basic ••••••••\n"), "{shown}");
    assert!(!shown.contains("YWxpY2U6c2VjcmV0"), "{shown}");
    // CreateOrder has no soapAction: sent as an empty quoted string.
    assert!(
        s.head.to_ascii_lowercase().contains("soapaction: \"\""),
        "{}",
        s.head
    );
}

#[test]
fn skip_validation_sends_broken_requests_anyway() {
    let e = customer();
    let (url, seen) = server::serve(vec![("500 Internal Server Error", FAULT_RESPONSE)]);
    ok(wb(&e, &["server", "add", "Local", &url]));
    ok(wb(&e, &["operation", "template", "GetCustomer", "--save"]));
    let req = "GetCustomer 1";
    fill_from_fixture(&e, req, "invalid-not-well-formed.xml");

    fails(
        &wb(&e, &["request", "send", req]),
        1,
        "--skip-validation sends anyway",
    );
    let o = wb(&e, &["request", "send", req, "--skip-validation"]);
    assert_eq!(ok(o.clone()), FAULT_RESPONSE);
    assert!(stderr(&o).contains("not well-formed"), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("sending despite 1 error"),
        "{}",
        stderr(&o)
    );
    let s = seen.recv().expect("request seen");
    let sent = fs::read_to_string(request_file(&e, req)).expect("file");
    assert_eq!(s.body, sent);
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
        &["request", "send", "GetCustomer 1"],
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
    let src = path(&e.src);
    let text = ok(run(&["inspect", path(&wsdl), "--xsd-dir", src]));
    assert!(text.contains("files supplied"), "{text}");
    for secret in ["urn:example", "Customer", "party.xsd"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let json: Value = serde_json::from_str(&ok(run(&[
        "inspect",
        path(&wsdl),
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
