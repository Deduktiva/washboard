//! Model tests against the fake front end. Projects are created in temp dirs from `fixtures/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use washboard_core::diag::DiagSource;
use washboard_core::model::{Auth, RequestId};
use washboard_core::project::{Project, REQUESTS_DIR, STATE_FILE, WsdlFile, WsdlSet};
use washboard_core::xml::TokenKind;

use crate::fake::Fake;
use crate::server;
use crate::{
    App, CheckState, CompletionKind, Completions, DialogAnswer, Event, ImportTarget, Issue,
    ModelError, OperationNode, ProjectKey, SchemaState,
};

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
fn clearing_recent_projects_survives_a_relaunch() {
    let setup = Setup::new();
    let a = make_project(&setup.tmp, "Alpha");
    {
        let (_fake, mut app) = setup.launch();
        let key = app.open_project(&a).expect("open a");
        assert!(app.close_project(key));
        app.take_events();
        app.clear_recent_projects();
        assert_eq!(app.take_events(), [Event::RecentProjectsChanged]);
        assert!(app.recent_projects().is_empty());
        app.clear_recent_projects();
        assert!(app.take_events().is_empty(), "already empty");
        assert!(app.quit().expect("quit"));
    }
    let (_fake, app) = setup.launch();
    assert!(app.recent_projects().is_empty());
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
    let key = app.open_project(&folder).expect("open");
    fake.pump_until(&mut app, |app| loaded(app, key));
    app.take_events();
    (fake, app, key)
}

fn loaded(app: &App, key: ProjectKey) -> bool {
    let window = app.project(key).expect("open");
    !matches!(window.schema(), SchemaState::Loading)
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
fn the_default_operation_is_the_first_supported_one() {
    let setup = Setup::new();
    let folder = make_project(&setup.tmp, "Legacy");
    let (fake, mut app) = setup.launch();
    app.open_project(&folder).expect("open");
    let key = app.projects().next().expect("open").0;
    assert!(matches!(
        app.default_operation(key),
        Err(ModelError::SchemaNotReady)
    ));
    fake.pump_after_wakes(&mut app, 1);
    let op = app.default_operation(key).expect("an operation");
    assert_eq!(op, lookup(&app, key, "LegacyPort").operation);
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
    settle(&fake, &mut app);
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
    let editor = app.project(key).expect("open").editor().expect("editor");
    let (len, loaded_version) = (editor.utf16_len(), editor.version());
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
    assert_ne!(editor.version(), loaded_version);
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
    assert_eq!(
        fake.running_timers(),
        3,
        "one each for autosave, well-formedness and validation: the first ones were cancelled"
    );
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
    fake.pump_until(&mut app, |app| loaded(app, kb));
    let op = lookup(&app, kb, "LegacyPort").operation;
    let b = app.new_request(kb, &op).expect("new");

    app.edit(ka, 0..0, "<!--a-->").expect("edit");
    app.edit(kb, 0..0, "<!--b-->").expect("edit");
    assert!(app.save_all());
    assert!(on_disk(&app, ka, a).starts_with("<!--a-->"));
    assert!(on_disk(&app, kb, b).starts_with("<!--b-->"));
    assert_eq!(
        fake.running_timers(),
        4,
        "only the two checks per project are left"
    );

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

/// Waits until every worker's result is applied, and drops the events.
fn settle(fake: &Fake, app: &mut App) {
    fake.pump_until(app, |app| app.jobs_running() == 0);
    app.take_events();
}

/// Waits for the next check result for the open editor.
fn next_check(fake: &Fake, app: &mut App, key: ProjectKey) -> Vec<Issue> {
    let changed = Event::DiagnosticsChanged { project: key };
    fake.pump_until(app, |app| app.events.contains(&changed));
    app.take_events();
    let window = app.project(key).expect("open");
    window.editor().expect("editor").issues().to_vec()
}

/// Replaces the editor's whole text.
fn set_text(app: &mut App, key: ProjectKey, text: &str) {
    let window = app.project(key).expect("open");
    let len = window.editor().expect("editor").utf16_len();
    app.edit(key, 0..len, text).expect("edit");
}

fn invalid_marker(app: &App, key: ProjectKey, request: RequestId) -> bool {
    let window = app.project(key).expect("open");
    let row = window.sidebar().requests.iter().find(|r| r.id == request);
    row.expect("row").invalid
}

#[test]
fn an_opened_request_is_validated() {
    let setup = Setup::new();
    let (fake, mut app, key) = open_loaded(&setup, "Legacy");
    let op = lookup(&app, key, "LegacyPort").operation;
    app.new_request(key, &op).expect("new");
    assert_eq!(next_check(&fake, &mut app, key), []);
}

#[test]
fn well_formedness_is_checked_150_ms_after_the_last_edit() {
    let setup = Setup::new();
    let (fake, mut app, key, first, _) = with_requests(&setup, "Legacy");

    // 😀 is two UTF-16 units: the error's range must count it that way.
    set_text(&mut app, key, "<!--😀-->\n<a>");
    fake.advance(&mut app, ms(149));
    assert_eq!(
        fake.running_timers(),
        3,
        "autosave, well-formedness, validation"
    );
    fake.advance(&mut app, ms(1));
    let issues = next_check(&fake, &mut app, key);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].source, DiagSource::WellFormedness);
    assert!(issues[0].is_error());
    assert_eq!(issues[0].line, Some(2));
    let at = issues[0].range.clone().expect("range").start;
    assert!(at >= 10, "{at} is past `<!--😀-->\\n` (10 units)");
    assert!(invalid_marker(&app, key, first));
}

