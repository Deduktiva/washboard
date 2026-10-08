//! Model tests against the fake front end. Projects are created in temp dirs from `fixtures/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use washboard_core::model::{Auth, RequestId};
use washboard_core::project::{Project, REQUESTS_DIR, STATE_FILE, WsdlFile, WsdlSet};
use washboard_core::xml::TokenKind;

use crate::fake::Fake;
use crate::{App, DialogAnswer, Event, ModelError, OperationNode, ProjectKey, SchemaState};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Creates a project named `name` in `<tmp>/<name>` and closes it again.
fn make_project(tmp: &TempDir, name: &str) -> PathBuf {
    let folder = tmp.path().join(name);
    let set = WsdlSet {
        files: vec![WsdlFile {
            source: fixtures().join("legacy-rpc/Legacy.wsdl"),
            dest: "Legacy.wsdl".into(),
        }],
        entry: "Legacy.wsdl".into(),
    };
    Project::create(&folder, name, &set).expect("create project");
    folder
}

struct Setup {
    tmp: TempDir,
    state_dir: PathBuf,
}

impl Setup {
    fn new() -> Setup {
        let tmp = TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        Setup { tmp, state_dir }
    }

    fn launch(&self) -> (Fake, App) {
        let (fake, front) = Fake::new();
        let mut app = App::new(&self.state_dir, front);
        app.launch();
        (fake, app)
    }
}

#[test]
fn first_launch_shows_the_welcome_window() {
    let setup = Setup::new();
    let (fake, mut app) = setup.launch();
    assert_eq!(
        app.take_events(),
        [
            Event::RecentProjectsChanged,
            Event::WelcomeVisibility { visible: true }
        ]
    );
    assert!(app.welcome_visible());
    assert_eq!(app.projects().count(), 0);
    assert!(fake.alerts().is_empty());
    assert!(
        fake.clock.borrow().timers.is_empty(),
        "nothing scheduled while idle"
    );
}

#[test]
fn welcome_shows_exactly_while_no_project_is_open() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    let b = make_project(&setup.tmp, "Beta");
    let (_fake, mut app) = setup.launch();
    app.take_events();

    let ka = app.open_project(&a).expect("open a");
    assert_eq!(
        app.take_events(),
        [
            Event::ProjectOpened { project: ka },
            Event::RecentProjectsChanged,
            Event::WelcomeVisibility { visible: false }
        ]
    );
    assert_eq!(app.project(ka).map(|w| w.name()), Some("Alpha"));

    let kb = app.open_project(&b).expect("open b");
    assert_eq!(
        app.take_events(),
        [
            Event::ProjectOpened { project: kb },
            Event::RecentProjectsChanged
        ]
    );
    assert_eq!(app.recent_projects(), [b.clone(), a.clone()]);

    assert!(app.close_project(ka));
    assert_eq!(app.take_events(), [Event::ProjectClosed { project: ka }]);
    assert!(app.close_project(kb));
    assert_eq!(
        app.take_events(),
        [
            Event::ProjectClosed { project: kb },
            Event::WelcomeVisibility { visible: true }
        ]
    );
}

#[test]
fn opening_an_open_project_focuses_its_window() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    let (_fake, mut app) = setup.launch();
    let key = app.open_project(&a).expect("open");
    app.take_events();
    // Another spelling of the same folder.
    let again = app
        .open_project(&setup.tmp.path().join("Alpha/../Alpha"))
        .expect("focus");
    assert_eq!(again, key);
    assert_eq!(app.take_events(), [Event::FocusProject { project: key }]);
    assert_eq!(app.projects().count(), 1);
}

#[test]
fn quit_and_launch_restore_the_open_projects() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    let b = make_project(&setup.tmp, "Beta");
    {
        let (_fake, mut app) = setup.launch();
        app.open_project(&a).expect("open a");
        app.open_project(&b).expect("open b");
        assert!(app.quit().expect("quit"));
    }
    assert!(setup.state_dir.join(STATE_FILE).is_file());

    let (fake, mut app) = setup.launch();
    let names: Vec<&str> = app.projects().map(|(_, w)| w.name()).collect();
    assert_eq!(names, ["Alpha", "Beta"], "window order kept");
    assert_eq!(app.recent_projects(), [b, a]);
    assert!(!app.welcome_visible());
    let events = app.take_events();
    assert_eq!(
        events.last(),
        Some(&Event::WelcomeVisibility { visible: false })
    );
    assert!(fake.alerts().is_empty());
}

