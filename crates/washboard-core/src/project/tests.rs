//! Project tests. Everything runs in temp dirs; WSDL inputs come from `fixtures/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::http::{Exchange, RawMessage, TlsInfo};
use crate::model::{Auth, HistoryId, OperationRef, QName, Server};
use crate::secrets::{MemorySecretStore, SecretKey, SecretStore};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

const CUSTOMER_FILES: &[&str] = &[
    "CustomerService.wsdl",
    "CustomerBinding.wsdl",
    "xsd/customer.xsd",
    "xsd/common/party.xsd",
    "xsd/common/party-ids.xsd",
    "xsd/ext/audit.xsd",
];

fn customer_set() -> WsdlSet {
    let base = fixtures().join("customer");
    WsdlSet {
        files: CUSTOMER_FILES
            .iter()
            .map(|d| WsdlFile {
                source: join_rel(&base, d),
                dest: (*d).to_owned(),
            })
            .collect(),
        entry: "CustomerService.wsdl".into(),
    }
}

fn legacy_set() -> WsdlSet {
    WsdlSet {
        files: vec![WsdlFile {
            source: fixtures().join("legacy-rpc/Legacy.wsdl"),
            dest: "Legacy.wsdl".into(),
        }],
        entry: "Legacy.wsdl".into(),
    }
}

fn new_project() -> (TempDir, PathBuf, Project) {
    let tmp = TempDir::new().expect("tempdir");
    let folder = tmp.path().join("Customer API");
    let p = Project::create(&folder, "Customer API", &customer_set()).expect("create");
    (tmp, folder, p)
}

fn op(name: &str) -> OperationRef {
    OperationRef {
        binding: QName::new("urn:example:customer", "CustomerBinding"),
        operation: name.into(),
    }
}

fn server(name: &str) -> Server {
    Server {
        id: ServerId::new(),
        name: name.into(),
        url: format!("https://{name}.example/soap"),
        ignore_tls_errors: false,
        auth: Auth::Basic {
            username: "alice".into(),
        },
        timeout: Duration::from_secs(30),
    }
}

fn exchange(at_secs: u64, status: Option<&str>) -> Exchange {
    Exchange {
        started_at: UNIX_EPOCH + Duration::from_secs(at_secs),
        duration: Duration::from_millis(123),
        request: RawMessage {
            start_line: "POST /soap HTTP/1.1".into(),
            headers: vec![("Content-Type".into(), "text/xml; charset=utf-8".into())],
            body: format!("<req n='{at_secs}'/>").into_bytes(),
        },
        response: status.map(|s| RawMessage {
            start_line: s.into(),
            headers: vec![
                ("Content-Type".into(), "text/xml".into()),
                ("X-Dup".into(), "1".into()),
                ("X-Dup".into(), "2".into()),
            ],
            body: format!("<resp n='{at_secs}'/>").into_bytes(),
        }),
        tls: TlsInfo::default(),
        error: status.is_none().then(|| "connection refused".to_owned()),
    }
}

