//! Model tests against the fake front end. Projects are created in temp dirs from `fixtures/`.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use washboard_core::project::{Project, STATE_FILE, WsdlFile, WsdlSet};

use crate::fake::Fake;
use crate::{App, DialogAnswer, Event};

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

    app.close_project(ka);
    assert_eq!(app.take_events(), [Event::ProjectClosed { project: ka }]);
    app.close_project(kb);
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
        app.quit().expect("quit");
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
        app.quit().expect("quit");
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
