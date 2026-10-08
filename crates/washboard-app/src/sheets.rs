//! The two sheets of the shell (PLAN §8): New Project with its import check, and Project
//! Settings › Servers. Sample data until WP-APP-INTEGRATION runs the real import check and
//! stores servers through `washboard-ui-model`.

use std::cell::{Cell, OnceCell, RefCell};
use std::fmt;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValue, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSGridView, NSLayoutAttribute,
    NSSecureTextField, NSSplitView, NSStackView, NSTabView, NSTabViewItem, NSTextField,
    NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, ns_string,
};

use crate::table::TextTable;

/// One `wsdl:import`/`xs:import`/`xs:include` found by the import check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// As written, e.g. `xs:import urn:example:common`.
    pub reference: String,
    /// The supplied file it resolved to, or `None` if no supplied file matches.
    pub resolved_to: Option<String>,
}

/// The New Project sheet in PLAN §8: two resolved references and one missing file.
pub fn sample_references() -> Vec<Reference> {
    let r = |reference: &str, to: Option<&str>| Reference {
        reference: reference.into(),
        resolved_to: to.map(Into::into),
    };
    vec![
        r("xs:import urn:example:common", Some("common/types.xsd")),
        r("xs:import urn:example:faults", Some("faults.xsd")),
        r("xs:include addresses.xsd", None),
    ]
}

type OnCreate = Box<dyn Fn(&str)>;

pub struct NewProjectIvars {
    references: RefCell<Vec<Reference>>,
    on_create: RefCell<Option<OnCreate>>,
    window: OnceCell<Retained<NSWindow>>,
    name: OnceCell<Retained<NSTextField>>,
    table: OnceCell<Retained<TextTable>>,
    create: OnceCell<Retained<NSButton>>,
}

impl fmt::Debug for NewProjectIvars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewProjectIvars")
            .field("references", &self.references)
            .finish_non_exhaustive()
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `NewProjectSheet` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NewProjectIvars]
    #[derive(Debug)]
    pub struct NewProjectSheet;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for NewProjectSheet {}

    impl NewProjectSheet {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(cancel:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) {
            end_sheet(self.window());
        }

        #[unsafe(method(create:))]
        fn create_action(&self, _sender: Option<&AnyObject>) {
            self.create();
        }

        #[unsafe(method(chooseFile:))]
        fn choose_file(&self, _sender: Option<&AnyObject>) {
            eprintln!("washboard-app: choosing files is not implemented yet");
        }
    }
);