fn names(p: &Project) -> Vec<String> {
    p.requests()
        .expect("requests")
        .into_iter()
        .map(|r| r.name)
        .collect()
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// ---- create / open / lock ----------------------------------------------------------------

#[test]
fn create_lays_out_folder_and_copies_byte_exact() {
    let (_tmp, folder, p) = new_project();
    for d in [WSDL_DIR, REQUESTS_DIR, HISTORY_DIR] {
        assert!(folder.join(d).is_dir(), "{d}");
    }
    assert!(folder.join(DB_FILE).is_file());
    for d in CUSTOMER_FILES {
        let src = fs::read(join_rel(&fixtures().join("customer"), d)).expect("src");
        let dst = fs::read(join_rel(&folder.join(WSDL_DIR), d)).expect("dst");
        assert_eq!(src, dst, "{d}");
    }
    // The fixture's BOM survived the copy.
    let entry = fs::read(p.entry_wsdl().expect("entry")).expect("read");
    assert!(entry.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert_eq!(p.wsdl_path().expect("path"), "wsdl/CustomerService.wsdl");
    assert_eq!(p.name().expect("name"), "Customer API");
    assert_eq!(p.history_limit().expect("limit"), DEFAULT_HISTORY_LIMIT);
    let age = SystemTime::now()
        .duration_since(p.wsdl_imported_at().expect("time"))
        .expect("past");
    assert!(age < Duration::from_secs(60));
}

#[test]
fn database_settings() {
    let (_tmp, _folder, p) = new_project();
    let version: i32 = p
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, SCHEMA_VERSION);
    let mode: String = p
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("mode");
    assert_eq!(mode, "delete");
    let fk: bool = p
        .conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .expect("fk");
    assert!(fk);
}

#[test]
fn reopen_keeps_identity() {
    let (_tmp, folder, p) = new_project();
    let id = p.id();
    drop(p);
    let p = Project::open(&folder).expect("open");
    assert_eq!(p.id(), id);
    assert_eq!(p.reconciliation(), &Reconciliation::default());
}

#[test]
fn second_open_is_refused_until_first_closes() {
    let (_tmp, folder, p) = new_project();
    match Project::open(&folder) {
        Err(ProjectError::AlreadyOpen(path)) => assert_eq!(path, folder),
        other => panic!("expected AlreadyOpen, got {other:?}"),
    }
    drop(p);
    Project::open(&folder).expect("open after close");
}

#[test]
fn read_only_open_works_while_locked_and_cannot_write() {
    let (_tmp, folder, mut p) = new_project();
    let r = p
        .create_request(&op("GetCustomer"), "<a/>")
        .expect("create");
    let ro = Project::open_read_only(&folder).expect("read-only open while locked");
    assert_eq!(ro.id(), p.id());
    assert_eq!(ro.name().expect("name"), "Customer API");
    assert_eq!(ro.requests().expect("requests"), vec![r.clone()]);
    assert_eq!(ro.read_request(r.id).expect("read"), "<a/>");
    // Sees the writer's later changes.
    p.rename_request(r.id, "Renamed").expect("rename");
    assert_eq!(ro.request(r.id).expect("request").name, "Renamed");
    // Writes fail, and the read-only handle does not hold the lock.
    let mut ro = ro;
    assert!(matches!(ro.set_name("x"), Err(ProjectError::Db(_))));
    drop(p);
    Project::open(&folder).expect("open while a read-only handle exists");
}

#[test]
fn read_only_open_refuses_non_projects_and_creates_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    match Project::open_read_only(tmp.path()) {
        Err(ProjectError::NotAProject(_)) => {}
        other => panic!("expected NotAProject, got {other:?}"),
    }
    assert_eq!(fs::read_dir(tmp.path()).expect("read_dir").count(), 0);
}

#[test]
fn create_refuses_non_empty_folder_and_bad_names() {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join("x"), "x").expect("write");
    assert!(matches!(
        Project::create(tmp.path(), "P", &customer_set()),
        Err(ProjectError::FolderNotEmpty(_))
    ));
    assert!(matches!(
        Project::create(&tmp.path().join("new"), "  ", &customer_set()),
        Err(ProjectError::EmptyProjectName)
    ));
}

#[test]
fn create_into_existing_empty_folder() {
    let tmp = TempDir::new().expect("tempdir");
    Project::create(tmp.path(), "P", &legacy_set()).expect("create");
}

#[test]
fn failed_create_cleans_up() {
    let tmp = TempDir::new().expect("tempdir");
    let folder = tmp.path().join("P");
    let mut set = customer_set();
    set.files.push(WsdlFile {
        source: tmp.path().join("missing.xsd"),
        dest: "missing.xsd".into(),
    });
    assert!(matches!(
        Project::create(&folder, "P", &set),
        Err(ProjectError::Io { .. })
    ));
    assert!(!folder.exists());

    // A pre-existing empty folder is left empty, not removed.
    fs::create_dir(&folder).expect("mkdir");
    assert!(Project::create(&folder, "P", &set).is_err());
    assert!(folder.is_dir());
    assert!(dir_names(&folder).is_empty());
}

#[test]
fn invalid_wsdl_sets_are_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let src = fixtures().join("legacy-rpc/Legacy.wsdl");
    let file = |dest: &str| WsdlFile {
        source: src.clone(),
        dest: dest.into(),
    };
    let cases = [
        WsdlSet {
            files: vec![],
            entry: "a.wsdl".into(),
        },
        WsdlSet {
            files: vec![file("../a.wsdl")],
            entry: "../a.wsdl".into(),
        },
        WsdlSet {
            files: vec![file("/abs.wsdl")],
            entry: "/abs.wsdl".into(),
        },
        WsdlSet {
            files: vec![file(".previous/a.wsdl")],
            entry: ".previous/a.wsdl".into(),
        },
        WsdlSet {
            files: vec![file("a//b.wsdl")],
            entry: "a//b.wsdl".into(),
        },
        WsdlSet {
            files: vec![file("a.wsdl"), file("A.WSDL")],
            entry: "a.wsdl".into(),
        },
        WsdlSet {
            files: vec![file("a.wsdl")],
            entry: "b.wsdl".into(),
        },
    ];
    for (i, set) in cases.iter().enumerate() {
        let folder = tmp.path().join(format!("p{i}"));
        assert!(
            matches!(
                Project::create(&folder, "P", set),
                Err(ProjectError::InvalidWsdlSet(_))
            ),
            "case {i}"
        );
        assert!(!folder.exists(), "case {i}");
    }
}