#[test]
fn a_missing_project_is_reported_once_and_dropped() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    let b = make_project(&setup.tmp, "Beta");
    {
        let (_fake, mut app) = setup.launch();
        app.open_project(&a).expect("open a");
        app.open_project(&b).expect("open b");
        assert!(app.quit().expect("quit"));
    }
    fs::remove_dir_all(&b).expect("remove Beta");

    let (fake, app) = setup.launch();
    let alerts = fake.alerts();
    assert_eq!(alerts.len(), 1, "one alert for all failures");
    assert!(alerts[0].message.contains("Beta"), "{alerts:?}");
    let names: Vec<&str> = app.projects().map(|(_, w)| w.name()).collect();
    assert_eq!(names, ["Alpha"]);
    drop(app);

    let (fake, app) = setup.launch();
    assert!(fake.alerts().is_empty(), "not reported again");
    assert_eq!(app.projects().count(), 1);
}

#[test]
fn an_unreadable_state_file_starts_empty_with_an_alert() {
    let setup = Setup::new();
    fs::create_dir_all(&setup.state_dir).expect("state dir");
    fs::write(setup.state_dir.join(STATE_FILE), "{ not json").expect("write");
    let (fake, app) = setup.launch();
    assert_eq!(fake.alerts().len(), 1);
    assert!(app.welcome_visible());
}

#[test]
fn open_project_panel() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    let (fake, mut app) = setup.launch();
    app.take_events();

    app.choose_and_open_project();
    fake.answer_folder(&mut app, DialogAnswer::Cancelled);
    assert!(app.take_events().is_empty(), "cancel does nothing");

    app.choose_and_open_project();
    fake.answer_folder(&mut app, DialogAnswer::Folder(setup.tmp.path().to_owned()));
    let alerts = fake.alerts();
    assert_eq!(alerts.len(), 1, "not a project folder");
    assert!(alerts[0].title.contains("Could not open"));

    app.choose_and_open_project();
    fake.answer_folder(&mut app, DialogAnswer::Folder(a));
    assert_eq!(app.projects().count(), 1);
}

#[test]
fn worker_results_are_applied_on_pump() {
    let setup = Setup::new();
    let (fake, mut app) = setup.launch();
    app.take_events();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    app.spawn(
        move || {
            // Holds until the test has checked that nothing ran early.
            let _ = rx.recv();
            42
        },
        |app, n: u32| {
            assert_eq!(n, 42);
            app.events.push(Event::RecentProjectsChanged);
        },
    );
    app.pump();
    assert!(app.take_events().is_empty(), "not done yet");
    tx.send(()).expect("release worker");
    fake.pump_after_wakes(&mut app, 1);
    assert_eq!(app.take_events(), [Event::RecentProjectsChanged]);
}

/// Opens a fresh project and waits for its schema.
fn open_loaded(setup: &Setup, name: &str) -> (Fake, App, ProjectKey) {
    let folder = make_project(&setup.tmp, name);
    let (fake, mut app) = setup.launch();
    app.open_project(&folder).expect("open");
    fake.pump_after_wakes(&mut app, 1);
    let key = app.projects().next().expect("open").0;
    app.take_events();
    (fake, app, key)
}

fn names(app: &App, key: ProjectKey) -> Vec<String> {
    let window = app.project(key).expect("open");
    window
        .sidebar()
        .requests
        .iter()
        .map(|r| r.name.clone())
        .collect()
}

fn lookup(app: &App, key: ProjectKey, port: &str) -> OperationNode {
    let window = app.project(key).expect("open");
    window.sidebar().services[0]
        .ports
        .iter()
        .find(|p| p.name == port)
        .expect("port")
        .operations[0]
        .clone()
}