#[test]
fn schema_errors_come_from_the_full_check_after_a_second() {
    let setup = Setup::new();
    let (fake, mut app, key, first, _) = with_requests(&setup, "Legacy");
    let text = editor_text(&app, key).replace("2026-01-01", "yesterday");
    set_text(&mut app, key, &text);

    fake.advance(&mut app, ms(150));
    assert_eq!(next_check(&fake, &mut app, key), [], "well-formed");
    fake.advance(&mut app, ms(850));
    let issues = next_check(&fake, &mut app, key);
    assert!(
        issues
            .iter()
            .any(|i| i.is_error() && i.source == DiagSource::Schema && i.line == Some(6)),
        "{issues:?}"
    );
    assert!(invalid_marker(&app, key, first));
    assert!(app.take_events().is_empty());

    let fixed = text.replace("yesterday", "2026-01-02");
    set_text(&mut app, key, &fixed);
    app.validate(key).expect("validate");
    assert_eq!(fake.running_timers(), 1, "only autosave is left");
    assert_eq!(next_check(&fake, &mut app, key), []);
    assert!(!invalid_marker(&app, key, first));
}

#[test]
fn results_for_an_older_text_are_dropped() {
    let setup = Setup::new();
    let (fake, mut app, key, _, _) = with_requests(&setup, "Legacy");

    set_text(&mut app, key, "<a>");
    app.validate(key).expect("validate");
    app.edit(key, 3..3, "</a>").expect("edit");
    fake.pump_until(&mut app, |app| app.jobs_running() == 0);
    assert!(
        !app.take_events()
            .contains(&Event::DiagnosticsChanged { project: key }),
        "the result for `<a>` must not be shown for `<a></a>`"
    );
    assert_eq!(
        app.project(key)
            .expect("open")
            .editor()
            .expect("editor")
            .issues(),
        []
    );
}

#[test]
fn results_for_another_request_are_dropped() {
    let setup = Setup::new();
    let (fake, mut app, key, _, second) = with_requests(&setup, "Legacy");
    set_text(&mut app, key, "<a>");
    app.validate(key).expect("validate");
    app.select_request(key, Some(second)).expect("select");
    fake.pump_until(&mut app, |app| app.jobs_running() == 0);
    let window = app.project(key).expect("open");
    assert_eq!(window.editor().expect("editor").issues(), []);
}

#[test]
fn without_a_schema_only_well_formedness_is_checked() {
    let setup = Setup::new();
    let folder = make_project(&setup.tmp, "Legacy");
    {
        let (fake, mut app) = setup.launch();
        let key = app.open_project(&folder).expect("open");
        fake.pump_until(&mut app, |app| loaded(app, key));
        let op = lookup(&app, key, "LegacyPort").operation;
        app.new_request(key, &op).expect("new");
        assert!(app.quit().expect("quit"));
    }
    fs::remove_file(folder.join("wsdl/Legacy.wsdl")).expect("remove");

    let (fake, mut app) = setup.launch();
    let key = app.projects().next().expect("open").0;
    let issues = next_check(&fake, &mut app, key);
    assert!(matches!(
        app.project(key).expect("open").schema(),
        SchemaState::Failed(_)
    ));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(!issues[0].is_error());
    assert!(
        issues[0].message.contains("only well-formedness"),
        "{issues:?}"
    );
}