#[test]
fn open_rejects_non_projects_without_leaving_files() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(matches!(
        Project::open(tmp.path()),
        Err(ProjectError::NotAProject(_))
    ));
    assert!(dir_names(tmp.path()).is_empty());

    // An empty file named washboard.sqlite is not a project either.
    fs::write(tmp.path().join(DB_FILE), b"").expect("write");
    assert!(matches!(
        Project::open(tmp.path()),
        Err(ProjectError::NotAProject(_))
    ));

    // Garbage in the database file is an error, not a panic.
    fs::write(tmp.path().join(DB_FILE), b"definitely not sqlite, but long enough to look like a header....................................").expect("write");
    assert!(Project::open(tmp.path()).is_err());
}

#[test]
fn newer_database_version_is_refused() {
    let (_tmp, folder, p) = new_project();
    drop(p);
    let conn = rusqlite::Connection::open(folder.join(DB_FILE)).expect("conn");
    conn.pragma_update(None, "user_version", 99).expect("bump");
    drop(conn);
    assert!(matches!(
        Project::open(&folder),
        Err(ProjectError::UnsupportedVersion {
            found: 99,
            supported: SCHEMA_VERSION
        })
    ));
}

#[test]
fn missing_folders_are_recreated_on_open() {
    // git does not track empty folders.
    let (_tmp, folder, p) = new_project();
    drop(p);
    fs::remove_dir(folder.join(REQUESTS_DIR)).expect("rm");
    fs::remove_dir(folder.join(HISTORY_DIR)).expect("rm");
    let p = Project::open(&folder).expect("open");
    assert!(folder.join(REQUESTS_DIR).is_dir());
    assert!(names(&p).is_empty());
}

#[test]
fn bad_database_contents_are_errors_not_panics() {
    let (_tmp, _folder, mut p) = new_project();
    let r = p
        .create_request(&op("GetCustomer"), "<a/>")
        .expect("create");
    p.conn
        .execute(
            "UPDATE request SET created_at = 'yesterday' WHERE id = ?1",
            params![r.id.to_string()],
        )
        .expect("corrupt");
    assert!(matches!(p.requests(), Err(ProjectError::Corrupt(_))));
    p.conn
        .execute(
            "UPDATE request SET created_at = '2026-01-01T00:00:00Z', id = 'nope'",
            [],
        )
        .expect("corrupt");
    assert!(matches!(p.requests(), Err(ProjectError::Corrupt(_))));
    p.conn
        .execute(
            "UPDATE request SET operation = 'garbage', id = ?1",
            params![r.id.to_string()],
        )
        .expect("corrupt");
    // A stale/garbled operation hint is just no hint.
    assert_eq!(p.request(r.id).expect("request").operation, None);
    p.conn
        .execute("UPDATE project SET history_limit = -5", [])
        .expect("corrupt");
    assert_eq!(p.history_limit().expect("limit"), DEFAULT_HISTORY_LIMIT);
}

// ---- reconciliation ----------------------------------------------------------------------

#[test]
fn reconciliation_on_open() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let b = p.create_request(&op("GetCustomer"), "<b/>").expect("b");
    drop(p);

    let req = folder.join(REQUESTS_DIR);
    fs::remove_file(req.join("GetCustomer 1.xml")).expect("rm");
    fs::write(req.join("From Git.xml"), "<g/>").expect("write");
    fs::write(req.join("Upper.XML"), "<u/>").expect("write");
    fs::write(req.join(".hidden.xml"), "<h/>").expect("write");
    fs::write(req.join(".wb-0123.tmp"), "<t/>").expect("write");
    fs::write(req.join("notes.txt"), "n").expect("write");
    fs::create_dir(req.join("dir.xml")).expect("mkdir");

    let p = Project::open(&folder).expect("open");
    let rec = p.reconciliation().clone();
    assert_eq!(rec.removed, vec![a.id]);
    assert_eq!(rec.added.len(), 2);
    assert_eq!(names(&p), ["GetCustomer 2", "From Git", "Upper"]);
    assert_eq!(p.request(b.id).expect("b").name, "GetCustomer 2");
    let g = p.requests().expect("list")[1].clone();
    assert!(rec.added.contains(&g.id));
    assert_eq!(g.operation, None);
    assert_eq!(p.read_request(g.id).expect("read"), "<g/>");
    assert!(matches!(
        p.request(a.id),
        Err(ProjectError::UnknownRequest(_))
    ));
}

