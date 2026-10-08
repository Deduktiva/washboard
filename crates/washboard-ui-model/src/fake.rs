//! A front end for tests: nothing runs on its own. Timers fire when the test advances the
//! manual clock, dialogs are recorded and answered by the test, and wakes are counted so a
//! test can wait for a worker without sleeping blindly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use washboard_core::secrets::MemorySecretStore;

use crate::front_end::{Alert, DialogId, Dialogs, FrontEnd, MainThread, TimerId, Timers};
use crate::{App, DialogAnswer};

#[derive(Debug, Default)]
pub struct FakeMainThread {
    wakes: Mutex<u64>,
    woken: Condvar,
}

impl MainThread for FakeMainThread {
    fn wake(&self) {
        if let Ok(mut n) = self.wakes.lock() {
            *n += 1;
            self.woken.notify_all();
        }
    }
}

#[derive(Debug, Default)]
pub struct Clock {
    pub now: Duration,
    /// Running timers and when they fire.
    pub timers: HashMap<TimerId, Duration>,
}

#[derive(Debug, Clone, Default)]
struct FakeTimers(Rc<RefCell<Clock>>);

impl Timers for FakeTimers {
    fn start(&self, id: TimerId, after: Duration) {
        let mut clock = self.0.borrow_mut();
        let at = clock.now + after;
        clock.timers.insert(id, at);
    }

    fn cancel(&self, id: TimerId) {
        self.0.borrow_mut().timers.remove(&id);
    }
}

#[derive(Debug, Default)]
pub struct DialogLog {
    pub folder_requests: Vec<DialogId>,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, Default)]
struct FakeDialogs(Rc<RefCell<DialogLog>>);

impl Dialogs for FakeDialogs {
    fn choose_project_folder(&self, id: DialogId) {
        self.0.borrow_mut().folder_requests.push(id);
    }

    fn alert(&self, alert: Alert) {
        self.0.borrow_mut().alerts.push(alert);
    }
}

/// The handles a test keeps after giving the front end to an [`App`].
#[derive(Debug)]
pub struct Fake {
    pub main_thread: Arc<FakeMainThread>,
    pub clock: Rc<RefCell<Clock>>,
    pub dialogs: Rc<RefCell<DialogLog>>,
}

impl Fake {
    pub fn new() -> (Fake, FrontEnd) {
        let main_thread = Arc::new(FakeMainThread::default());
        let timers = FakeTimers::default();
        let dialogs = FakeDialogs::default();
        let secrets = Arc::new(MemorySecretStore::default());
        let fake = Fake {
            main_thread: main_thread.clone(),
            clock: timers.0.clone(),
            dialogs: dialogs.0.clone(),
        };
        let front = FrontEnd {
            main_thread,
            timers: Box::new(timers),
            dialogs: Box::new(dialogs),
            secrets,
        };
        (fake, front)
    }

    /// Waits until the main thread has been woken `n` times in total, then pumps.
    pub fn pump_after_wakes(&self, app: &mut App, n: u64) {
        let guard = self.main_thread.wakes.lock().expect("wake counter");
        let (_guard, timeout) = self
            .main_thread
            .woken
            .wait_timeout_while(guard, Duration::from_secs(10), |w| *w < n)
            .expect("wake counter");
        assert!(!timeout.timed_out(), "worker did not wake the main thread");
        app.pump();
    }

    /// Answers the most recent open-panel request.
    pub fn answer_folder(&self, app: &mut App, answer: DialogAnswer) {
        let id = self
            .dialogs
            .borrow_mut()
            .folder_requests
            .pop()
            .expect("an open panel was requested");
        app.dialog_answered(id, answer);
    }

    pub fn alerts(&self) -> Vec<Alert> {
        std::mem::take(&mut self.dialogs.borrow_mut().alerts)
    }
}
