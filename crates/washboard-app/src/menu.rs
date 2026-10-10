//! The main menu, built in code (no nib).
//!
//! Items without a target go up the responder chain: text views handle Edit, the project
//! window's controller the Project menu, the app delegate the rest. An item nobody answers is
//! disabled by AppKit's automatic enabling, which is how Project items grey out while only the
//! welcome window is open.

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

use crate::welcome::RecentProject;

/// A menu item sending `action`. Every action the app's menus send, its own and AppKit's
/// (`copy:`, `performFindPanelAction:`, …), takes the sender as its only argument and returns
/// nothing, whether it reaches a target set later or goes up the responder chain.
pub fn menu_item(
    title: &str,
    action: Option<Sel>,
    key: &str,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: `action` is `None` or an action method taking the sender, as above.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

const CMD: NSEventModifierFlags = NSEventModifierFlags::Command;
const SHIFT_CMD: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Shift.0);
const OPT_CMD: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Option.0);
const CTRL_CMD: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Control.0);
const CTRL: NSEventModifierFlags = NSEventModifierFlags::Control;
const NONE: NSEventModifierFlags = NSEventModifierFlags(0);

/// `NSFindPanelAction` values, sent as the item's tag with `performFindPanelAction:`.
const FIND_SHOW: isize = 1;
const FIND_NEXT: isize = 2;
const FIND_PREVIOUS: isize = 3;
const FIND_SET_FIND_STRING: isize = 7;
const FIND_REPLACE: isize = 12;

const OPEN_RECENT: &str = "Recent Projects";
const CLEAR_MENU: &str = "Clear Menu";

/// `NSBackspaceCharacter`: the key equivalent AppKit shows as ⌫.
const BACKSPACE: &str = "\u{8}";

enum Entry {
    Item {
        title: &'static str,
        action: Option<&'static CStr>,
        key: &'static str,
        modifiers: NSEventModifierFlags,
        tag: isize,
    },
    Separator,
    Submenu(&'static str, Special, &'static [Entry]),
}

/// Submenus AppKit manages once it is told about them.
#[derive(Clone, Copy, PartialEq)]
enum Special {
    None,
    Services,
    Window,
    Help,
}

const fn item(
    title: &'static str,
    action: &'static CStr,
    key: &'static str,
    modifiers: NSEventModifierFlags,
) -> Entry {
    Entry::Item {
        title,
        action: Some(action),
        key,
        modifiers,
        tag: 0,
    }
}

const fn find(
    title: &'static str,
    key: &'static str,
    modifiers: NSEventModifierFlags,
    tag: isize,
) -> Entry {
    Entry::Item {
        title,
        action: Some(c"performFindPanelAction:"),
        key,
        modifiers,
        tag,
    }
}

const APP: &[Entry] = &[
    item(
        "About Washboard",
        c"orderFrontStandardAboutPanel:",
        "",
        NONE,
    ),
    Entry::Separator,
    item("Settings…", c"showSettings:", ",", CMD),
    Entry::Separator,
    Entry::Submenu("Services", Special::Services, &[]),
    Entry::Separator,
    item("Hide Washboard", c"hide:", "h", CMD),
    item("Hide Others", c"hideOtherApplications:", "h", OPT_CMD),
    item("Show All", c"unhideAllApplications:", "", NONE),
    Entry::Separator,
    item("Quit Washboard", c"terminate:", "q", CMD),
];

const FILE: &[Entry] = &[
    Entry::Submenu(
        "New",
        Special::None,
        &[
            item("New Project…", c"newProject:", "n", SHIFT_CMD),
            Entry::Separator,
            item("New Request…", c"newRequest:", "n", CMD),
        ],
    ),
    item("Open Project…", c"openProject:", "o", CMD),
    // Refilled from the model's recent projects by `set_recent_projects`.
    Entry::Submenu(
        OPEN_RECENT,
        Special::None,
        &[item(CLEAR_MENU, c"clearRecentProjects:", "", NONE)],
    ),
    item("Close Project", c"performClose:", "w", CMD),
    item("Save All", c"saveAll:", "s", CMD),
    Entry::Separator,
    item("Duplicate Request", c"duplicateRequest:", "d", CMD),
    item("Rename Request", c"renameRequest:", "", NONE),
    item("Delete Request", c"deleteRequest:", BACKSPACE, CMD),
    Entry::Separator,
    item("Send", c"sendRequest:", "\r", CMD),
    // Enabled only while a send is in flight. The toolbar's Send turns into Cancel instead;
    // the menu keeps both, so pressing ⌘↩ twice never cancels the first send.
    item("Cancel Send", c"cancelSend:", ".", CMD),
];