#[test]
fn reconcile_keeps_identity_across_case_only_rename() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let entry = p
        .record_exchange(
            a.id,
            &server("dev"),
            &exchange(1000, Some("HTTP/1.1 200 OK")),
            false,
        )
        .expect("record");
    let req = folder.join(REQUESTS_DIR);
    fs::rename(req.join("GetCustomer 1.xml"), req.join("getcustomer 1.xml")).expect("mv");
    let rec = p.reconcile().expect("reconcile");
    assert_eq!(rec, Reconciliation::default());
    assert_eq!(p.request(a.id).expect("a").name, "getcustomer 1");
    assert_eq!(p.history(a.id).expect("history")[0].id, entry.id);
}

#[test]
fn reconcile_while_open_and_orphan_history_pruning() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    p.record_exchange(
        a.id,
        &server("dev"),
        &exchange(1000, Some("HTTP/1.1 200 OK")),
        false,
    )
    .expect("record");
    let hist = folder.join(HISTORY_DIR).join(a.id.to_string());
    assert!(hist.is_dir());
    // A stray file from an interrupted write, and a folder that is not ours.
    fs::write(hist.join("19700101T000000Z.response.xml"), "x").expect("write");
    fs::create_dir(folder.join(HISTORY_DIR).join("keep-me")).expect("mkdir");
    assert_eq!(p.prune_orphan_history().expect("prune"), 1);
    assert_eq!(dir_names(&hist).len(), 2);

    fs::remove_file(folder.join(REQUESTS_DIR).join("GetCustomer 1.xml")).expect("rm");
    let rec = p.reconcile().expect("reconcile");
    assert_eq!(rec.removed, vec![a.id]);
    // History files stay until pruned.
    assert!(hist.is_dir());
    assert_eq!(p.prune_orphan_history().expect("prune"), 1);
    assert!(!hist.exists());
    assert!(folder.join(HISTORY_DIR).join("keep-me").is_dir());
}

// ---- requests ----------------------------------------------------------------------------

#[test]
fn auto_names_use_lowest_free_number() {
    let (_tmp, _folder, mut p) = new_project();
    let r1 = p.create_request(&op("GetCustomer"), "<a/>").expect("1");
    let r2 = p.create_request(&op("GetCustomer"), "<a/>").expect("2");
    p.create_request(&op("GetCustomer"), "<a/>").expect("3");
    assert_eq!(r1.name, "GetCustomer 1");
    assert_eq!(r2.name, "GetCustomer 2");
    assert_eq!(r1.operation, Some(op("GetCustomer")));
    p.delete_request(r2.id).expect("delete");
    let again = p.create_request(&op("GetCustomer"), "<a/>").expect("again");
    assert_eq!(again.name, "GetCustomer 2");
    // A file created behind our back also counts as taken.
    fs::write(p.root().join(REQUESTS_DIR).join("CreateOrder 1.xml"), "x").expect("write");
    assert_eq!(
        p.create_request(&op("CreateOrder"), "<a/>")
            .expect("co")
            .name,
        "CreateOrder 2"
    );
}

#[test]
fn create_named_validates() {
    let (_tmp, _folder, mut p) = new_project();
    p.create_request_named("Mine", None, "<a/>").expect("mine");
    for (bad, reason) in [
        ("", NameError::Empty),
        ("a/b", NameError::Slash),
        ("a:b", NameError::Colon),
        (".x", NameError::LeadingDot),
    ] {
        match p.create_request_named(bad, None, "<a/>") {
            Err(ProjectError::InvalidName { reason: r, .. }) => assert_eq!(r, reason),
            other => panic!("{bad:?}: {other:?}"),
        }
    }
    assert!(matches!(
        p.create_request_named("MINE", None, "<a/>"),
        Err(ProjectError::NameTaken(_))
    ));
    assert_eq!(names(&p), ["Mine"]);
}

