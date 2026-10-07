//! The main menu, built in code (no nib).
//!
//! Items without a target go up the responder chain: text views handle Edit, the project
//! window's controller the Project menu, the app delegate the rest. An item nobody answers is
//! disabled by AppKit's automatic enabling, which is how Project items grey out while only the
//! welcome window is open.

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

const CMD: NSEventModifierFlags = NSEventModifierFlags::Command;
const SHIFT_CMD: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Shift.0);
const OPT_CMD: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Option.0);
const NONE: NSEventModifierFlags = NSEventModifierFlags(0);

/// `NSFindPanelAction` values, sent as the item's tag with `performFindPanelAction:`.
const FIND_SHOW: isize = 1;
const FIND_NEXT: isize = 2;
const FIND_PREVIOUS: isize = 3;
const FIND_SET_FIND_STRING: isize = 7;

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
    // No action: disabled until there are settings.
    Entry::Item {
        title: "Settings…",
        action: None,
        key: ",",
        modifiers: CMD,
        tag: 0,
    },
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
    item("New Project…", c"newProject:", "n", SHIFT_CMD),
    item("Open Project…", c"openProject:", "o", CMD),
    // NSDocumentController fills the submenu that holds `clearRecentDocuments:`.
    Entry::Submenu(
        "Open Recent",
        Special::None,
        &[item("Clear Menu", c"clearRecentDocuments:", "", NONE)],
    ),
    Entry::Separator,
    item("Close", c"performClose:", "w", CMD),
    item("Save All", c"saveAll:", "s", CMD),
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
            find("Find Next", "g", CMD, FIND_NEXT),
            find("Find Previous", "g", SHIFT_CMD, FIND_PREVIOUS),
            find("Use Selection for Find", "e", CMD, FIND_SET_FIND_STRING),
        ],
    ),
];

const PROJECT: &[Entry] = &[
    item("New Request", c"newRequest:", "n", CMD),
    item("Duplicate", c"duplicateRequest:", "d", CMD),
    item("Rename", c"renameRequest:", "", NONE),
    item("Delete", c"deleteRequest:", BACKSPACE, CMD),
    Entry::Separator,
    item("Validate", c"validateRequest:", "b", CMD),
    item("Send", c"sendRequest:", "\r", CMD),
    Entry::Separator,
    item("Replace WSDL…", c"replaceWsdl:", "", NONE),
    item("Project Settings…", c"projectSettings:", "", NONE),
];

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
    Entry::Submenu("Project", Special::None, PROJECT),
    Entry::Submenu("Window", Special::Window, WINDOW),
];

/// Builds the main menu and installs it on `app`, including the Services and Window menus
/// AppKit maintains.
pub fn install(app: &NSApplication, mtm: MainThreadMarker) {
    let menu = build("", MAIN, app, mtm);
    app.setMainMenu(Some(&menu));
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
                let action = action.map(Sel::register);
                // SAFETY: no target is set, so the action goes up the responder chain, where
                // every receiver takes the sender as its only argument.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        action,
                        &NSString::from_str(key),
                    )
                };
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
                }
            }
        }
    }
    menu
}