const EDIT: &[Entry] = &[
    item("Undo", c"undo:", "z", CMD),
    item("Redo", c"redo:", "z", SHIFT_CMD),
    Entry::Separator,
    item("Cut", c"cut:", "x", CMD),
    item("Copy", c"copy:", "c", CMD),
    item("Paste", c"paste:", "v", CMD),
    item("Select All", c"selectAll:", "a", CMD),
    Entry::Separator,
    Entry::Submenu(
        "Find",
        Special::None,
        &[
            find("Find…", "f", CMD, FIND_SHOW),
            find("Find and Replace…", "f", OPT_CMD, FIND_REPLACE),
            find("Find Next", "g", CMD, FIND_NEXT),
            find("Find Previous", "g", SHIFT_CMD, FIND_PREVIOUS),
            find("Use Selection for Find", "e", CMD, FIND_SET_FIND_STRING),
            item(
                "Jump to Selection",
                c"centerSelectionInVisibleArea:",
                "j",
                CMD,
            ),
        ],
    ),
    Entry::Separator,
    // Answered by the project window, enabled while it has a request open.
    item("Format XML", c"formatXML:", "i", CTRL),
    item("Validate", c"validateRequest:", "b", CMD),
];

// `toggleSidebar:` reaches the project window's split view controller, which also retitles
// the item Show or Hide Sidebar.
const VIEW: &[Entry] = &[item("Show Sidebar", c"toggleSidebar:", "s", CTRL_CMD)];

const WINDOW: &[Entry] = &[
    item("Minimize", c"performMiniaturize:", "m", CMD),
    item("Zoom", c"performZoom:", "", NONE),
    Entry::Separator,
    item("HTTP Log", c"showHttpLog:", "l", OPT_CMD),
    Entry::Separator,
    item("Bring All to Front", c"arrangeInFront:", "", NONE),
];

const MAIN: &[Entry] = &[
    Entry::Submenu("Washboard", Special::None, APP),
    Entry::Submenu("File", Special::None, FILE),
    Entry::Submenu("Edit", Special::None, EDIT),
    Entry::Submenu("View", Special::None, VIEW),
    Entry::Submenu("Window", Special::Window, WINDOW),
    // Empty until there is documentation: registered as the Help menu, it gets the system's
    // menu search field.
    Entry::Submenu("Help", Special::Help, &[]),
];

/// Builds the main menu and installs it on `app`, including the Services, Window and Help
/// menus AppKit maintains.
pub fn install(app: &NSApplication, mtm: MainThreadMarker) {
    let menu = build("", MAIN, app, mtm);
    app.setMainMenu(Some(&menu));
}

/// Refills File ▸ Open Recent: one item per project, most recent first, whose tag is its index
/// in the model's list, then Clear Menu.
pub(crate) fn set_recent_projects(app: &NSApplication, recent: &[RecentProject]) {
    let mtm = app.mtm();
    let submenu = app
        .mainMenu()
        .and_then(|m| m.itemWithTitle(&NSString::from_str("File")))
        .and_then(|i| i.submenu())
        .and_then(|m| m.itemWithTitle(&NSString::from_str(OPEN_RECENT)))
        .and_then(|i| i.submenu());
    let Some(submenu) = submenu else {
        return;
    };
    submenu.removeAllItems();
    for (index, project) in recent.iter().enumerate() {
        // No target: the action goes up the responder chain to the app delegate.
        let item = menu_item(&project.name, Some(sel!(openRecentProject:)), "", mtm);
        item.setTag(index as isize);
        item.setToolTip(Some(&NSString::from_str(&project.path)));
        submenu.addItem(&item);
    }
    if !recent.is_empty() {
        submenu.addItem(&NSMenuItem::separatorItem(mtm));
    }
    let clear = menu_item(CLEAR_MENU, Some(sel!(clearRecentProjects:)), "", mtm);
    submenu.addItem(&clear);
}

fn build(
    title: &str,
    entries: &[Entry],
    app: &NSApplication,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for entry in entries {
        match entry {
            Entry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Entry::Item {
                title,
                action,
                key,
                modifiers,
                tag,
            } => {
                // No target: the action goes up the responder chain.
                let item = menu_item(title, action.map(Sel::register), key, mtm);
                item.setKeyEquivalentModifierMask(*modifiers);
                item.setTag(*tag);
                menu.addItem(&item);
            }
            Entry::Submenu(title, special, entries) => {
                let submenu = build(title, entries, app, mtm);
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(title));
                item.setSubmenu(Some(&submenu));
                menu.addItem(&item);
                match special {
                    Special::None => {}
                    Special::Services => app.setServicesMenu(Some(&submenu)),
                    Special::Window => app.setWindowsMenu(Some(&submenu)),
                    Special::Help => app.setHelpMenu(Some(&submenu)),
                }
            }
        }
    }
    menu
}