#[test]
fn rename_moves_the_file() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let b = p.create_request(&op("GetCustomer"), "<b/>").expect("b");
    let req = folder.join(REQUESTS_DIR);

    let r = p.rename_request(a.id, "Lookup Alice").expect("rename");
    assert_eq!(r.name, "Lookup Alice");
    assert!(req.join("Lookup Alice.xml").is_file());
    assert!(!req.join("GetCustomer 1.xml").exists());
    assert_eq!(p.read_request(a.id).expect("read"), "<a/>");

    assert!(matches!(
        p.rename_request(a.id, "getcustomer 2"),
        Err(ProjectError::NameTaken(_))
    ));
    assert!(matches!(
        p.rename_request(a.id, "x/y"),
        Err(ProjectError::InvalidName { .. })
    ));
    // Case-only change of its own name is fine.
    assert_eq!(
        p.rename_request(b.id, "getcustomer 2").expect("case").name,
        "getcustomer 2"
    );
    assert_eq!(p.read_request(b.id).expect("read"), "<b/>");
    assert!(matches!(
        p.rename_request(RequestId::new(), "z"),
        Err(ProjectError::UnknownRequest(_))
    ));
}

#[test]
fn duplicate_copies_bytes_and_places_after_original() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    p.create_request(&op("CreateOrder"), "<c/>").expect("c");
    let with_bom = [&[0xEF, 0xBB, 0xBF][..], b"<a>\xC3\xBC</a>"].concat();
    fs::write(
        folder.join(REQUESTS_DIR).join("GetCustomer 1.xml"),
        &with_bom,
    )
    .expect("write");

    let d1 = p.duplicate_request(a.id).expect("dup");
    let d2 = p.duplicate_request(a.id).expect("dup");
    assert_eq!(d1.name, "GetCustomer 1 copy");
    assert_eq!(d2.name, "GetCustomer 1 copy 2");
    assert_eq!(d1.operation, a.operation);
    assert_eq!(
        names(&p),
        [
            "GetCustomer 1",
            "GetCustomer 1 copy 2",
            "GetCustomer 1 copy",
            "CreateOrder 1"
        ]
    );
    assert_eq!(
        fs::read(p.request_path(d1.id).expect("path")).expect("read"),
        with_bom
    );
    let d3 = p.duplicate_request(d1.id).expect("dup of dup");
    assert_eq!(d3.name, "GetCustomer 1 copy copy");
}

#[test]
fn delete_removes_file_rows_and_history() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    p.record_exchange(
        a.id,
        &server("dev"),
        &exchange(1000, Some("HTTP/1.1 200 OK")),
        false,
    )
    .expect("record");
    p.delete_request(a.id).expect("delete");
    assert!(names(&p).is_empty());
    assert!(dir_names(&folder.join(REQUESTS_DIR)).is_empty());
    assert!(dir_names(&folder.join(HISTORY_DIR)).is_empty());
    let n: i64 = p
        .conn
        .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 0);
    assert!(matches!(
        p.delete_request(a.id),
        Err(ProjectError::UnknownRequest(_))
    ));
}

#[test]
fn read_and_write_preserve_bom_and_are_atomic() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let path = p.request_path(a.id).expect("path");
    assert!(!fs::read(&path).expect("read").starts_with(&[0xEF]));

    p.write_request(a.id, "<b>ü</b>").expect("write");
    assert_eq!(fs::read(&path).expect("read"), "<b>ü</b>".as_bytes());

    fs::write(&path, b"\xEF\xBB\xBF<c/>").expect("write");
    assert_eq!(p.read_request(a.id).expect("read"), "<c/>");
    p.write_request(a.id, "<d/>").expect("write");
    assert_eq!(fs::read(&path).expect("read"), b"\xEF\xBB\xBF<d/>");

    // UTF-16 is decoded for the editor.
    let mut utf16 = vec![0xFF, 0xFE];
    for u in "<e>ü</e>".encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    fs::write(&path, &utf16).expect("write");
    assert_eq!(p.read_request(a.id).expect("read"), "<e>ü</e>");

    fs::write(&path, b"\xFF\xFF\xFF").expect("write");
    assert!(matches!(
        p.read_request(a.id),
        Err(ProjectError::Decode { .. })
    ));

    // No temp files left behind.
    assert_eq!(dir_names(&folder.join(REQUESTS_DIR)), ["GetCustomer 1.xml"]);
}

#[test]
fn request_order() {
    let (_tmp, _folder, mut p) = new_project();
    let a = p.create_request_named("A", None, "").expect("a");
    let b = p.create_request_named("B", None, "").expect("b");
    let c = p.create_request_named("C", None, "").expect("c");
    p.set_request_order(&[c.id, a.id, RequestId::new(), c.id])
        .expect("order");
    assert_eq!(names(&p), ["C", "A", "B"]);
    p.set_request_order(&[b.id]).expect("order");
    assert_eq!(names(&p), ["B", "C", "A"]);
}