#[test]
fn the_operation_tree_shows_unsupported_operations() {
    let setup = Setup::new();
    let (_fake, app, key) = open_loaded(&setup, "Legacy");
    let services = &app.project(key).expect("open").sidebar().services;
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "LegacyService");
    let ports: Vec<_> = services[0].ports.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(ports, ["LegacyPort", "LegacyEncodedPort"]);
    let rpc = lookup(&app, key, "LegacyPort");
    assert_eq!(rpc.name(), "Lookup");
    assert_eq!(rpc.unsupported, None);
    assert!(lookup(&app, key, "LegacyEncodedPort").unsupported.is_some());
}

#[test]
fn new_request_waits_for_the_schema() {
    let setup = Setup::new();
    let folder = make_project(&setup.tmp, "Legacy");
    let (fake, mut app) = setup.launch();
    app.open_project(&folder).expect("open");
    let key = app.projects().next().expect("open").0;
    assert!(matches!(
        app.project(key).expect("open").schema(),
        SchemaState::Loading
    ));
    assert!(
        app.project(key)
            .expect("open")
            .sidebar()
            .services
            .is_empty()
    );
    fake.pump_after_wakes(&mut app, 1);
    assert!(
        app.take_events()
            .contains(&Event::SidebarChanged { project: key })
    );
    assert!(matches!(
        app.project(key).expect("open").schema(),
        SchemaState::Ready(_)
    ));
}

#[test]
fn new_request_creates_selects_and_starts_rename() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let id = app.new_request(key, &op).expect("new request");
    assert_eq!(
        app.take_events(),
        [
            Event::SidebarChanged { project: key },
            Event::SelectionChanged { project: key },
            Event::ServerSelectionChanged { project: key },
            Event::EditorReplaced { project: key },
            Event::BeginRename {
                project: key,
                request: id
            },
        ]
    );
    let window = app.project(key).expect("open");
    assert_eq!(window.selected_request(), Some(id));
    assert_eq!(names(&app, key), ["Lookup 1"]);
    let text = window.project().read_request(id).expect("read");
    assert!(text.contains("Lookup"), "{text}");
}

#[test]
fn unsupported_operations_get_no_request() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyEncodedPort").operation;
    assert!(matches!(
        app.new_request(key, &op),
        Err(ModelError::Envelope(_))
    ));
    assert!(names(&app, key).is_empty());
}

#[test]
fn rename_and_duplicate() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let id = app.new_request(key, &op).expect("new request");
    app.rename_request(key, id, "Find customer")
        .expect("rename");
    assert!(matches!(
        app.rename_request(key, id, ""),
        Err(ModelError::Project(_))
    ));
    let copy = app.duplicate_request(key, id).expect("duplicate");
    assert_eq!(names(&app, key), ["Find customer", "Find customer copy"]);
    assert_eq!(
        app.project(key).expect("open").selected_request(),
        Some(copy)
    );
}