const FAULT: &str = "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\">\
<soapenv:Body><soapenv:Fault><faultcode>soapenv:Server</faultcode>\
<faultstring>no such customer</faultstring></soapenv:Fault></soapenv:Body></soapenv:Envelope>";

const OK_BODY: &str = "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\">\
<soapenv:Body><r/></soapenv:Body></soapenv:Envelope>";

/// A project with one request and a server pointing at `url`, selected.
fn ready_to_send(setup: &Setup, url: &str) -> (Fake, App, ProjectKey, RequestId) {
    let (fake, mut app, key, first, _) = with_requests(setup, "Legacy");
    let id = app.add_server(key).expect("add");
    let mut server = app.project(key).expect("open").servers()[0].clone();
    server.url = url.to_owned();
    server.auth = Auth::Basic {
        username: "alice".into(),
    };
    app.update_server(key, &server, Some("s3cret"))
        .expect("update");
    app.choose_server(key, id).expect("choose");
    settle(&fake, &mut app);
    (fake, app, key, first)
}

fn send_and_wait(fake: &Fake, app: &mut App, key: ProjectKey) -> Vec<Event> {
    app.send(key).expect("send");
    assert!(app.project(key).expect("open").sending());
    fake.pump_until(app, |app| app.jobs_running() == 0);
    app.take_events()
}

#[test]
fn send_shows_the_response_and_records_history() {
    let setup = Setup::new();
    let (url, server) = server::serve_once("200 OK", OK_BODY, Duration::ZERO);
    let (fake, mut app, key, first) = ready_to_send(&setup, &url);
    let events = send_and_wait(&fake, &mut app, key);
    for e in [
        Event::SendStateChanged { project: key },
        Event::ResponseChanged { project: key },
        Event::HistoryChanged { project: key },
        Event::LogAppended,
    ] {
        assert!(events.contains(&e), "{e:?} in {events:?}");
    }
    let wire = server.join().expect("server");
    assert!(wire.starts_with("POST /soap HTTP/1.1"), "{wire}");
    assert!(wire.contains("<tns:Lookup>"), "{wire}");

    let window = app.project(key).expect("open");
    assert!(!window.sending());
    let response = window.response().expect("response");
    assert_eq!(response.status, Some(200));
    assert_eq!(response.error, None);
    assert!(response.fault.is_none());
    assert_eq!(response.size, OK_BODY.len());
    assert!(
        response.body.as_deref().expect("body").contains("\n"),
        "pretty-printed"
    );
    assert_eq!(window.history().len(), 1);
    assert_eq!(response.history, Some(window.history()[0].id));
    assert_eq!(window.history()[0].request_id, first);

    let log = app.http_log();
    assert_eq!(log.len(), 1);
    let masked = log[0].request_headers(false);
    let auth = masked
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("authorization"));
    assert_eq!(auth.map(|(_, v)| v.as_str()), Some("Basic ••••••••"));
    let revealed = log[0].request_headers(true);
    let auth = revealed
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("authorization"));
    assert_ne!(auth.map(|(_, v)| v.as_str()), Some("Basic ••••••••"));
}

#[test]
fn responses_name_their_server_after_renames_and_deletes() {
    let setup = Setup::new();
    let (url, _server) = server::serve_once("200 OK", OK_BODY, Duration::ZERO);
    let (fake, mut app, key, _) = ready_to_send(&setup, &url);
    send_and_wait(&fake, &mut app, key);
    let label = |app: &App| {
        let window = app.project(key).expect("open");
        let response = window.response().expect("response");
        window.server_label(response.server, &response.url)
    };
    let mut server = app.project(key).expect("open").servers()[0].clone();
    assert_eq!(label(&app), server.name);

    server.name = "Renamed".into();
    app.update_server(key, &server, None).expect("rename");
    assert_eq!(label(&app), "Renamed", "the current name");

    app.delete_server(key, server.id).expect("delete");
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next());
    assert_eq!(
        Some(label(&app).as_str()),
        host,
        "the URL's host once deleted"
    );
}

#[test]
fn faults_are_called_out() {
    let setup = Setup::new();
    let (url, _server) = server::serve_once("500 Internal Server Error", FAULT, Duration::ZERO);
    let (fake, mut app, key, _) = ready_to_send(&setup, &url);
    send_and_wait(&fake, &mut app, key);
    let window = app.project(key).expect("open");
    let response = window.response().expect("response");
    assert_eq!(response.status, Some(500));
    let fault = response.fault.as_ref().expect("fault");
    assert_eq!(fault.string, "no such customer");
    assert!(window.history()[0].soap_fault);
}

