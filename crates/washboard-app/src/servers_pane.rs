//! A project's Servers pane in the Settings window: the project's servers in a list, each
//! with an Edit… button that opens it in a sheet, and the servers the WSDL suggests.
//!
//! A sheet, as System Settings edits a network's details, rather than a form under the list:
//! that form showed with no server selected, and which server it edited followed a list
//! selection. The sheet edits a copy; Save writes it through the model, Cancel drops it.

use std::cell::{Cell, OnceCell, RefCell};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSButton, NSControlStateValueOn, NSLayoutPriorityRequired, NSSecureTextField, NSSwitch,
    NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSSize, NSString, ns_string};
use washboard_core::model::{Auth, Server, ServerId};
use washboard_ui_model::{ProjectKey, SuggestedServer};

use crate::app::ModelAccess;
use crate::form;
use crate::layout::{self, view};
use crate::settings_window::{project_note, state, target_button};

const TIMEOUT_WIDTH: f64 = 60.0;
const SHEET_WIDTH: f64 = 480.0;
/// A new server's timeout, until the user picks another; the model's default.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The sheet's controls.
#[derive(Debug)]
struct ServerForm {
    title: Retained<NSTextField>,
    name: Retained<NSTextField>,
    url: Retained<NSTextField>,
    ignore_tls: Retained<NSSwitch>,
    auth_none: Retained<NSButton>,
    auth_basic: Retained<NSButton>,
    user: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    timeout: Retained<NSTextField>,
    /// The User and Password rows, shown with Basic auth only.
    user_row: Retained<NSView>,
    password_row: Retained<NSView>,
    delete: Retained<NSButton>,
}