#[test]
fn delete_asks_first_and_selects_a_neighbour() {
    let setup = Setup::new();
    let (fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let first = app.new_request(key, &op).expect("new");
    let second = app.new_request(key, &op).expect("new");
    app.select_request(key, Some(first)).expect("select");

    app.delete_request(key, first).expect("delete");
    let confirm = fake.answer_confirm(&mut app, DialogAnswer::Cancelled);
    assert_eq!(confirm.title, "Delete “Lookup 1”?");
    assert_eq!(names(&app, key), ["Lookup 1", "Lookup 2"]);

    app.take_events();
    app.delete_request(key, first).expect("delete");
    fake.answer_confirm(&mut app, DialogAnswer::Confirmed);
    assert_eq!(names(&app, key), ["Lookup 2"]);
    let window = app.project(key).expect("open");
    assert_eq!(window.selected_request(), Some(second));
    let events = app.take_events();
    assert!(events.contains(&Event::SidebarChanged { project: key }));
    assert!(events.contains(&Event::SelectionChanged { project: key }));
}

#[test]
fn deleting_the_last_request_clears_the_selection() {
    let setup = Setup::new();
    let (fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let id = app.new_request(key, &op).expect("new");
    app.delete_request(key, id).expect("delete");
    fake.answer_confirm(&mut app, DialogAnswer::Confirmed);
    assert_eq!(app.project(key).expect("open").selected_request(), None);
}

#[test]
fn the_selection_survives_a_relaunch() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let first = app.new_request(key, &op).expect("new");
    let _second = app.new_request(key, &op).expect("new");
    app.select_request(key, Some(first)).expect("select");
    assert!(app.quit().expect("quit"));
    drop(app);

    let (_fake, app) = setup.launch();
    let (_, window) = app.projects().next().expect("open");
    assert_eq!(window.selected_request(), Some(first));
}

#[test]
fn the_server_popup_follows_the_request() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    let a = app.add_server(key).expect("add");
    let b = app.add_server(key).expect("add");
    let window = app.project(key).expect("open");
    assert_eq!(window.servers().len(), 2);
    assert_eq!(
        window.selected_server(),
        Some(a),
        "first server without a request"
    );

    let first = app.new_request(key, &op).expect("new");
    app.choose_server(key, b).expect("choose");
    let second = app.new_request(key, &op).expect("new");
    assert_eq!(
        app.project(key).expect("open").selected_server(),
        Some(b),
        "a new request starts with the last used server"
    );
    app.choose_server(key, a).expect("choose");
    app.select_request(key, Some(first)).expect("select");
    assert_eq!(app.project(key).expect("open").selected_server(), Some(b));
    app.select_request(key, Some(second)).expect("select");
    assert_eq!(app.project(key).expect("open").selected_server(), Some(a));

    app.delete_server(key, a).expect("delete");
    assert_eq!(app.project(key).expect("open").selected_server(), Some(b));
}

#[test]
fn server_passwords_go_to_the_secret_store() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    let id = app.add_server(key).expect("add");
    let mut server = app.project(key).expect("open").servers()[0].clone();
    server.name = "Staging".into();
    server.auth = Auth::Basic {
        username: "alice".into(),
    };
    app.update_server(key, &server, Some("s3cret"))
        .expect("update");
    assert_eq!(
        app.take_events().first(),
        Some(&Event::ServersChanged { project: key })
    );
    assert_eq!(app.project(key).expect("open").servers()[0].name, "Staging");
    assert_eq!(
        app.server_password(key, id).expect("pw").as_deref(),
        Some("s3cret")
    );

    app.update_server(key, &server, None).expect("update");
    assert_eq!(
        app.server_password(key, id).expect("pw").as_deref(),
        Some("s3cret")
    );

    server.auth = Auth::None;
    app.update_server(key, &server, Some("ignored"))
        .expect("update");
    assert_eq!(app.server_password(key, id).expect("pw"), None);
}