#[test]
fn a_transport_error_is_shown_and_recorded() {
    let setup = Setup::new();
    let url = server::closed_port_url();
    let (fake, mut app, key, _) = ready_to_send(&setup, &url);
    send_and_wait(&fake, &mut app, key);
    let window = app.project(key).expect("open");
    let response = window.response().expect("response");
    assert!(response.error.is_some());
    assert_eq!(response.status, None);
    assert!(window.history()[0].error.is_some());
}

#[test]
fn send_refuses_an_invalid_request() {
    let setup = Setup::new();
    let (fake, mut app, key, _) = ready_to_send(&setup, "http://127.0.0.1:9/");
    let text = editor_text(&app, key).replace("2026-01-01", "yesterday");
    set_text(&mut app, key, &text);
    let events = send_and_wait(&fake, &mut app, key);
    assert!(
        events.contains(&Event::ShowIssues { project: key }),
        "{events:?}"
    );
    let window = app.project(key).expect("open");
    assert!(
        window
            .editor()
            .expect("editor")
            .issues()
            .iter()
            .any(Issue::is_error)
    );
    assert!(window.response().is_none());
    assert!(window.history().is_empty());
    assert!(app.http_log().is_empty());
    assert!(
        on_disk(&app, key, window.editor().expect("editor").request()).contains("yesterday"),
        "send saves first"
    );
}

#[test]
fn a_cancelled_send_is_ignored() {
    let setup = Setup::new();
    let (url, _server) = server::serve_once("200 OK", OK_BODY, ms(300));
    let (fake, mut app, key, _) = ready_to_send(&setup, &url);
    app.send(key).expect("send");
    assert!(matches!(app.send(key), Err(ModelError::AlreadySending)));
    app.cancel_send(key);
    assert!(!app.project(key).expect("open").sending());
    fake.pump_until(&mut app, |app| app.jobs_running() == 0);
    let events = app.take_events();
    assert!(
        !events.contains(&Event::ResponseChanged { project: key }),
        "{events:?}"
    );
    let window = app.project(key).expect("open");
    assert!(window.response().is_none());
    assert!(window.history().is_empty());
    assert!(app.http_log().is_empty());
}

#[test]
fn history_entries_can_be_shown_and_restored() {
    let setup = Setup::new();
    let (url, _server) = server::serve_once("200 OK", OK_BODY, Duration::ZERO);
    let (fake, mut app, key, first) = ready_to_send(&setup, &url);
    let sent = editor_text(&app, key);
    send_and_wait(&fake, &mut app, key);
    let entry = app.project(key).expect("open").history()[0].id;

    set_text(&mut app, key, "<changed/>");
    app.restore_request(key, entry).expect("restore");
    assert!(
        app.take_events()
            .contains(&Event::EditorReplaced { project: key })
    );
    assert_eq!(editor_text(&app, key), sent);
    assert!(app.project(key).expect("open").edited());

    // Another request has no history; coming back shows the last response again.
    let op = lookup(&app, key, "LegacyPort").operation;
    app.new_request(key, &op).expect("new");
    assert!(app.project(key).expect("open").response().is_none());
    app.select_request(key, Some(first)).expect("select");
    let window = app.project(key).expect("open");
    assert_eq!(window.response().expect("response").history, Some(entry));

    app.show_history(key, entry).expect("show");
    assert!(
        app.take_events()
            .contains(&Event::ResponseChanged { project: key })
    );
}

#[test]
fn send_needs_a_server() {
    let setup = Setup::new();
    let (_fake, mut app, key, _, _) = with_requests(&setup, "Legacy");
    assert!(matches!(app.send(key), Err(ModelError::NoServer)));
    assert!(!app.project(key).expect("open").sending());
}

fn customer() -> PathBuf {
    fixtures().join("customer")
}

fn checked(app: &App, target: ImportTarget) -> bool {
    let sheet = app.import_sheet(target).expect("sheet");
    !matches!(sheet.check, CheckState::Checking)
}

