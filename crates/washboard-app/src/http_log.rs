//! The HTTP log: one panel for the whole app listing every exchange, with the selected
//! request and response side by side (PLAN §8). `Authorization` values are masked until the
//! user reveals them, so a screen share or screenshot doesn't leak credentials.

use std::cell::{Cell, OnceCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSPanel, NSScrollView, NSSplitView, NSStackView, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};

use crate::table::TextTable;
use crate::text::mask_authorization;

/// One logged exchange; sample data until the log comes from `washboard-ui-model`.
#[derive(Debug, Clone)]
pub struct LoggedExchange {
    pub time: String,
    pub method_url: String,
    pub status: String,
    pub request: String,
    pub response: String,
}

pub fn sample_exchanges() -> Vec<LoggedExchange> {
    vec![LoggedExchange {
        time: "14:03:12".into(),
        method_url: "POST https://staging.example.com/customer".into(),
        status: "200 OK".into(),
        request: "POST /customer HTTP/1.1\nHost: staging.example.com\n\
                  Authorization: Basic YWxpY2U6c2VjcmV0\nContent-Type: text/xml; charset=utf-8\n\
                  SOAPAction: \"urn:example:customer/GetCustomer\"\n\n<soapenv:Envelope …/>\n"
            .into(),
        response: "HTTP/1.1 200 OK\nContent-Type: text/xml; charset=utf-8\n\n<soap:Envelope …/>\n"
            .into(),
    }]
}

#[derive(Debug)]
pub struct LogIvars {
    exchanges: Vec<LoggedExchange>,
    selected: Cell<Option<usize>>,
    revealed: Cell<bool>,
    panel: OnceCell<Retained<NSPanel>>,
    table: OnceCell<Retained<TextTable>>,
    request: OnceCell<Retained<NSScrollView>>,
    response: OnceCell<Retained<NSScrollView>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `HttpLog` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = LogIvars]
    #[derive(Debug)]
    pub struct HttpLog;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for HttpLog {}

    impl HttpLog {
        // SAFETY: action methods take the sender and return nothing.
        #[unsafe(method(toggleAuthorization:))]
        fn toggle_authorization(&self, _sender: Option<&AnyObject>) {
            self.set_revealed(!self.ivars().revealed.get());
        }
    }
);

impl HttpLog {
    pub fn new(exchanges: Vec<LoggedExchange>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LogIvars {
            exchanges,
            selected: Cell::new(None),
            revealed: Cell::new(false),
            panel: OnceCell::new(),
            table: OnceCell::new(),
            request: OnceCell::new(),
            response: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = TextTable::new(&["Time", "Request", "Status"], mtm);
        table.set_rows(
            this.ivars()
                .exchanges
                .iter()
                .map(|e| vec![e.time.clone(), e.method_url.clone(), e.status.clone()])
                .collect(),
        );
        let weak = objc2::rc::Weak::from(&*this);
        table.on_click(move |row| {
            if let Some(log) = weak.load() {
                log.select(row);
            }
        });

        let request = read_only_text(mtm);
        let response = read_only_text(mtm);
        let sides = NSSplitView::new(mtm);
        sides.setVertical(true);
        sides.addSubview(&request);
        sides.addSubview(&response);

        // SAFETY: this object owns the button through the panel, so it outlives the button's
        // weak target reference.
        let reveal = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Show Authorization"),
                Some(&this),
                Some(sel!(toggleAuthorization:)),
                mtm,
            )
        };
        let detail: [Retained<NSView>; 2] = [
            Retained::into_super(Retained::into_super(reveal)),
            Retained::into_super(sides),
        ];
        let detail = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&detail), mtm);
        detail.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);

        let split = NSSplitView::new(mtm);
        split.setVertical(false);
        split.addSubview(table.view());
        split.addSubview(&detail);

        let panel = panel(mtm);
        panel.setContentView(Some(&split));
        let _ = this.ivars().panel.set(panel);
        let _ = this.ivars().table.set(table);
        let _ = this.ivars().request.set(request);
        let _ = this.ivars().response.set(response);
        if !this.ivars().exchanges.is_empty() {
            this.select(0);
        }
        this
    }

    pub fn panel(&self) -> &NSPanel {
        self.ivars().panel.get().expect("set in new()")
    }

    pub fn show(&self) {
        self.panel().makeKeyAndOrderFront(None);
    }

    pub fn table(&self) -> &TextTable {
        self.ivars().table.get().expect("set in new()")
    }

    /// The request side as displayed.
    pub fn request_text(&self) -> String {
        text_of(self.ivars().request.get().expect("set in new()"))
    }

    pub fn set_revealed(&self, revealed: bool) {
        self.ivars().revealed.set(revealed);
        if let Some(row) = self.ivars().selected.get() {
            self.select(row);
        }
    }

    pub fn select(&self, row: usize) {
        let Some(exchange) = self.ivars().exchanges.get(row) else {
            return;
        };
        self.ivars().selected.set(Some(row));
        let request = if self.ivars().revealed.get() {
            exchange.request.clone()
        } else {
            mask_authorization(&exchange.request)
        };
        set_text(self.ivars().request.get().expect("set in new()"), &request);
        set_text(
            self.ivars().response.get().expect("set in new()"),
            &exchange.response,
        );
    }
}

fn panel(mtm: MainThreadMarker) -> Retained<NSPanel> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 560.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::UtilityWindow;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: `HttpLog` keeps the panel, so AppKit must not release it on close as well.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(ns_string!("HTTP Log"));
    panel.setHidesOnDeactivate(false);
    panel.center();
    panel
}

fn read_only_text(mtm: MainThreadMarker) -> Retained<NSScrollView> {
    let scroll = NSTextView::scrollableTextView(mtm);
    if let Some(text) = text_view(&scroll) {
        text.setEditable(false);
    }
    scroll
}

fn text_view(scroll: &NSScrollView) -> Option<Retained<NSTextView>> {
    scroll.documentView()?.downcast::<NSTextView>().ok()
}

fn set_text(scroll: &NSScrollView, text: &str) {
    if let Some(view) = text_view(scroll) {
        view.setString(&NSString::from_str(text));
    }
}

fn text_of(scroll: &NSScrollView) -> String {
    text_view(scroll)
        .map(|v| v.string().to_string())
        .unwrap_or_default()
}