#[test]
fn commands_on_a_closed_project_fail_softly() {
    let setup = Setup::new();
    let (_fake, mut app, key) = open_loaded(&setup, "Legacy");
    assert!(app.close_project(key));
    assert!(matches!(
        app.select_request(key, None),
        Err(ModelError::UnknownProject)
    ));
    assert!(matches!(
        app.add_server(key),
        Err(ModelError::UnknownProject)
    ));
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A project with two requests, the first selected, and the events drained.
fn with_requests(setup: &Setup, name: &str) -> (Fake, App, ProjectKey, RequestId, RequestId) {
    let (fake, mut app, key) = open_loaded(setup, name);
    let op = lookup(&app, key, "LegacyPort").operation;
    let first = app.new_request(key, &op).expect("new");
    let second = app.new_request(key, &op).expect("new");
    app.select_request(key, Some(first)).expect("select");
    app.take_events();
    (fake, app, key, first, second)
}

fn editor_text(app: &App, key: ProjectKey) -> String {
    let window = app.project(key).expect("open");
    window.editor().expect("editor").text().to_owned()
}

fn on_disk(app: &App, key: ProjectKey, request: RequestId) -> String {
    let window = app.project(key).expect("open");
    window.project().read_request(request).expect("read")
}

fn request_file(app: &App, key: ProjectKey, request: RequestId) -> PathBuf {
    let window = app.project(key).expect("open");
    let name = window.project().request(request).expect("meta").name;
    window.path().join(REQUESTS_DIR).join(format!("{name}.xml"))
}

#[test]
fn selecting_a_request_opens_it_in_the_editor() {
    let setup = Setup::new();
    let (_fake, mut app, key, first, second) = with_requests(&setup, "Legacy");
    assert_eq!(editor_text(&app, key), on_disk(&app, key, first));
    app.select_request(key, Some(second)).expect("select");
    let events = app.take_events();
    assert!(events.contains(&Event::EditorReplaced { project: key }));
    let window = app.project(key).expect("open");
    assert_eq!(window.editor().expect("editor").request(), second);
    app.select_request(key, None).expect("select");
    assert!(app.project(key).expect("open").editor().is_none());
    assert!(matches!(
        app.edit(key, 0..0, "x"),
        Err(ModelError::NoRequestSelected)
    ));
}

#[test]
fn an_edit_marks_dirty_and_retokenizes_in_utf16() {
    let setup = Setup::new();
    let (_fake, mut app, key, first, _) = with_requests(&setup, "Legacy");
    let len = app
        .project(key)
        .expect("open")
        .editor()
        .expect("editor")
        .utf16_len();
    app.edit(key, len..len, "<!--😀-->").expect("edit");
    let events = app.take_events();
    let Event::TokensChanged { range, .. } = &events[0] else {
        panic!("{events:?}");
    };
    assert!(range.start <= len && range.end == len + 9, "{range:?}");
    assert_eq!(
        events[1..],
        [
            Event::SidebarChanged { project: key },
            Event::EditedChanged { project: key },
        ]
    );
    // The comment is one token of 9 UTF-16 units (the emoji counts two).
    let window = app.project(key).expect("open");
    let editor = window.editor().expect("editor");
    assert_eq!(
        editor.tokens_utf16(len..len + 9),
        [(len..len + 9, TokenKind::Comment)]
    );
    assert!(editor.dirty());
    assert_eq!(editor.version(), 1);
    assert!(window.edited());
    assert!(
        window
            .sidebar()
            .requests
            .iter()
            .any(|r| r.id == first && r.dirty)
    );

    // Replace the emoji (2 units) after `<!--`: only TokensChanged, it is dirty already.
    app.edit(key, len + 4..len + 6, "x").expect("edit");
    assert!(matches!(
        app.take_events()[..],
        [Event::TokensChanged { .. }]
    ));
    assert!(editor_text(&app, key).ends_with("<!--x-->"));
}

#[test]
fn autosave_runs_a_second_after_the_last_edit() {
    let setup = Setup::new();
    let (fake, mut app, key, first, _) = with_requests(&setup, "Legacy");
    let saved = on_disk(&app, key, first);
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    fake.advance(&mut app, ms(600));
    app.edit(key, 0..0, "<!--b-->").expect("edit");
    assert_eq!(fake.running_timers(), 1, "the first timer was cancelled");
    fake.advance(&mut app, ms(900));
    assert_eq!(
        on_disk(&app, key, first),
        saved,
        "not yet: 900 ms since the last edit"
    );
    app.take_events();
    fake.advance(&mut app, ms(100));
    assert_eq!(
        on_disk(&app, key, first),
        format!("<!--b--><!--a-->{saved}")
    );
    assert_eq!(
        app.take_events(),
        [
            Event::SidebarChanged { project: key },
            Event::EditedChanged { project: key },
        ]
    );
    assert!(!app.project(key).expect("open").edited());
    assert_eq!(fake.running_timers(), 0);
}

#[test]
fn switching_requests_saves_first() {
    let setup = Setup::new();
    let (fake, mut app, key, first, second) = with_requests(&setup, "Legacy");
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    app.select_request(key, Some(second)).expect("select");
    assert!(on_disk(&app, key, first).starts_with("<!--a-->"));
    assert_eq!(fake.running_timers(), 0);
    assert!(!app.project(key).expect("open").edited());
}

#[test]
fn focus_loss_deactivation_and_close_save() {
    let setup = Setup::new();
    let (_fake, mut app, key, first, _) = with_requests(&setup, "Legacy");
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    app.window_resigned_key(key);
    assert!(on_disk(&app, key, first).starts_with("<!--a-->"));

    app.edit(key, 0..0, "<!--b-->").expect("edit");
    app.app_deactivated();
    assert!(on_disk(&app, key, first).starts_with("<!--b-->"));

    app.edit(key, 0..0, "<!--c-->").expect("edit");
    let file = request_file(&app, key, first);
    assert!(app.close_project(key));
    assert!(
        fs::read_to_string(file)
            .expect("read")
            .starts_with("<!--c-->")
    );
}

#[test]
fn save_all_and_quit_cover_every_project() {
    let setup = Setup::new();
    let (fake, mut app, ka, a, _) = with_requests(&setup, "A");
    let folder = make_project(&setup.tmp, "B");
    let kb = app.open_project(&folder).expect("open");
    fake.pump_after_wakes(&mut app, 2);
    let op = lookup(&app, kb, "LegacyPort").operation;
    let b = app.new_request(kb, &op).expect("new");

    app.edit(ka, 0..0, "<!--a-->").expect("edit");
    app.edit(kb, 0..0, "<!--b-->").expect("edit");
    assert!(app.save_all());
    assert!(on_disk(&app, ka, a).starts_with("<!--a-->"));
    assert!(on_disk(&app, kb, b).starts_with("<!--b-->"));
    assert_eq!(fake.running_timers(), 0);

    app.edit(ka, 0..0, "<!--q-->").expect("edit");
    assert!(app.quit().expect("quit"));
    assert!(on_disk(&app, ka, a).starts_with("<!--q-->"));
}

#[test]
fn duplicate_includes_unsaved_edits() {
    let setup = Setup::new();
    let (_fake, mut app, key, first, _) = with_requests(&setup, "Legacy");
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    let copy = app.duplicate_request(key, first).expect("duplicate");
    assert!(on_disk(&app, key, copy).starts_with("<!--a-->"));
}

#[test]
fn deleting_an_edited_request_drops_its_edits() {
    let setup = Setup::new();
    let (fake, mut app, key, first, second) = with_requests(&setup, "Legacy");
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    app.delete_request(key, first).expect("delete");
    fake.answer_confirm(&mut app, DialogAnswer::Confirmed);
    assert!(fake.alerts().is_empty());
    assert_eq!(fake.running_timers(), 0);
    let window = app.project(key).expect("open");
    assert_eq!(window.editor().expect("editor").request(), second);
    assert!(!window.edited());
}

#[test]
fn a_utf8_bom_is_kept_and_utf16_becomes_utf8() {
    let setup = Setup::new();
    let (_fake, mut app, key, first, second) = with_requests(&setup, "Legacy");
    let text = on_disk(&app, key, first);

    let bom_file = request_file(&app, key, first);
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend_from_slice(text.as_bytes());
    fs::write(&bom_file, bytes).expect("write");
    let utf16_file = request_file(&app, key, second);
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    fs::write(&utf16_file, bytes).expect("write");

    app.select_request(key, Some(second)).expect("select");
    app.select_request(key, Some(first)).expect("select");
    assert_eq!(
        editor_text(&app, key),
        text,
        "the BOM is not part of the text"
    );
    app.edit(key, 0..0, "<!--a-->").expect("edit");
    app.select_request(key, Some(second)).expect("select");
    assert_eq!(editor_text(&app, key), text, "UTF-16 decoded");
    app.edit(key, 0..0, "<!--b-->").expect("edit");
    assert!(app.save_all());

    let saved = fs::read(&bom_file).expect("read");
    assert_eq!(saved[..3], *b"\xEF\xBB\xBF");
    assert_eq!(saved[3..], *format!("<!--a-->{text}").as_bytes());
    let saved = fs::read(&utf16_file).expect("read");
    assert_eq!(saved, format!("<!--b-->{text}").into_bytes());
}

#[test]
fn the_restored_selection_opens_in_the_editor() {
    let setup = Setup::new();
    let (_fake, mut app, key, _, second) = with_requests(&setup, "Legacy");
    app.select_request(key, Some(second)).expect("select");
    let text = editor_text(&app, key);
    assert!(app.quit().expect("quit"));
    drop(app);
    let (_fake, app) = setup.launch();
    let (_, window) = app.projects().next().expect("open");
    let editor = window.editor().expect("editor");
    assert_eq!((editor.request(), editor.text()), (second, text.as_str()));
}