impl NewProjectSheet {
    pub fn new(references: Vec<Reference>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NewProjectIvars {
            references: RefCell::new(Vec::new()),
            on_create: RefCell::new(None),
            window: OnceCell::new(),
            name: OnceCell::new(),
            table: OnceCell::new(),
            create: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let name = NSTextField::textFieldWithString(ns_string!("Customer API"), mtm);
        let choose = |title: &str| this.button(title, sel!(chooseFile:));
        let form = grid(
            vec![
                vec![label("Name:", mtm), view(name.clone())],
                vec![
                    label("Location:", mtm),
                    row(vec![label("~/Projects/soap", mtm), choose("Choose…")], mtm),
                ],
                vec![
                    label("WSDL:", mtm),
                    row(
                        vec![label("CustomerService.wsdl", mtm), choose("Choose…")],
                        mtm,
                    ),
                ],
                vec![
                    label("XSD files:", mtm),
                    row(
                        vec![
                            label("common/types.xsd, faults.xsd", mtm),
                            choose("Add…"),
                            choose("−"),
                        ],
                        mtm,
                    ),
                ],
            ],
            mtm,
        );

        let table = TextTable::new(&["", "Reference", "Resolved to"], mtm);
        let cancel = target_button("Cancel", &this, sel!(cancel:), mtm);
        cancel.setKeyEquivalent(ns_string!("\u{1b}"));
        let create = target_button("Create", &this, sel!(create:), mtm);
        create.setKeyEquivalent(ns_string!("\r"));
        let buttons = row(vec![view(cancel), view(create.clone())], mtm);

        let content = column(
            vec![
                form,
                label("References", mtm),
                view(table.view().retain()),
                buttons,
            ],
            mtm,
        );
        let window = sheet_window("New Project", NSSize::new(560.0, 420.0), mtm);
        window.setContentView(Some(&content));

        let _ = this.ivars().window.set(window);
        let _ = this.ivars().name.set(name);
        let _ = this.ivars().table.set(table);
        let _ = this.ivars().create.set(create);
        this.set_references(references);
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    pub fn name_field(&self) -> &NSTextField {
        self.ivars().name.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        self.ivars().table.get().expect("set in new()")
    }

    pub fn create_button(&self) -> &NSButton {
        self.ivars().create.get().expect("set in new()")
    }

    /// Called with the project name when the user creates the project.
    pub fn on_create(&self, f: impl Fn(&str) + 'static) {
        *self.ivars().on_create.borrow_mut() = Some(Box::new(f));
    }

    /// Shows the import check's result. Create stays disabled while any reference is
    /// unresolved (PLAN §4 "Create project").
    pub fn set_references(&self, references: Vec<Reference>) {
        let rows = references
            .iter()
            .map(|r| match &r.resolved_to {
                Some(to) => vec!["✓".into(), r.reference.clone(), to.clone()],
                None => vec!["✗".into(), r.reference.clone(), "not supplied".into()],
            })
            .collect();
        self.table().set_rows(rows);
        let resolved = references.iter().all(|r| r.resolved_to.is_some());
        self.create_button().setEnabled(resolved);
        *self.ivars().references.borrow_mut() = references;
    }

    /// Shows the sheet on `parent`.
    pub fn present(&self, parent: &NSWindow) {
        parent.beginSheet_completionHandler(self.window(), None);
    }

    /// What the Create button does; also called by tests. Does nothing while disabled.
    pub fn create(&self) {
        if !self.create_button().isEnabled() {
            return;
        }
        end_sheet(self.window());
        let name = self.name_field().stringValue().to_string();
        if let Some(f) = &*self.ivars().on_create.borrow() {
            f(&name);
        }
    }

    fn button(&self, title: &str, action: Sel) -> Retained<NSView> {
        view(target_button(title, self, action, self.mtm()))
    }
}

/// One server as the settings sheet edits it. The password never appears here: it lives in
/// the Keychain (PLAN §6), and the shell has none to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub url: String,
    pub ignore_tls_errors: bool,
    pub basic_auth: bool,
    pub user: String,
    pub timeout_secs: u32,
}

pub fn sample_servers() -> Vec<Server> {
    let s = |name: &str, url: &str, ignore_tls_errors: bool| Server {
        name: name.into(),
        url: url.into(),
        ignore_tls_errors,
        basic_auth: false,
        user: String::new(),
        timeout_secs: 60,
    };
    vec![
        s("Production", "https://api.example.com/ws/customer", false),
        s("Staging", "https://stg.example.com/ws/customer", true),
        s("Local", "http://localhost:8080/ws/customer", false),
    ]
}

/// The form controls of the Servers tab.
#[derive(Debug)]
struct ServerForm {
    name: Retained<NSTextField>,
    url: Retained<NSTextField>,
    ignore_tls: Retained<NSButton>,
    auth_none: Retained<NSButton>,
    auth_basic: Retained<NSButton>,
    user: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    timeout: Retained<NSTextField>,
}

#[derive(Debug)]
pub struct SettingsIvars {
    servers: RefCell<Vec<Server>>,
    selected: Cell<Option<usize>>,
    window: OnceCell<Retained<NSWindow>>,
    tabs: OnceCell<Retained<NSTabView>>,
    table: OnceCell<Retained<TextTable>>,
    form: OnceCell<ServerForm>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SettingsSheet` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsIvars]
    #[derive(Debug)]
    pub struct SettingsSheet;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SettingsSheet {}

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for SettingsSheet {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for SettingsSheet {
        // SAFETY: the signature matches `controlTextDidChange:`.
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.commit_form();
        }
    }

    impl SettingsSheet {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(formChanged:))]
        fn form_changed(&self, _sender: Option<&AnyObject>) {
            self.commit_form();
        }

        #[unsafe(method(addServer:))]
        fn add_server_action(&self, _sender: Option<&AnyObject>) {
            self.add_server();
        }

        #[unsafe(method(removeServer:))]
        fn remove_server_action(&self, _sender: Option<&AnyObject>) {
            self.remove_selected();
        }

        #[unsafe(method(done:))]
        fn done(&self, _sender: Option<&AnyObject>) {
            end_sheet(self.window());
        }
    }
);

impl SettingsSheet {
    pub fn new(project: &str, servers: Vec<Server>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SettingsIvars {
            servers: RefCell::new(servers),
            selected: Cell::new(None),
            window: OnceCell::new(),
            tabs: OnceCell::new(),
            table: OnceCell::new(),
            form: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = TextTable::new(&[], mtm);
        let weak = objc2::rc::Weak::from(&*this);
        table.on_click(move |row| {
            if let Some(sheet) = weak.load() {
                sheet.select(row);
            }
        });
        let form = this.server_form(mtm);
        let fields = grid(
            vec![
                vec![label("Name:", mtm), view(form.name.clone())],
                vec![label("URL:", mtm), view(form.url.clone())],
                vec![label("TLS:", mtm), view(form.ignore_tls.clone())],
                vec![
                    label("Auth:", mtm),
                    row(
                        vec![view(form.auth_none.clone()), view(form.auth_basic.clone())],
                        mtm,
                    ),
                ],
                vec![label("User:", mtm), view(form.user.clone())],
                vec![label("Password:", mtm), view(form.password.clone())],
                vec![
                    label("Timeout:", mtm),
                    row(vec![view(form.timeout.clone()), label("s", mtm)], mtm),
                ],
            ],
            mtm,
        );
        let list = column(
            vec![
                view(table.view().retain()),
                row(
                    vec![
                        view(target_button("+", &this, sel!(addServer:), mtm)),
                        view(target_button("−", &this, sel!(removeServer:), mtm)),
                    ],
                    mtm,
                ),
            ],
            mtm,
        );
        let servers = NSSplitView::new(mtm);
        servers.setVertical(true);
        servers.addSubview(&list);
        servers.addSubview(&fields);

        let general = label(&format!("Name: {project}"), mtm);
        let tabs = NSTabView::new(mtm);
        for (title, content) in [("General", general), ("Servers", view(servers))] {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(title));
            item.setView(Some(&content));
            tabs.addTabViewItem(&item);
        }
        tabs.selectTabViewItemAtIndex(1);

        let done = target_button("Done", &this, sel!(done:), mtm);
        done.setKeyEquivalent(ns_string!("\r"));
        let content = column(vec![view(tabs.clone()), view(done)], mtm);
        let window = sheet_window(
            &format!("{project} — Settings"),
            NSSize::new(640.0, 400.0),
            mtm,
        );
        window.setContentView(Some(&content));

        let _ = this.ivars().window.set(window);
        let _ = this.ivars().tabs.set(tabs);
        let _ = this.ivars().table.set(table);
        let _ = this.ivars().form.set(form);
        this.reload_table();
        if !this.ivars().servers.borrow().is_empty() {
            this.select(0);
        }
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    pub fn tabs(&self) -> &NSTabView {
        self.ivars().tabs.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        self.ivars().table.get().expect("set in new()")
    }

    pub fn servers(&self) -> Vec<Server> {
        self.ivars().servers.borrow().clone()
    }

    pub fn selected(&self) -> Option<usize> {
        self.ivars().selected.get()
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

    pub fn basic_auth_button(&self) -> &NSButton {
        &self.form().auth_basic
    }

    pub fn present(&self, parent: &NSWindow) {
        parent.beginSheet_completionHandler(self.window(), None);
    }

    /// Loads server `row` into the form.
    pub fn select(&self, row: usize) {
        let Some(server) = self.ivars().servers.borrow().get(row).cloned() else {
            return;
        };
        self.ivars().selected.set(Some(row));
        let table = self.table().table();
        table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        let form = self.form();
        form.name.setStringValue(&NSString::from_str(&server.name));
        form.url.setStringValue(&NSString::from_str(&server.url));
        form.ignore_tls.setState(state(server.ignore_tls_errors));
        form.auth_none.setState(state(!server.basic_auth));
        form.auth_basic.setState(state(server.basic_auth));
        form.user.setStringValue(&NSString::from_str(&server.user));
        form.timeout
            .setStringValue(&NSString::from_str(&server.timeout_secs.to_string()));
        self.enable_auth_fields(server.basic_auth);
    }

    /// Writes the form back into the selected server. Runs on every keystroke and control
    /// change, so the list always shows what the form says.
    pub fn commit_form(&self) {
        let Some(row) = self.selected() else {
            return;
        };
        let form = self.form();
        let basic_auth = form.auth_basic.state() == NSControlStateValueOn;
        {
            let mut servers = self.ivars().servers.borrow_mut();
            let Some(server) = servers.get_mut(row) else {
                return;
            };
            server.name = form.name.stringValue().to_string();
            server.url = form.url.stringValue().to_string();
            server.ignore_tls_errors = form.ignore_tls.state() == NSControlStateValueOn;
            server.basic_auth = basic_auth;
            server.user = form.user.stringValue().to_string();
            // An unparsable timeout keeps the last good value; the model will validate it.
            if let Ok(secs) = form.timeout.stringValue().to_string().trim().parse() {
                server.timeout_secs = secs;
            }
        }
        self.enable_auth_fields(basic_auth);
        self.reload_table();
    }

    pub fn add_server(&self) {
        let row = {
            let mut servers = self.ivars().servers.borrow_mut();
            servers.push(Server {
                name: "New Server".into(),
                url: "https://".into(),
                ignore_tls_errors: false,
                basic_auth: false,
                user: String::new(),
                timeout_secs: 60,
            });
            servers.len() - 1
        };
        self.reload_table();
        self.select(row);
    }

    pub fn remove_selected(&self) {
        let Some(row) = self.selected() else {
            return;
        };
        let remaining = {
            let mut servers = self.ivars().servers.borrow_mut();
            if row < servers.len() {
                servers.remove(row);
            }
            servers.len()
        };
        self.ivars().selected.set(None);
        self.reload_table();
        if remaining > 0 {
            self.select(row.min(remaining - 1));
        }
    }

    fn form(&self) -> &ServerForm {
        self.ivars().form.get().expect("set in new()")
    }

    fn reload_table(&self) {
        let rows = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .map(|s| vec![s.name.clone()])
            .collect();
        self.table().set_rows(rows);
        if let Some(row) = self.selected() {
            self.table()
                .table()
                .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        }
    }

    fn enable_auth_fields(&self, basic_auth: bool) {
        let form = self.form();
        form.user.setEnabled(basic_auth);
        form.password.setEnabled(basic_auth);
    }

    fn server_form(&self, mtm: MainThreadMarker) -> ServerForm {
        let text = |placeholder: &str| {
            let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            // SAFETY: this object owns the field through its window, so it outlives the
            // field's weak delegate reference.
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
            field
        };
        // SAFETY (buttons): this object owns them through its window, so it outlives their
        // weak target references.
        let (ignore_tls, auth_none, auth_basic) = unsafe {
            (
                NSButton::checkboxWithTitle_target_action(
                    ns_string!("Ignore certificate errors"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("None"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("Basic"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
            )
        };
        let password = NSSecureTextField::new(mtm);
        password.setPlaceholderString(Some(ns_string!("Stored in the Keychain")));
        ServerForm {
            name: text("Name"),
            url: text("https://"),
            ignore_tls,
            auth_none,
            auth_basic,
            user: text("User"),
            password,
            timeout: text("60"),
        }
    }
}

fn state(on: bool) -> NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

/// Ends `sheet` on whatever window it is attached to.
fn end_sheet(sheet: &NSWindow) {
    if let Some(parent) = sheet.sheetParent() {
        parent.endSheet(sheet);
    }
}

fn sheet_window(title: &str, size: NSSize, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // SAFETY: the designated initializer, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the sheet's controller keeps the `Retained<NSWindow>`, so AppKit must not
    // release it on close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window
}

fn target_button(
    title: &str,
    target: &NSObject,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let target: &AnyObject = target;
    // SAFETY: every caller owns the button through its window, so the target outlives the
    // button's weak target reference.
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    view(NSTextField::labelWithString(&NSString::from_str(text), mtm))
}

fn view<T: Message + AsRef<NSView>>(v: Retained<T>) -> Retained<NSView> {
    let v: &NSView = (*v).as_ref();
    v.retain()
}

fn row(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    view(stack)
}

fn column(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    view(stack)
}

fn grid(rows: Vec<Vec<Retained<NSView>>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let rows: Vec<Retained<NSArray<NSView>>> = rows
        .iter()
        .map(|r| NSArray::from_retained_slice(r))
        .collect();
    view(NSGridView::gridViewWithViews(
        &NSArray::from_retained_slice(&rows),
        mtm,
    ))
}
