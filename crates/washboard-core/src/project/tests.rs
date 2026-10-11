//! Project tests. Everything runs in temp dirs; WSDL inputs come from `fixtures/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::http::{Exchange, RawMessage};
use crate::model::{Auth, HistoryId, OperationRef, QName, Server};
use crate::secrets::{MemorySecretStore, SecretKey, SecretStore};
use crate::test_support::{self, fixtures};

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
    assert!(matches!(
        Project::open_read_only(tmp.path()),
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

// ---- folders -----------------------------------------------------------------------------

fn paths(p: &Project) -> Vec<String> {
    p.requests()
        .expect("requests")
        .into_iter()
        .map(|r| names::join(&r.folder, &r.name))
        .collect()
}

/// A WSDL outside `fixtures/`, with the files next to it.
fn load_wsdl(path: &Path) -> crate::wsdl::Wsdl {
    crate::wsdl::load_from_disk(path, &[path.parent().expect("dir").to_owned()])
        .expect("wsdl")
        .wsdl
}

/// The operations of `wsdl`'s binding named `binding`.
fn op_in(wsdl: &crate::wsdl::Wsdl, binding: &str, operation: &str) -> OperationRef {
    let b = wsdl
        .definitions
        .bindings
        .iter()
        .find(|b| b.name.local == binding)
        .expect("binding");
    let o = b
        .operations
        .iter()
        .find(|o| o.name == operation)
        .expect("operation");
    b.operation_ref(o)
}

#[test]
fn requests_live_in_nested_folders() {
    let (_tmp, folder, mut p) = new_project();
    let a = p
        .create_request_in("GetCustomer", &op("GetCustomer"), "<a/>")
        .expect("a");
    assert_eq!(a.folder, "GetCustomer");
    assert_eq!(a.name, "GetCustomer 1");
    let req = folder.join(REQUESTS_DIR);
    assert!(req.join("GetCustomer/GetCustomer 1.xml").is_file());
    assert_eq!(
        p.request_path(a.id).expect("path"),
        req.join("GetCustomer").join("GetCustomer 1.xml")
    );
    // An existing folder is reused whatever its case; a deeper one is created.
    let b = p
        .create_request_in("getcustomer", &op("GetCustomer"), "<b/>")
        .expect("b");
    assert_eq!(b.folder, "GetCustomer");
    assert_eq!(b.name, "GetCustomer 2");
    let c = p
        .create_request_named_in("A/B", "Deep", None, "<c/>")
        .expect("c");
    assert_eq!(c.folder, "A/B");
    // Names are unique across folders.
    assert!(matches!(
        p.create_request_named_in("A", "deep", None, "<d/>"),
        Err(ProjectError::NameTaken(_))
    ));
    assert!(matches!(
        p.create_request_named_in("A/.git", "X", None, "<d/>"),
        Err(ProjectError::InvalidName { .. })
    ));
    assert_eq!(p.folders().expect("folders"), ["A", "A/B", "GetCustomer"]);
    // Rename and duplicate stay in the folder.
    let a = p.rename_request(a.id, "First").expect("rename");
    assert_eq!(a.folder, "GetCustomer");
    assert!(req.join("GetCustomer/First.xml").is_file());
    let copy = p.duplicate_request(c.id).expect("duplicate");
    assert_eq!(
        (copy.folder.as_str(), copy.name.as_str()),
        ("A/B", "Deep copy")
    );
    assert_eq!(p.read_request(copy.id).expect("read"), "<c/>");
}

#[test]
fn reconciliation_walks_folders_and_keeps_moves() {
    let (_tmp, folder, mut p) = new_project();
    let a = p
        .create_request_in("Old", &op("GetCustomer"), "<a/>")
        .expect("a");
    let entry = p
        .record_exchange(
            a.id,
            &server("dev"),
            &exchange(1000, Some("HTTP/1.1 200 OK")),
            false,
        )
        .expect("record");
    let b = p.create_request(&op("CreateOrder"), "<b/>").expect("b");
    drop(p);

    let req = folder.join(REQUESTS_DIR);
    // A move in Finder, a file from git in a new folder, hidden things that stay ignored.
    fs::create_dir_all(req.join("New/Deeper")).expect("mkdir");
    fs::rename(
        req.join("Old/GetCustomer 1.xml"),
        req.join("New/Deeper/GetCustomer 1.xml"),
    )
    .expect("mv");
    fs::create_dir(req.join("Git")).expect("mkdir");
    fs::write(req.join("Git/From Git.xml"), "<g/>").expect("write");
    fs::create_dir(req.join(".git")).expect("mkdir");
    fs::write(req.join(".git/Ignored.xml"), "<i/>").expect("write");
    fs::write(req.join("New/.wb-1.tmp"), "<t/>").expect("write");

    let p = Project::open(&folder).expect("open");
    let rec = p.reconciliation().clone();
    assert!(rec.removed.is_empty(), "{rec:?}");
    assert_eq!(rec.added.len(), 1);
    let moved = p.request(a.id).expect("a");
    assert_eq!(moved.folder, "New/Deeper");
    assert_eq!(p.history(a.id).expect("history")[0].id, entry.id);
    assert_eq!(p.request(b.id).expect("b").folder, "");
    assert_eq!(
        paths(&p),
        ["New/Deeper/GetCustomer 1", "CreateOrder 1", "Git/From Git"]
    );
    // Empty folders are kept: the disk is the truth.
    assert_eq!(
        p.folders().expect("folders"),
        ["Git", "New", "New/Deeper", "Old"]
    );
}

#[test]
fn same_name_in_two_folders_is_accepted() {
    let (_tmp, folder, mut p) = new_project();
    let a = p
        .create_request_named_in("One", "Same", None, "<a/>")
        .expect("a");
    let req = folder.join(REQUESTS_DIR);
    fs::create_dir(req.join("Two")).expect("mkdir");
    fs::write(req.join("Two/Same.xml"), "<b/>").expect("write");
    // A move by the user would clash; a removed row with two candidates is not a move.
    let rec = p.reconcile().expect("reconcile");
    assert_eq!(rec.added.len(), 1);
    assert_eq!(paths(&p), ["One/Same", "Two/Same"]);
    assert!(matches!(
        p.move_request(a.id, "Two"),
        Err(ProjectError::NameTaken(_))
    ));
    fs::remove_file(req.join("One/Same.xml")).expect("rm");
    fs::create_dir(req.join("Three")).expect("mkdir");
    fs::write(req.join("Three/Same.xml"), "<c/>").expect("write");
    let rec = p.reconcile().expect("reconcile");
    // Exactly one new file of that name: the move keeps the id.
    assert!(rec.removed.is_empty(), "{rec:?}");
    assert_eq!(p.request(a.id).expect("a").folder, "Three");
}

#[test]
fn placement_follows_the_supported_ports() {
    let customer = test_support::customer();
    let get = op_in(&customer, "CustomerBinding", "GetCustomer");
    // One SOAP 1.1 port next to a SOAP 1.2 one: one level.
    assert_eq!(operation_folder(&customer, &get), "GetCustomer");
    let legacy = test_support::legacy();
    let lookup = op_in(&legacy, "LegacyBinding", "Lookup");
    // rpc/literal next to rpc/encoded: still one level.
    assert_eq!(operation_folder(&legacy, &lookup), "Lookup");

    // The same WSDL with the encoded binding made literal has two supported ports.
    let tmp = TempDir::new().expect("tempdir");
    let text = fs::read_to_string(fixtures().join("legacy-rpc/Legacy.wsdl")).expect("read");
    let two = tmp.path().join("TwoPorts.wsdl");
    fs::write(&two, text.replace("use=\"encoded\"", "use=\"literal\"")).expect("write");
    let two = load_wsdl(&two);
    assert_eq!(
        operation_folder(&two, &op_in(&two, "LegacyBinding", "Lookup")),
        "LegacyPort/Lookup"
    );
    assert_eq!(
        operation_folder(&two, &op_in(&two, "LegacyEncodedBinding", "Lookup")),
        "LegacyEncodedPort/Lookup"
    );
}

#[test]
fn folder_commands() {
    let (_tmp, folder, mut p) = new_project();
    let req = folder.join(REQUESTS_DIR);
    assert_eq!(p.create_untitled_folder("").expect("new"), "New Folder");
    assert_eq!(p.create_untitled_folder("").expect("new"), "New Folder 2");
    assert_eq!(
        p.create_untitled_folder("new folder").expect("nested"),
        "New Folder/New Folder"
    );
    assert!(matches!(
        p.create_folder("NEW FOLDER"),
        Err(ProjectError::FolderExists(_))
    ));
    let a = p
        .create_request_named_in("New Folder/New Folder", "A", None, "<a/>")
        .expect("a");
    p.record_exchange(
        a.id,
        &server("dev"),
        &exchange(1000, Some("HTTP/1.1 200 OK")),
        false,
    )
    .expect("record");

    // Rename: the requests inside follow, ids and history kept.
    assert_eq!(
        p.rename_folder("New Folder", "Orders").expect("rename"),
        "Orders"
    );
    assert_eq!(p.request(a.id).expect("a").folder, "Orders/New Folder");
    assert_eq!(p.history(a.id).expect("history").len(), 1);
    assert!(req.join("Orders/New Folder/A.xml").is_file());
    assert!(matches!(
        p.rename_folder("Orders", "new folder 2"),
        Err(ProjectError::FolderExists(_))
    ));
    assert!(matches!(
        p.rename_folder("Orders", "a/b"),
        Err(ProjectError::InvalidName { .. })
    ));
    assert_eq!(
        p.rename_folder("Orders", "ORDERS").expect("case only"),
        "ORDERS"
    );

    // Move: refused into itself and its descendants, and onto a folder of the same name.
    assert!(matches!(
        p.move_folder("ORDERS", "ORDERS/New Folder"),
        Err(ProjectError::MoveIntoItself(_))
    ));
    assert!(matches!(
        p.move_folder("ORDERS", "ORDERS"),
        Err(ProjectError::MoveIntoItself(_))
    ));
    p.create_folder("New Folder 2/New Folder").expect("create");
    assert!(matches!(
        p.move_folder("ORDERS/New Folder", "New Folder 2"),
        Err(ProjectError::FolderExists(_))
    ));
    assert_eq!(
        p.move_folder("ORDERS/New Folder", "").expect("to the top"),
        "New Folder"
    );
    assert_eq!(p.request(a.id).expect("a").folder, "New Folder");
    assert_eq!(
        p.move_folder("New Folder", "ORDERS").expect("back"),
        "ORDERS/New Folder"
    );

    // A request moves on its own, creating the folder.
    let moved = p.move_request(a.id, "Elsewhere/Deep").expect("move");
    assert_eq!(moved.folder, "Elsewhere/Deep");
    assert!(req.join("Elsewhere/Deep/A.xml").is_file());
    assert_eq!(p.history(a.id).expect("history").len(), 1);
    assert_eq!(p.move_request(a.id, "").expect("top").folder, "");

    // Delete takes the requests and their history with it.
    let b = p
        .create_request_named_in("ORDERS/New Folder", "B", None, "<b/>")
        .expect("b");
    p.record_exchange(
        b.id,
        &server("dev"),
        &exchange(2000, Some("HTTP/1.1 200 OK")),
        false,
    )
    .expect("record");
    assert_eq!(p.delete_folder("ORDERS").expect("delete"), [b.id]);
    assert!(!req.join("ORDERS").exists());
    assert!(!folder.join(HISTORY_DIR).join(b.id.to_string()).exists());
    assert_eq!(paths(&p), ["A"]);
    assert!(matches!(
        p.delete_folder("ORDERS"),
        Err(ProjectError::UnknownFolder(_))
    ));
    assert!(matches!(
        p.delete_folder(""),
        Err(ProjectError::UnknownFolder(_))
    ));
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

/// The `ServerError` that `check_server` (and so add and update) reports for `server`.
fn server_error(p: &Project, server: &Server) -> ServerError {
    match p.check_server(server) {
        Err(ProjectError::InvalidServer(e)) => e,
        other => panic!("{}: {other:?}", server.url),
    }
}

#[test]
fn servers_need_a_free_name_and_a_url_send_accepts() {
    let (_tmp, _folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let mut dev = server("dev");
    p.add_server(&dev).expect("add");

    let blank = Server {
        name: " \t".into(),
        ..server("x")
    };
    assert_eq!(server_error(&p, &blank), ServerError::EmptyName);
    assert!(matches!(
        p.add_server(&blank),
        Err(ProjectError::InvalidServer(ServerError::EmptyName))
    ));

    // Taken: compared trimmed and case-sensitively; the server's own name is not a clash.
    let twin = Server {
        name: " dev ".into(),
        ..server("x")
    };
    assert_eq!(
        server_error(&p, &twin),
        ServerError::NameTaken("dev".into())
    );
    assert_eq!(
        p.add_server(&twin).expect_err("taken").to_string(),
        "a server named \"dev\" already exists"
    );
    p.check_server(&Server {
        name: "Dev".into(),
        ..server("x")
    })
    .expect("case differs");
    p.check_server(&dev).expect("its own name");

    for (url, expected) in [
        ("", ServerError::EmptyUrl),
        ("  ", ServerError::EmptyUrl),
        ("https://", ServerError::InvalidUrl(String::new())),
        ("example.com/soap", ServerError::InvalidUrl(String::new())),
        ("ftp://example.com/", ServerError::InvalidUrl(String::new())),
        ("/soap", ServerError::InvalidUrl(String::new())),
        (
            "http://user:pw@example.com/",
            ServerError::InvalidUrl(String::new()),
        ),
        (
            " https://example.com/",
            ServerError::InvalidUrl(String::new()),
        ),
    ] {
        let bad = Server {
            url: url.into(),
            ..server("x")
        };
        match (server_error(&p, &bad), expected) {
            (ServerError::InvalidUrl(msg), ServerError::InvalidUrl(_)) => {
                assert_eq!(Err(msg), crate::http::check_url(url), "{url:?}");
            }
            (got, expected) => assert_eq!(got, expected, "{url:?}"),
        }
    }
    for url in [
        "http://localhost/",
        "https://example.com",
        "http://127.0.0.1:8080/ws?x=1",
        "HTTPS://Example.com/soap",
    ] {
        p.check_server(&Server {
            url: url.into(),
            ..server("x")
        })
        .unwrap_or_else(|e| panic!("{url}: {e}"));
    }

    // Updates are checked too, and a refused one changes nothing.
    let before = dev.clone();
    dev.url = "https://".into();
    dev.timeout = Duration::from_secs(1);
    assert!(matches!(
        p.update_server(&dev, &secrets),
        Err(ProjectError::InvalidServer(ServerError::InvalidUrl(_)))
    ));
    assert_eq!(p.servers().expect("list"), [before]);
}

/// Projects written before the check may hold servers it refuses. They still load; saving one
/// needs it fixed first, deleting it does not.
#[test]
fn invalid_stored_servers_are_read_but_not_saved_unchanged() {
    let (_tmp, _folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let mut a = server("dup");
    let b = server("dup");
    for s in [&a, &b] {
        p.conn
            .execute(
                "INSERT INTO server (id, name, url, ignore_tls_errors, auth_kind, username,
                                     timeout_secs, sort_order)
                 VALUES (?1, ?2, 'https://', 0, 'none', NULL, 60, 0)",
                params![s.id.to_string(), s.name],
            )
            .expect("insert");
    }
    let stored = p.servers().expect("list");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].url, "https://");

    a.url = "https://".into();
    a.auth = Auth::None;
    a.timeout = Duration::from_secs(60);
    assert!(matches!(
        p.update_server(&a, &secrets),
        Err(ProjectError::InvalidServer(ServerError::NameTaken(_)))
    ));
    a.name = "fixed".into();
    assert!(matches!(
        p.update_server(&a, &secrets),
        Err(ProjectError::InvalidServer(ServerError::InvalidUrl(_)))
    ));
    a.url = "https://fixed.example/".into();
    p.update_server(&a, &secrets).expect("fixed");
    p.delete_server(b.id, &secrets).expect("delete");
    assert_eq!(p.servers().expect("list"), [a]);
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

#[test]
fn default_server_is_the_requests_else_the_projects_last() {
    let (_tmp, _folder, mut p) = new_project();
    let secrets = MemorySecretStore::default();
    let dev = server("dev");
    let prod = server("prod");
    p.add_server(&dev).expect("add");
    p.add_server(&prod).expect("add");
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    assert_eq!(p.default_server(Some(&a)).expect("default"), None);
    assert_eq!(p.default_server(None).expect("default"), None);

    p.set_request_last_server(a.id, Some(dev.id)).expect("last");
    let b = p.create_request(&op("GetCustomer"), "<b/>").expect("b");
    p.set_request_last_server(b.id, Some(prod.id))
        .expect("last");
    let a = p.request(a.id).expect("a");
    assert_eq!(p.default_server(Some(&a)).expect("default"), Some(dev.id));
    assert_eq!(p.default_server(None).expect("default"), Some(prod.id));

    // A stale `RequestMeta` naming a deleted server falls back to the project's last.
    p.set_request_last_server(b.id, Some(dev.id)).expect("last");
    p.set_request_last_server(a.id, Some(prod.id))
        .expect("last");
    let stale = p.request(b.id).expect("b");
    p.delete_server(dev.id, &secrets).expect("delete");
    assert_eq!(
        p.default_server(Some(&stale)).expect("default"),
        Some(prod.id)
    );
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
fn history_keeps_the_request_head_masked() {
    let (_tmp, folder, mut p) = new_project();
    let dev = server("dev");
    p.add_server(&dev).expect("add");
    let a = p.create_request(&op("GetCustomer"), "<a/>").expect("a");
    let mut sent = exchange(1_791_295_392, Some("HTTP/1.1 200 OK"));
    sent.request
        .headers
        .push(("authorization".into(), "Basic YWxpY2U6c2VjcmV0".into()));
    let old = p.record_exchange(a.id, &dev, &sent, false).expect("record");
    let head = p.load_history(old.id).expect("load").request_head;
    assert_eq!(
        head,
        Some(RequestHead {
            start_line: "POST /soap HTTP/1.1".into(),
            headers: vec![
                ("Content-Type".into(), "text/xml; charset=utf-8".into()),
                ("authorization".into(), "Basic ••••••••".into()),
            ],
        })
    );
    drop(p);

    // A v1 database: the column is added on open, and rows from before show no head.
    let conn = rusqlite::Connection::open(folder.join(DB_FILE)).expect("conn");
    conn.execute_batch("ALTER TABLE history DROP COLUMN request_headers; PRAGMA user_version = 1;")
        .expect("downgrade");
    drop(conn);
    let mut p = Project::open(&folder).expect("open");
    assert_eq!(p.load_history(old.id).expect("load").request_head, None);
    let new = p
        .record_exchange(a.id, &dev, &exchange(1_791_295_400, None), false)
        .expect("record");
    assert!(p.load_history(new.id).expect("load").request_head.is_some());
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

#[test]
fn wsdl_files_lists_entry_first_and_skips_hidden() {
    let (_tmp, folder, mut p) = new_project();
    fs::write(folder.join(WSDL_DIR).join(".DS_Store"), "x").expect("write");
    assert_eq!(
        p.wsdl_files().expect("files"),
        [
            "CustomerService.wsdl",
            "CustomerBinding.wsdl",
            "xsd/common/party-ids.xsd",
            "xsd/common/party.xsd",
            "xsd/customer.xsd",
            "xsd/ext/audit.xsd",
        ]
    );
    // `.previous` holds the old set and is not listed.
    p.replace_wsdl(&legacy_set()).expect("replace");
    assert_eq!(p.wsdl_files().expect("files"), ["Legacy.wsdl"]);
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

#[cfg(unix)]
#[test]
fn note_recent_keeps_one_entry_per_folder() {
    let tmp = TempDir::new().expect("tempdir");
    let folder = tmp.path().join("Customer API");
    fs::create_dir(&folder).expect("mkdir");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&folder, &link).expect("symlink");

    let mut state = AppState::default();
    state.note_recent(&folder);
    state.note_recent(&tmp.path().join("other"));
    state.note_recent(&link);
    state.note_recent(&tmp.path().join(".").join("Customer API"));
    assert_eq!(
        state.recent_projects,
        [
            tmp.path().join(".").join("Customer API"),
            tmp.path().join("other")
        ]
    );
    assert!(same_folder(&folder, &link));
    // A folder that is gone compares as spelled.
    assert!(same_folder(Path::new("/gone/x"), Path::new("/gone/x")));
    assert!(!same_folder(Path::new("/gone/x"), Path::new("/gone/y")));
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

#[test]
fn import_check_finds_the_set_and_refuses_unresolved_references() {
    let base = fixtures().join("customer");
    let checked = check_import(&base.join("CustomerService.wsdl"), &[]).expect("check");
    assert!(checked.is_usable(), "{:?}", checked.wsdl.check.diagnostics);
    let mut dests: Vec<&str> = checked.set.files.iter().map(|f| f.dest.as_str()).collect();
    dests.sort_unstable();
    let mut expected = CUSTOMER_FILES.to_vec();
    expected.sort_unstable();
    assert_eq!(dests, expected);

    // Alone, the entry WSDL's import of the binding cannot resolve; nothing is compiled.
    let tmp = TempDir::new().expect("tempdir");
    let alone = tmp.path().join("CustomerService.wsdl");
    fs::copy(base.join("CustomerService.wsdl"), &alone).expect("copy");
    let checked = check_import(&alone, &[]).expect("check");
    assert!(!checked.is_usable());
    assert!(checked.compile.is_empty());
}

#[test]
fn project_schema_loads_the_copied_set() {
    let (_tmp, _folder, p) = new_project();
    let schema = ProjectSchema::load(&p.entry_wsdl().expect("entry"), p.wsdl_dir())
        .expect("load")
        .prepared();
    assert!(schema.compile_errors().is_empty());
    assert!(schema.request_schema().is_some());
    let ops = schema.wsdl.supported_operations();
    assert!(ops.iter().any(|o| o.operation == "GetCustomer"), "{ops:?}");
}

#[test]
fn create_request_for_files_like_new_request() {
    let (_tmp, _folder, mut p) = new_project();
    let schema = ProjectSchema::load(&p.entry_wsdl().expect("entry"), p.wsdl_dir()).expect("load");
    let op = schema
        .wsdl
        .supported_operations()
        .into_iter()
        .find(|o| o.operation == "GetCustomer")
        .expect("op");
    let text = schema
        .request_template(&op, &crate::schema::TemplateOptions::default())
        .expect("template");

    let a = p
        .create_request_for(&schema.wsdl, &op, None, None, &text)
        .expect("a");
    assert_eq!(a.folder, operation_folder(&schema.wsdl, &op));
    assert_eq!(a.name, "GetCustomer 1");
    assert_eq!(a.operation.as_ref(), Some(&op));
    assert_eq!(p.read_request(a.id).expect("read"), text);

    let b = p
        .create_request_for(&schema.wsdl, &op, Some("Mine"), Some("Mine 1"), "<a/>")
        .expect("b");
    assert_eq!((b.folder.as_str(), b.name.as_str()), ("Mine", "Mine 1"));
}