#[test]
fn a_missing_include_blocks_create() {
    let setup = Setup::new();
    let (fake, mut app) = setup.launch();
    let target = ImportTarget::NewProject;
    app.begin_import(target);
    app.set_import_destination("Customers", Some(setup.tmp.path().to_owned()))
        .expect("destination");
    // Only the entry WSDL: the imported binding WSDL and the schemas are missing.
    app.set_import_files(target, customer().join("CustomerService.wsdl"), Vec::new())
        .expect("files");
    assert!(app.take_events().contains(&Event::ImportChanged { target }));
    fake.pump_until(&mut app, |app| checked(app, target));
    let sheet = app.import_sheet(target).expect("sheet");
    let CheckState::Done(result) = &sheet.check else {
        panic!("{:?}", sheet.check);
    };
    assert!(result.check.has_errors(), "{:?}", result.check.diagnostics);
    assert!(!sheet.can_finish(target));
    assert!(matches!(
        app.create_project(),
        Err(ModelError::ImportNotReady)
    ));
}

#[test]
fn create_copies_opens_and_suggests_servers() {
    let setup = Setup::new();
    let (fake, mut app) = setup.launch();
    let target = ImportTarget::NewProject;
    app.begin_import(target);
    app.set_import_files(
        target,
        customer().join("CustomerService.wsdl"),
        vec![customer()],
    )
    .expect("files");
    fake.pump_until(&mut app, |app| checked(app, target));
    let sheet = app.import_sheet(target).expect("sheet");
    assert!(!sheet.can_finish(target), "no name and folder yet");
    app.set_import_destination("Customers", Some(setup.tmp.path().to_owned()))
        .expect("destination");
    assert!(app.import_sheet(target).expect("sheet").can_finish(target));

    let key = app.create_project().expect("create");
    assert!(app.import_sheet(target).is_none());
    let folder = setup.tmp.path().join("Customers");
    assert!(folder.join("wsdl/CustomerService.wsdl").is_file());
    assert!(folder.join("wsdl/xsd/customer.xsd").is_file());
    assert_eq!(app.recent_projects()[0], folder);
    assert!(!app.welcome_visible());

    let window = app.project(key).expect("open");
    assert!(
        window.servers().is_empty(),
        "nothing connects before the user confirms"
    );
    let suggested = window.suggested_servers().to_vec();
    assert_eq!(suggested.len(), 1, "only the SOAP 1.1 port: {suggested:?}");
    app.confirm_suggested_server(key, 0, "http://127.0.0.1:9/customers")
        .expect("confirm");
    let window = app.project(key).expect("open");
    assert!(window.suggested_servers().is_empty());
    assert_eq!(window.servers()[0].url, "http://127.0.0.1:9/customers");
    assert_eq!(window.servers()[0].name, suggested[0].port);
}

#[test]
fn replace_wsdl_revalidates_every_request() {
    let setup = Setup::new();
    let (fake, mut app, key, first, second) = with_requests(&setup, "Legacy");
    let target = ImportTarget::ReplaceWsdl(key);
    app.begin_import(target);
    app.set_import_files(
        target,
        customer().join("CustomerService.wsdl"),
        vec![customer()],
    )
    .expect("files");
    fake.pump_until(&mut app, |app| checked(app, target));
    app.replace_wsdl(key).expect("replace");
    let replaced = Event::WsdlReplaced { project: key };
    fake.pump_until(&mut app, |app| app.events.contains(&replaced));
    settle(&fake, &mut app);

    let window = app.project(key).expect("open");
    let outcome = window.replace_outcome().expect("outcome");
    assert!(
        outcome.removed.iter().any(|op| op.operation == "Lookup"),
        "{outcome:?}"
    );
    assert!(!outcome.added.is_empty());
    assert_eq!(outcome.invalid, [first, second], "Lookup is gone");
    assert!(invalid_marker(&app, key, first) && invalid_marker(&app, key, second));
    let services = &window.sidebar().services;
    assert!(
        services.iter().any(|s| s.name != "LegacyService"),
        "{services:?}"
    );
    // The open editor is checked against the new schema too.
    let issues = window.editor().expect("editor").issues();
    assert!(issues.iter().any(Issue::is_error), "{issues:?}");
}

