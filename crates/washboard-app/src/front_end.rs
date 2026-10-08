//! The model's front-end traits on AppKit and GCD (PLAN §2.1).
//!
//! None of these call back into the model synchronously: the model is borrowed while it calls
//! them, so every answer arrives on a later main-queue turn, through the app delegate.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime, MainThreadBound};
use objc2::rc::{Retained, Weak};
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSModalResponse,
    NSModalResponseOK, NSOpenPanel, NSWindow,
};
use objc2_foundation::NSString;
use washboard_ui_model::{
    Alert, Confirm, DialogAnswer, DialogId, Dialogs, MainThread, TimerId, Timers,
};

use crate::app::AppDelegate;

/// The app delegate, reachable from any thread; only loaded on the main thread.
pub(crate) type DelegateRef = Arc<MainThreadBound<Weak<AppDelegate>>>;

pub(crate) fn delegate_ref(delegate: &AppDelegate, mtm: MainThreadMarker) -> DelegateRef {
    Arc::new(MainThreadBound::new(Weak::from(delegate), mtm))
}

/// Runs `f` with the delegate on a later turn of the main queue, if the delegate still exists.
fn later(delegate: &DelegateRef, f: impl FnOnce(&AppDelegate, MainThreadMarker) + Send + 'static) {
    let delegate = delegate.clone();
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
        if let Some(delegate) = delegate.get(mtm).load() {
            f(&delegate, mtm);
        }
    });
}

/// Wakes the main thread with one main-queue block per burst of worker results.
#[derive(Debug)]
pub(crate) struct Wake {
    pending: Arc<AtomicBool>,
    delegate: DelegateRef,
}

impl Wake {
    pub(crate) fn new(delegate: DelegateRef) -> Wake {
        Wake {
            pending: Arc::new(AtomicBool::new(false)),
            delegate,
        }
    }
}

impl MainThread for Wake {
    fn wake(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending = self.pending.clone();
        later(&self.delegate, move |delegate, _| {
            // Cleared before pumping, so a result queued during the pump wakes again.
            pending.store(false, Ordering::Release);
            delegate.pump();
        });
    }
}

/// One-shot timers as delayed main-queue blocks. A block that finds its timer restarted or
/// cancelled since (a newer generation, or none) does nothing.
#[derive(Debug)]
pub(crate) struct DispatchTimers {
    delegate: DelegateRef,
    generations: Rc<RefCell<HashMap<TimerId, u64>>>,
    next: Cell<u64>,
}

impl DispatchTimers {
    pub(crate) fn new(
        delegate: DelegateRef,
        generations: Rc<RefCell<HashMap<TimerId, u64>>>,
    ) -> DispatchTimers {
        DispatchTimers {
            delegate,
            generations,
            next: Cell::new(0),
        }
    }
}

impl Timers for DispatchTimers {
    fn start(&self, id: TimerId, after: Duration) {
        let generation = self.next.get() + 1;
        self.next.set(generation);
        self.generations.borrow_mut().insert(id, generation);
        let Ok(when) = DispatchTime::try_from(after) else {
            return;
        };
        let delegate = self.delegate.clone();
        let fire = move || {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            if let Some(delegate) = delegate.get(mtm).load() {
                delegate.timer_due(id, generation);
            }
        };
        if DispatchQueue::main().after(when, fire).is_err() {
            eprintln!("washboard-app: could not schedule a timer");
        }
    }

    fn cancel(&self, id: TimerId) {
        self.generations.borrow_mut().remove(&id);
    }
}

/// Open panels and alerts. Shown as sheets on the key window when there is one, else as
/// app-modal windows.
#[derive(Debug)]
pub(crate) struct AppKitDialogs {
    delegate: DelegateRef,
}

impl AppKitDialogs {
    pub(crate) fn new(delegate: DelegateRef) -> AppKitDialogs {
        AppKitDialogs { delegate }
    }
}

impl Dialogs for AppKitDialogs {
    fn choose_project_folder(&self, id: DialogId) {
        later(&self.delegate, move |delegate, mtm| {
            let delegate = delegate.retain();
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(false);
            panel.setCanChooseDirectories(true);
            panel.setAllowsMultipleSelection(false);
            panel.setPrompt(Some(&NSString::from_str("Open")));
            panel.setMessage(Some(&NSString::from_str(
                "Choose a Washboard project folder.",
            )));
            let chosen = panel.clone();
            let handler = RcBlock::new(move |response: NSModalResponse| {
                let folder = (response == NSModalResponseOK)
                    .then(|| chosen.URL()?.to_file_path())
                    .flatten();
                let answer = folder.map_or(DialogAnswer::Cancelled, DialogAnswer::Folder);
                delegate.dialog_answered(id, answer);
            });
            panel.beginWithCompletionHandler(&handler);
        });
    }

    fn alert(&self, alert: Alert) {
        later(&self.delegate, move |_, mtm| {
            let ns = NSAlert::new(mtm);
            ns.setMessageText(&NSString::from_str(&alert.title));
            ns.setInformativeText(&NSString::from_str(&alert.message));
            show(&ns, None, mtm);
        });
    }

    fn confirm(&self, id: DialogId, confirm: Confirm) {
        later(&self.delegate, move |delegate, mtm| {
            let delegate = delegate.retain();
            let ns = NSAlert::new(mtm);
            ns.setAlertStyle(NSAlertStyle::Warning);
            ns.setMessageText(&NSString::from_str(&confirm.title));
            ns.setInformativeText(&NSString::from_str(&confirm.message));
            ns.addButtonWithTitle(&NSString::from_str(&confirm.action));
            ns.addButtonWithTitle(&NSString::from_str("Cancel"));
            let answer = move |response: NSModalResponse| {
                let answer = if response == NSAlertFirstButtonReturn {
                    DialogAnswer::Confirmed
                } else {
                    DialogAnswer::Cancelled
                };
                delegate.dialog_answered(id, answer);
            };
            show(&ns, Some(Box::new(answer)), mtm);
        });
    }
}

/// As a sheet on the key window, or app-modal without one.
fn show(alert: &NSAlert, answer: Option<Box<dyn Fn(NSModalResponse)>>, mtm: MainThreadMarker) {
    match parent_window(mtm) {
        Some(window) => {
            let handler = answer.map(|answer| RcBlock::new(move |r: NSModalResponse| answer(r)));
            alert.beginSheetModalForWindow_completionHandler(&window, handler.as_deref());
        }
        None => {
            let response = alert.runModal();
            if let Some(answer) = answer {
                answer(response);
            }
        }
    }
}

/// The window a sheet should attach to: the key window, unless it already has one.
fn parent_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    app.keyWindow()
        .or_else(|| app.mainWindow())
        .filter(|w| w.attachedSheet().is_none() && w.isVisible())
}

/// Where the app keeps `state.json`: `~/Library/Application Support/Washboard`.
pub(crate) fn default_state_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    home.join("Library/Application Support/Washboard")
}