#[derive(Debug)]
pub struct ServersIvars {
    key: ProjectKey,
    /// The model's servers as last shown.
    servers: RefCell<Vec<Server>>,
    suggested: RefCell<Vec<SuggestedServer>>,
    view: OnceCell<Retained<NSView>>,
    list: OnceCell<Retained<NSBox>>,
    /// The suggestions' section and the group its rows go in.
    suggestions: OnceCell<(Retained<NSView>, Retained<NSBox>)>,
    sheet: OnceCell<Retained<NSWindow>>,
    body: OnceCell<Retained<NSView>>,
    form: OnceCell<ServerForm>,
    /// What the sheet edits while it is open: `Some(None)` for a server not added yet.
    editing: Cell<Option<Option<ServerId>>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ServersPane` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ServersIvars]
    #[derive(Debug)]
    pub struct ServersPane;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ServersPane {}

    impl ServersPane {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(authChanged:))]
        fn auth_changed(&self, _sender: Option<&AnyObject>) {
            self.show_auth_rows();
        }

        #[unsafe(method(addServer:))]
        fn add_server_action(&self, _sender: Option<&AnyObject>) {
            self.add_server();
        }

        #[unsafe(method(editServer:))]
        fn edit_server_action(&self, sender: Option<&AnyObject>) {
            self.edit(tag(sender));
        }

        #[unsafe(method(saveServer:))]
        fn save_action(&self, _sender: Option<&AnyObject>) {
            self.save();
        }

        #[unsafe(method(cancelServer:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) {
            self.cancel();
        }

        #[unsafe(method(deleteServer:))]
        fn delete_action(&self, _sender: Option<&AnyObject>) {
            self.delete();
        }

        #[unsafe(method(confirmSuggestion:))]
        fn confirm_suggestion_action(&self, sender: Option<&AnyObject>) {
            self.confirm_suggestion(tag(sender));
        }
    }
);

impl ServersPane {
    pub(crate) fn new(key: ProjectKey, project: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ServersIvars {
            key,
            servers: RefCell::new(Vec::new()),
            suggested: RefCell::new(Vec::new()),
            view: OnceCell::new(),
            list: OnceCell::new(),
            suggestions: OnceCell::new(),
            sheet: OnceCell::new(),
            body: OnceCell::new(),
            form: OnceCell::new(),
            editing: Cell::new(None),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let list = form::group(vec![], mtm);
        let suggestion_rows = form::group(vec![], mtm);
        let suggestions = form::Section::new(mtm)
            .header(&form::header("Suggested by the WSDL", mtm))
            .group(&suggestion_rows)
            .text("Servers named by the WSDL's addresses. Add one to use it.")
            .build();
        let view = form::page(
            vec![
                project_note(project, mtm),
                form::Section::new(mtm)
                    .header(&form::header("Servers", mtm))
                    .group(&list)
                    .build(),
                suggestions.clone(),
            ],
            mtm,
        );
        let (sheet, body, form) = this.server_sheet(mtm);

        let ivars = this.ivars();
        let _ = ivars.view.set(view);
        let _ = ivars.list.set(list);
        let _ = ivars.suggestions.set((suggestions, suggestion_rows));
        let _ = ivars.sheet.set(sheet);
        let _ = ivars.body.set(body);
        let _ = ivars.form.set(form);
        this.reload();
        this
    }

    /// The pane's view, which the window shows while the pane is selected.
    pub fn view(&self) -> &Retained<NSView> {
        self.ivars().view.get().expect("set in new()")
    }

    /// The server sheet, attached to the Settings window while a server is edited.
    pub fn sheet(&self) -> &NSWindow {
        self.ivars().sheet.get().expect("set in new()")
    }

    /// The sheet's content, laid out by `form`.
    pub fn sheet_body(&self) -> &NSView {
        self.ivars().body.get().expect("set in new()")
    }

    pub fn servers(&self) -> Vec<Server> {
        self.ivars().servers.borrow().clone()
    }

    /// The servers the list shows: name and address.
    pub fn server_rows(&self) -> Vec<Vec<String>> {
        self.ivars()
            .servers
            .borrow()
            .iter()
            .map(|s| vec![s.name.clone(), s.url.clone()])
            .collect()
    }

    /// The servers suggested by the WSDL's `soap:address`es, not yet confirmed: port, address.
    pub fn suggestion_rows(&self) -> Vec<Vec<String>> {
        self.ivars()
            .suggested
            .borrow()
            .iter()
            .map(|s| vec![s.port.clone(), s.url.clone()])
            .collect()
    }

    /// Whether the suggestions' section is shown.
    pub fn shows_suggestions(&self) -> bool {
        !self
            .ivars()
            .suggestions
            .get()
            .expect("set in new()")
            .0
            .isHidden()
    }

    /// The row of the server the sheet edits; `None` while it adds one or is closed.
    pub fn editing(&self) -> Option<usize> {
        let id = self.ivars().editing.get()??;
        self.row_of(id)
    }

    /// Whether the sheet is open.
    pub fn is_editing(&self) -> bool {
        self.ivars().editing.get().is_some()
    }

    /// Whether the sheet shows the User and Password rows, which Basic auth needs.
    pub fn shows_credentials(&self) -> bool {
        // `form::set_row_shown` hides the row's wrapper, not the row itself.
        !self.form().user_row.isHiddenOrHasHiddenAncestor()
    }

    pub fn name_field(&self) -> &NSTextField {
        &self.form().name
    }

    pub fn url_field(&self) -> &NSTextField {
        &self.form().url
    }

    pub fn user_field(&self) -> &NSTextField {
        &self.form().user
    }

    pub fn password_field(&self) -> &NSSecureTextField {
        &self.form().password
    }

    pub fn basic_auth_button(&self) -> &NSButton {
        &self.form().auth_basic
    }

    pub fn delete_button(&self) -> &NSButton {
        &self.form().delete
    }

    /// Shows the model's servers and suggestions.
    pub fn reload(&self) {
        let key = self.ivars().key;
        let (servers, suggested) = self
            .read(|app| {
                app.project(key)
                    .map(|w| (w.servers().to_vec(), w.suggested_servers().to_vec()))
            })
            .flatten()
            .unwrap_or_default();
        self.show_suggestions(&suggested);
        *self.ivars().suggested.borrow_mut() = suggested;
        *self.ivars().servers.borrow_mut() = servers;
        self.show_list();
    }

    /// Opens the sheet on server `row`.
    pub fn edit(&self, row: usize) {
        let Some(server) = self.ivars().servers.borrow().get(row).cloned() else {
            return;
        };
        self.ivars().editing.set(Some(Some(server.id)));
        self.load(&server, &server.name);
        self.form().delete.setHidden(false);
        self.present();
    }

    /// Opens the sheet on a new server, which Save adds.
    pub fn add_server(&self) {
        let server = Server {
            id: ServerId::new(),
            name: String::new(),
            url: String::new(),
            ignore_tls_errors: false,
            auth: Auth::None,
            timeout: DEFAULT_TIMEOUT,
        };
        self.ivars().editing.set(Some(None));
        self.load(&server, "New Server");
        self.form().delete.setHidden(true);
        self.present();
    }

    /// Writes the sheet's server through the model and closes the sheet. A typed password goes
    /// to the secret store; an empty one leaves the stored one alone.
    pub fn save(&self) {
        let Some(editing) = self.ivars().editing.get() else {
            return;
        };
        let key = self.ivars().key;
        let id = match editing {
            Some(id) => id,
            None => match self.command("Could not add a server", |app| app.add_server(key)) {
                // From here on the sheet edits the added server, so that a second Save after
                // a failed one does not add another.
                Some(id) => {
                    self.ivars().editing.set(Some(Some(id)));
                    id
                }
                None => return,
            },
        };
        let current = self
            .read(|app| {
                app.project(key)
                    .and_then(|w| w.servers().iter().find(|s| s.id == id).cloned())
            })
            .flatten();
        let Some(current) = current else {
            // Deleted elsewhere while the sheet was open.
            self.close_sheet();
            return;
        };
        let (server, password) = self.edited(&current);
        if server != current || password.is_some() {
            let saved = self.command("Could not save the server", |app| {
                app.update_server(key, &server, password.as_deref())
            });
            if saved.is_none() {
                return;
            }
        }
        self.close_sheet();
    }

    /// Closes the sheet without saving.
    pub fn cancel(&self) {
        self.close_sheet();
    }

    /// Deletes the server the sheet edits and closes the sheet.
    pub fn delete(&self) {
        let Some(Some(id)) = self.ivars().editing.get() else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not delete the server", |app| {
            app.delete_server(key, id)
        });
        self.close_sheet();
    }

    /// Adds suggestion `row` as a server with its address as suggested.
    pub fn confirm_suggestion(&self, row: usize) {
        let Some(url) = self
            .ivars()
            .suggested
            .borrow()
            .get(row)
            .map(|s| s.url.clone())
        else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not add the server", |app| {
            app.confirm_suggested_server(key, row, &url)
        });
    }

    /// `current` with the sheet's values, and the password to store, if one was typed.
    fn edited(&self, current: &Server) -> (Server, Option<String>) {
        let form = self.form();
        let basic_auth = form.auth_basic.state() == NSControlStateValueOn;
        let mut server = current.clone();
        let name = form.name.stringValue().to_string();
        if !name.trim().is_empty() {
            server.name = name;
        }
        server.url = form.url.stringValue().to_string();
        server.ignore_tls_errors = form.ignore_tls.state() == NSControlStateValueOn;
        server.auth = if basic_auth {
            Auth::Basic {
                username: form.user.stringValue().to_string(),
            }
        } else {
            Auth::None
        };
        // An unparsable or zero timeout keeps the stored one.
        if let Ok(secs) = form.timeout.stringValue().to_string().trim().parse::<u64>()
            && secs > 0
        {
            server.timeout = Duration::from_secs(secs);
        }
        let password = form.password.stringValue().to_string();
        let password = (basic_auth && !password.is_empty()).then_some(password);
        (server, password)
    }

    fn load(&self, server: &Server, title: &str) {
        let form = self.form();
        let (basic_auth, user) = match &server.auth {
            Auth::None => (false, ""),
            Auth::Basic { username } => (true, username.as_str()),
        };
        form.title.setStringValue(&NSString::from_str(title));
        form.name.setStringValue(&NSString::from_str(&server.name));
        form.url.setStringValue(&NSString::from_str(&server.url));
        form.ignore_tls.setState(state(server.ignore_tls_errors));
        form.auth_none.setState(state(!basic_auth));
        form.auth_basic.setState(state(basic_auth));
        form.user.setStringValue(&NSString::from_str(user));
        form.password.setStringValue(ns_string!(""));
        form.timeout
            .setStringValue(&NSString::from_str(&server.timeout.as_secs().to_string()));
        self.show_auth_rows();
    }

    fn present(&self) {
        let sheet = self.sheet();
        form::fit_window(sheet, self.sheet_body());
        sheet.makeFirstResponder(Some(&self.form().name));
        if sheet.sheetParent().is_none()
            && let Some(parent) = self.view().window()
        {
            parent.beginSheet_completionHandler(sheet, None);
        }
    }

    fn close_sheet(&self) {
        self.ivars().editing.set(None);
        let sheet = self.sheet();
        // Ending the field editor before the sheet goes.
        sheet.makeFirstResponder(None);
        match sheet.sheetParent() {
            Some(parent) => parent.endSheet(sheet),
            None => sheet.orderOut(None),
        }
    }

    /// User and Password only with Basic auth; the sheet follows their height.
    fn show_auth_rows(&self) {
        let form = self.form();
        let basic_auth = form.auth_basic.state() == NSControlStateValueOn;
        form::set_row_shown(&form.user_row, basic_auth);
        form::set_row_shown(&form.password_row, basic_auth);
        form::fit_window(self.sheet(), self.sheet_body());
    }

    fn row_of(&self, id: ServerId) -> Option<usize> {
        self.ivars()
            .servers
            .borrow()
            .iter()
            .position(|s| s.id == id)
    }

    fn form(&self) -> &ServerForm {
        self.ivars().form.get().expect("set in new()")
    }

    /// One row per server: its name and address, and Edit…; then Add Server….
    fn show_list(&self) {
        let mtm = self.mtm();
        let mut rows: Vec<Retained<NSView>> = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let edit = target_button("Edit…", self, sel!(editServer:), mtm);
                edit.setTag(NSInteger::try_from(i).unwrap_or(0));
                form::subtitle_row(&s.name, &s.url, &edit, mtm)
            })
            .collect();
        if rows.is_empty() {
            rows.push(form::text_row("No servers", mtm));
        }
        let add = target_button("Add Server…", self, sel!(addServer:), mtm);
        rows.push(form::button_row(vec![view(add)], mtm));
        let list = self.ivars().list.get().expect("set in new()");
        form::set_rows(list, rows, mtm);
    }

    /// One row per suggestion: its port and address, and an Add button.
    fn show_suggestions(&self, suggested: &[SuggestedServer]) {
        let mtm = self.mtm();
        let (section, rows) = self.ivars().suggestions.get().expect("set in new()");
        form::set_shown(section, !suggested.is_empty());
        let rows_now = suggested
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let add = target_button("Add", self, sel!(confirmSuggestion:), mtm);
                add.setTag(NSInteger::try_from(i).unwrap_or(0));
                form::subtitle_row(&s.port, &s.url, &add, mtm)
            })
            .collect();
        form::set_rows(rows, rows_now, mtm);
    }

    fn server_sheet(
        &self,
        mtm: MainThreadMarker,
    ) -> (Retained<NSWindow>, Retained<NSView>, ServerForm) {
        let text = |placeholder: &str| {
            let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            field
        };
        let name = text("New Server");
        let url = text("https://");
        let user = text("User");
        let timeout = text("60");
        let password = NSSecureTextField::new(mtm);
        password.setPlaceholderString(Some(ns_string!("Saved to the Keychain")));
        let ignore_tls = NSSwitch::new(mtm);
        let target: &AnyObject = self;
        // SAFETY: the pane owns the sheet and its controls, so it outlives their weak target
        // references; `authChanged:` takes the sender.
        let (auth_none, auth_basic) = unsafe {
            (
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("None"),
                    Some(target),
                    Some(sel!(authChanged:)),
                    mtm,
                ),
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("Basic"),
                    Some(target),
                    Some(sel!(authChanged:)),
                    mtm,
                ),
            )
        };
        form::set_width(&timeout, TIMEOUT_WIDTH, NSLayoutPriorityRequired);
        let auth = form::hstack(vec![view(auth_none.clone()), view(auth_basic.clone())], mtm);
        let timeout_row = form::hstack(
            vec![
                view(timeout.clone()),
                view(NSTextField::labelWithString(ns_string!("seconds"), mtm)),
            ],
            mtm,
        );
        let user_row = form::field_row("User", &user, mtm);
        let password_row = form::field_row("Password", &password, mtm);
        let group = form::group(
            vec![
                form::field_row("Name", &name, mtm),
                form::field_row("URL", &url, mtm),
                form::control_row("Ignore certificate errors", &ignore_tls, mtm),
                form::control_row("Authentication", &auth, mtm),
                user_row.clone(),
                password_row.clone(),
                form::control_row("Timeout", &timeout_row, mtm),
            ],
            mtm,
        );
        let title = form::header("", mtm);

        let delete = target_button("Delete Server", self, sel!(deleteServer:), mtm);
        let cancel = target_button("Cancel", self, sel!(cancelServer:), mtm);
        cancel.setKeyEquivalent(ns_string!("\u{1b}"));
        let save = target_button("Save", self, sel!(saveServer:), mtm);
        save.setKeyEquivalent(ns_string!("\r"));
        let body = form::body(
            vec![
                form::Section::new(mtm)
                    .header(&title)
                    .group(&group)
                    .text("A password is saved to the Keychain, not in the project folder.")
                    .build(),
            ],
            form::dialog_buttons(
                Some(view(delete.clone())),
                vec![view(cancel), view(save)],
                mtm,
            ),
            SHEET_WIDTH,
            mtm,
        );

        let sheet = layout::owned_window(
            ns_string!("Server"),
            NSSize::new(SHEET_WIDTH, 400.0),
            NSWindowStyleMask::Titled,
            mtm,
        );
        if let Some(content) = sheet.contentView() {
            form::fill(&content, &body);
        }
        let form = ServerForm {
            title,
            name,
            url,
            ignore_tls,
            auth_none,
            auth_basic,
            user,
            password,
            timeout,
            user_row,
            password_row,
            delete,
        };
        (sheet, body, form)
    }
}

impl ModelAccess for ServersPane {}

/// The row a button stands for, from its tag.
fn tag(sender: Option<&AnyObject>) -> usize {
    let tag = sender
        .and_then(|s| s.downcast_ref::<NSButton>())
        .map_or(0, |b| b.tag());
    usize::try_from(tag).unwrap_or(0)
}