/// A project made from `fixtures/customer` with one CreateOrder request holding the fixture's
/// valid CreateOrder text.
fn customer_editor(setup: &Setup) -> (Fake, App, ProjectKey, String) {
    let (fake, mut app) = setup.launch();
    let target = ImportTarget::NewProject;
    app.begin_import(target);
    app.set_import_destination("Customers", Some(setup.tmp.path().to_owned()))
        .expect("destination");
    app.set_import_files(
        target,
        customer().join("CustomerService.wsdl"),
        vec![customer()],
    )
    .expect("files");
    fake.pump_until(&mut app, |app| checked(app, target));
    let key = app.create_project().expect("create");
    fake.pump_until(&mut app, |app| loaded(app, key));
    let window = app.project(key).expect("open");
    let op = window.sidebar().services[0].ports[0]
        .operations
        .iter()
        .find(|o| o.name() == "CreateOrder")
        .expect("CreateOrder")
        .operation
        .clone();
    app.new_request(key, &op).expect("new");
    let text =
        fs::read_to_string(customer().join("requests/valid-create-order.xml")).expect("fixture");
    set_text(&mut app, key, &text);
    settle(&fake, &mut app);
    (fake, app, key, text)
}

/// UTF-16 offset of `needle` plus `skip`; the fixture is ASCII, so bytes are UTF-16 units.
fn offset(text: &str, needle: &str, skip: usize) -> usize {
    text.find(needle).expect("needle") + skip
}

fn texts(completions: &Completions) -> Vec<&str> {
    completions.items.iter().map(|i| i.text.as_str()).collect()
}

#[test]
fn completes_child_elements_with_the_prefix_in_scope() {
    let setup = Setup::new();
    let (_fake, app, key, text) = customer_editor(&setup);
    let at = offset(&text, "<cus:line", 1);
    let c = app.completions(key, at).expect("completions");
    assert_eq!(texts(&c), ["cus:line", "cus:status"]);
    assert_eq!(c.replace, at..at + "cus:line".len());
    assert_eq!(c.items[0].kind, CompletionKind::Element);
    assert!(
        c.items[0]
            .detail
            .as_deref()
            .expect("detail")
            .contains("1..*")
    );
}

#[test]
fn completes_attributes_values_and_types() {
    let setup = Setup::new();
    let (_fake, app, key, text) = customer_editor(&setup);

    let c = app
        .completions(key, offset(&text, "sku=", 0))
        .expect("attributes");
    assert_eq!(texts(&c), ["sku", "qty"]);
    assert!(
        c.items
            .iter()
            .all(|i| i.detail.as_deref() == Some("required"))
    );

    let c = app
        .completions(key, offset(&text, ">NEW<", 1))
        .expect("values");
    assert_eq!(texts(&c), ["NEW", "SHIPPED", "CANCELLED"]);

    let at = offset(&text, "com:PublicCompany\"", 0);
    let c = app.completions(key, at).expect("types");
    assert_eq!(c.items[0].kind, CompletionKind::Type);
    assert!(texts(&c).contains(&"com:PublicCompany"), "{:?}", texts(&c));
    assert_eq!(c.replace, at..at + "com:PublicCompany".len());

    let c = app
        .completions(key, offset(&text, "</cus:status>", 2))
        .expect("end tag");
    assert_eq!(texts(&c), ["cus:status"]);
}

#[test]
fn hover_shows_the_type_in_effect() {
    let setup = Setup::new();
    let (_fake, app, key, text) = customer_editor(&setup);
    let at = offset(&text, "<cus:party", 3);
    let hover = app.hover(key, at).expect("hover");
    assert_eq!(hover.name, "cus:party");
    let start = offset(&text, "<cus:party", 1);
    assert_eq!(hover.range, start..start + "cus:party".len());
    assert!(
        hover
            .lines
            .contains(&"type com:PublicCompany (declared com:Party)".to_owned()),
        "{hover:?}"
    );
    assert!(hover.lines.contains(&"exactly 1".to_owned()), "{hover:?}");
}

#[test]
fn nothing_is_offered_outside_header_and_body_blocks() {
    let setup = Setup::new();
    let (_fake, app, key, text) = customer_editor(&setup);
    assert_eq!(app.completions(key, offset(&text, "<!-- expect", 6)), None);
    assert_eq!(app.hover(key, offset(&text, "<soapenv:Body", 3)), None);
}

#[test]
fn a_fresh_end_tag_offers_the_open_element() {
    let setup = Setup::new();
    let (_fake, mut app, key, text) = customer_editor(&setup);
    let at = offset(&text, "<cus:status>NEW", "<cus:status>NEW".len());
    let end = offset(&text, "</cus:status>", "</cus:status>".len());
    app.edit(key, at..end, "</").expect("edit");
    let c = app.completions(key, at + 2).expect("end tag");
    assert_eq!(texts(&c), ["cus:status"]);
}