// ---- servers -----------------------------------------------------------------------------

#[test]
fn server_crud_and_order() {
    let (_tmp, folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let dev = server("dev");
    let mut prod = server("prod");
    p.add_server(&dev).expect("add");
    p.add_server(&prod).expect("add");
    assert_eq!(p.servers().expect("list"), [dev.clone(), prod.clone()]);

    prod.url = "https://prod2.example/soap".into();
    prod.ignore_tls_errors = true;
    prod.timeout = Duration::from_millis(1500);
    p.update_server(&prod, &secrets).expect("update");
    let got = p.server(prod.id).expect("get");
    assert_eq!(got.url, prod.url);
    assert!(got.ignore_tls_errors);
    assert_eq!(got.timeout, Duration::from_secs(2));

    p.set_server_order(&[prod.id]).expect("order");
    let ids: Vec<ServerId> = p.servers().expect("list").iter().map(|s| s.id).collect();
    assert_eq!(ids, [prod.id, dev.id]);

    assert!(matches!(
        p.update_server(&server("ghost"), &secrets),
        Err(ProjectError::UnknownServer(_))
    ));
    drop(p);
    let p = Project::open(&folder).expect("reopen");
    assert_eq!(p.servers().expect("list").len(), 2);
}

#[test]
fn passwords_go_through_the_secret_store() {
    let (_tmp, _folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let mut dev = server("dev");
    p.add_server(&dev).expect("add");
    p.set_server_password(dev.id, Some("s3cret"), &secrets)
        .expect("set");
    assert_eq!(
        p.server_password(dev.id, &secrets).expect("get").as_deref(),
        Some("s3cret")
    );
    let key = SecretKey {
        project: p.id(),
        server: dev.id,
    };
    assert_eq!(secrets.get(&key).expect("get").as_deref(), Some("s3cret"));
    // Nothing secret in the database.
    let dump: Vec<String> = p
        .conn
        .prepare("SELECT name || url || COALESCE(username, '') FROM server")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert!(dump.iter().all(|s| !s.contains("s3cret")));

    // Switching to no auth drops the password.
    dev.auth = Auth::None;
    p.update_server(&dev, &secrets).expect("update");
    assert_eq!(secrets.get(&key).expect("get"), None);

    p.set_server_password(dev.id, Some("again"), &secrets)
        .expect("set");
    p.set_server_password(dev.id, None, &secrets)
        .expect("clear");
    assert_eq!(secrets.get(&key).expect("get"), None);
    assert!(matches!(
        p.set_server_password(ServerId::new(), Some("x"), &secrets),
        Err(ProjectError::UnknownServer(_))
    ));
}

#[test]
fn deleting_a_server_clears_last_server_and_secret() {
    let (_tmp, _folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let dev = server("dev");
    let prod = server("prod");
    p.add_server(&dev).expect("add");
    p.add_server(&prod).expect("add");
    p.set_server_password(dev.id, Some("pw"), &secrets)
        .expect("pw");

    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    assert_eq!(a.last_server, None);
    p.set_request_last_server(a.id, Some(dev.id)).expect("last");
    assert_eq!(p.last_used_server().expect("last"), Some(dev.id));
    // New requests start with the most recently used server.
    let b = p.create_request(&op("GetCustomer"), "<b/>").expect("b");
    assert_eq!(b.last_server, Some(dev.id));

    p.delete_server(dev.id, &secrets).expect("delete");
    assert_eq!(p.request(a.id).expect("a").last_server, None);
    assert_eq!(p.request(b.id).expect("b").last_server, None);
    assert_eq!(p.last_used_server().expect("last"), None);
    assert_eq!(secrets.get(&p.secret_key(dev.id)).expect("get"), None);
    assert_eq!(p.servers().expect("list"), [prod]);
    assert!(matches!(
        p.delete_server(dev.id, &secrets),
        Err(ProjectError::UnknownServer(_))
    ));
}

// ---- history -----------------------------------------------------------------------------

#[test]
fn record_list_load_history() {
    let (_tmp, folder, mut p) = new_project();
    let dev = server("dev");
    p.add_server(&dev).expect("add");
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");

    let ok = p
        .record_exchange(
            a.id,
            &dev,
            &exchange(1_791_295_392, Some("HTTP/1.1 500 Internal")),
            true,
        )
        .expect("record");
    let failed = p
        .record_exchange(a.id, &dev, &exchange(1_791_295_400, None), false)
        .expect("record");
    assert_eq!(ok.http_status, Some(500));
    assert!(ok.soap_fault);
    assert_eq!(ok.duration, Some(Duration::from_millis(123)));
    assert_eq!(failed.error.as_deref(), Some("connection refused"));
    assert_eq!(p.request(a.id).expect("a").last_server, Some(dev.id));
    assert_eq!(p.last_used_server().expect("last"), Some(dev.id));

    let list = p.history(a.id).expect("history");
    assert_eq!(list, [failed.clone(), ok.clone()]);

    let dir = folder.join(HISTORY_DIR).join(a.id.to_string());
    assert_eq!(
        dir_names(&dir),
        [
            "20261006T140312Z.request.xml",
            "20261006T140312Z.response.xml",
            "20261006T140320Z.request.xml"
        ]
    );
    let rec = p.load_history(ok.id).expect("load");
    assert_eq!(rec.entry, ok);
    assert_eq!(rec.request_body, b"<req n='1791295392'/>");
    assert_eq!(
        rec.response_body.as_deref(),
        Some(&b"<resp n='1791295392'/>"[..])
    );
    assert_eq!(rec.response_headers.len(), 3);
    assert_eq!(
        rec.response_headers[2],
        ("X-Dup".to_owned(), "2".to_owned())
    );
    let rec = p.load_history(failed.id).expect("load");
    assert_eq!(rec.response_body, None);
    assert!(rec.response_headers.is_empty());
    assert!(matches!(
        p.load_history(HistoryId::new()),
        Err(ProjectError::UnknownHistory(_))
    ));
}

#[test]
fn same_second_sends_get_distinct_files() {
    let (_tmp, _folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let e1 = p
        .record_exchange(
            a.id,
            &server("dev"),
            &exchange(1000, Some("HTTP/1.1 200 OK")),
            false,
        )
        .expect("1");
    let e2 = p
        .record_exchange(
            a.id,
            &server("dev"),
            &exchange(1000, Some("HTTP/1.1 200 OK")),
            false,
        )
        .expect("2");
    assert_ne!(e1.id, e2.id);
    assert_eq!(p.history(a.id).expect("history").len(), 2);
    assert_eq!(p.load_history(e2.id).expect("load").entry, e2);
    // An unknown server (not in the project) is recorded but does not become "last server".
    assert_eq!(p.request(a.id).expect("a").last_server, None);
}

#[test]
fn history_is_pruned_to_the_limit() {
    let (_tmp, folder, mut p) = new_project();
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let b = p.create_request(&op("GetCustomer"), "<b/>").expect("b");
    p.set_history_limit(3).expect("limit");
    let dev = server("dev");
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(
            p.record_exchange(
                a.id,
                &dev,
                &exchange(1000 + i, Some("HTTP/1.1 200 OK")),
                false,
            )
            .expect("record")
            .id,
        );
    }
    p.record_exchange(b.id, &dev, &exchange(5000, Some("HTTP/1.1 200 OK")), false)
        .expect("record");
    let kept: Vec<HistoryId> = p.history(a.id).expect("h").iter().map(|e| e.id).collect();
    assert_eq!(kept, [ids[4], ids[3], ids[2]]);
    assert_eq!(
        dir_names(&folder.join(HISTORY_DIR).join(a.id.to_string())).len(),
        6
    );

    p.set_history_limit(1).expect("limit");
    assert_eq!(p.history_limit().expect("limit"), 1);
    let kept: Vec<HistoryId> = p.history(a.id).expect("h").iter().map(|e| e.id).collect();
    assert_eq!(kept, [ids[4]]);
    assert_eq!(
        dir_names(&folder.join(HISTORY_DIR).join(a.id.to_string())).len(),
        2
    );
    assert_eq!(p.history(b.id).expect("h").len(), 1);

    // History follows the request through a rename.
    p.rename_request(a.id, "Renamed").expect("rename");
    assert_eq!(p.history(a.id).expect("h").len(), 1);
}

// ---- WSDL replacement --------------------------------------------------------------------

#[test]
fn replace_wsdl_keeps_one_previous_set() {
    let (_tmp, folder, mut p) = new_project();
    let wsdl = folder.join(WSDL_DIR);
    let r = p.create_request(&op("GetCustomer"), "<a/>").expect("a");

    p.replace_wsdl(&legacy_set()).expect("replace");
    assert_eq!(p.wsdl_path().expect("path"), "wsdl/Legacy.wsdl");
    assert_eq!(dir_names(&wsdl), [".previous", "Legacy.wsdl"]);
    let prev = dir_names(&wsdl.join(PREVIOUS_WSDL_DIR));
    assert_eq!(prev.len(), 1);
    let first_backup = wsdl.join(PREVIOUS_WSDL_DIR).join(&prev[0]);
    assert_eq!(
        dir_names(&first_backup),
        ["CustomerBinding.wsdl", "CustomerService.wsdl", "xsd"]
    );
    assert!(first_backup.join("xsd/common/party.xsd").is_file());

    p.replace_wsdl(&customer_set()).expect("replace back");
    assert_eq!(p.wsdl_path().expect("path"), "wsdl/CustomerService.wsdl");
    let prev = dir_names(&wsdl.join(PREVIOUS_WSDL_DIR));
    assert_eq!(prev.len(), 1);
    assert_eq!(
        dir_names(&wsdl.join(PREVIOUS_WSDL_DIR).join(&prev[0])),
        ["Legacy.wsdl"]
    );
    assert_eq!(
        fs::read(wsdl.join("xsd/common/party.xsd")).expect("read"),
        fs::read(fixtures().join("customer/xsd/common/party.xsd")).expect("read")
    );
    // Requests are untouched; no staging folders left.
    assert_eq!(p.read_request(r.id).expect("read"), "<a/>");
    assert!(dir_names(&folder).iter().all(|n| !n.starts_with(".wb-")));
}

#[test]
fn failed_replace_leaves_wsdl_untouched() {
    let (tmp, folder, mut p) = new_project();
    let before = dir_names(&folder.join(WSDL_DIR));
    let set = WsdlSet {
        files: vec![WsdlFile {
            source: tmp.path().join("missing.wsdl"),
            dest: "x.wsdl".into(),
        }],
        entry: "x.wsdl".into(),
    };
    assert!(p.replace_wsdl(&set).is_err());
    assert_eq!(dir_names(&folder.join(WSDL_DIR)), before);
    assert_eq!(p.wsdl_path().expect("path"), "wsdl/CustomerService.wsdl");
    assert!(dir_names(&folder).iter().all(|n| !n.starts_with(".wb-")));
}

// ---- ui_state ----------------------------------------------------------------------------

#[test]
fn ui_state_round_trip() {
    let (_tmp, folder, mut p) = new_project();
    assert_eq!(p.ui_state("split").expect("get"), None);
    p.set_ui_state("split", Some("0.3")).expect("set");
    p.set_ui_state("split", Some("0.4")).expect("set");
    p.set_ui_state("tab", Some("headers")).expect("set");
    p.set_ui_state("tab", None).expect("delete");
    drop(p);
    let p = Project::open(&folder).expect("open");
    assert_eq!(p.ui_state("split").expect("get").as_deref(), Some("0.4"));
    assert_eq!(p.ui_state("tab").expect("get"), None);
}

// ---- app state ---------------------------------------------------------------------------

#[test]
fn app_state_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("Application Support/Washboard");
    assert_eq!(AppState::load(&dir).expect("load"), AppState::default());

    let mut state = AppState::default();
    state.open_projects.push(OpenProject {
        path: "/Users/me/Customer API".into(),
        bookmark: Some(vec![0, 1, 255]),
        window_autosave_name: Some("project-1".into()),
        last_selected_request: Some(RequestId::new()),
    });
    state
        .open_projects
        .push(OpenProject::new("/Users/me/Other"));
    for i in 0..12 {
        state.note_recent(Path::new(&format!("/p{i}")));
    }
    state.note_recent(Path::new("/p5"));
    assert_eq!(state.recent_projects.len(), MAX_RECENT);
    assert_eq!(state.recent_projects[0], Path::new("/p5"));
    assert_eq!(state.recent_projects[1], Path::new("/p11"));

    state.save(&dir).expect("save");
    assert_eq!(AppState::load(&dir).expect("load"), state);
    assert_eq!(dir_names(&dir), [STATE_FILE]);
}

#[test]
fn app_state_tolerates_unknown_fields_and_rejects_garbage() {
    let tmp = TempDir::new().expect("tempdir");
    let file = tmp.path().join(STATE_FILE);
    fs::write(
        &file,
        r#"{"version": 7, "future": true,
            "open_projects": [{"path": "/x", "last_selected_request": "not-a-uuid", "new": 1}]}"#,
    )
    .expect("write");
    let s = AppState::load(tmp.path()).expect("load");
    assert_eq!(s.open_projects, [OpenProject::new("/x")]);
    assert!(s.recent_projects.is_empty());

    fs::write(&file, "{not json").expect("write");
    assert!(matches!(
        AppState::load(tmp.path()),
        Err(AppStateError::Invalid { .. })
    ));
}
