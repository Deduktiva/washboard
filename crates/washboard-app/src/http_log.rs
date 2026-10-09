//! The HTTP log: one panel for the whole app listing the model's exchanges, with the selected
//! request and response side by side (PLAN §8). `Authorization` values are masked until the
//! user reveals them, so a screen share or screenshot doesn't leak credentials. Above the two
//! sides, a line says how TLS went, loudly when certificate verification was skipped.

use std::cell::{Cell, OnceCell, RefCell};
use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSPanel, NSScrollView, NSSplitView, NSStackView,
    NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSDate, NSDateFormatter, NSDateFormatterStyle, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, ns_string,
};
use washboard_core::xml;
use washboard_ui_model::LogEntry;

use crate::app::with_delegate;
use crate::layout;
use crate::table::TextTable;
use crate::text::tls_line;

/// What the log lists for one exchange, copied out of the model so the model is not borrowed
/// while AppKit draws.
#[derive(Debug, Clone)]
struct Row {
    time: String,
    request_line: String,
    status: String,
    entry: LogEntry,
}

#[derive(Debug)]
pub struct LogIvars {
    rows: RefCell<Vec<Row>>,
    selected: Cell<Option<usize>>,
    revealed: Cell<bool>,
    panel: OnceCell<Retained<NSPanel>>,
    table: OnceCell<Retained<TextTable>>,
    request: OnceCell<Retained<NSScrollView>>,
    response: OnceCell<Retained<NSScrollView>>,
    tls: OnceCell<Retained<NSTextField>>,
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
    /// An empty log; [`reload`](Self::reload) fills it from the model.
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LogIvars {
            rows: RefCell::new(Vec::new()),
            selected: Cell::new(None),
            revealed: Cell::new(false),
            panel: OnceCell::new(),
            table: OnceCell::new(),
            request: OnceCell::new(),
            response: OnceCell::new(),
            tls: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = TextTable::new(&["Time", "Request", "Status"], mtm);
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
        let tls = NSTextField::labelWithString(ns_string!(""), mtm);
        let header = layout::row(
            &[
                Retained::into_super(Retained::into_super(reveal)),
                Retained::into_super(Retained::into_super(tls.clone())),
            ],
            mtm,
        );
        header.setSpacing(12.0);
        let detail: [Retained<NSView>; 2] =
            [Retained::into_super(header), Retained::into_super(sides)];
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
        let _ = this.ivars().tls.set(tls);
        this
    }

    /// Lists the model's log, oldest first (`LogAppended`), keeping the selected exchange
    /// selected while it is still in the log.
    pub fn reload(&self) {
        let entries = with_delegate(self.mtm(), |d| {
            d.read(|app| app.http_log().iter().cloned().collect::<Vec<_>>())
        })
        .flatten()
        .unwrap_or_default();
        let selected = self.ivars().selected.get().and_then(|row| {
            self.ivars()
                .rows
                .borrow()
                .get(row)
                .map(|r| r.entry.exchange.started_at)
        });
        let formatter = NSDateFormatter::new();
        formatter.setDateStyle(NSDateFormatterStyle::NoStyle);
        formatter.setTimeStyle(NSDateFormatterStyle::MediumStyle);
        let rows: Vec<Row> = entries
            .into_iter()
            .map(|entry| {
                let exchange = &entry.exchange;
                let since_epoch = exchange
                    .started_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                let date = NSDate::dateWithTimeIntervalSince1970(since_epoch.as_secs_f64());
                let status = match (&exchange.response, &exchange.error) {
                    (Some(response), _) => response.status_text().to_owned(),
                    (None, Some(error)) => format!("Failed: {error}"),
                    (None, None) => "—".to_owned(),
                };
                Row {
                    time: formatter.stringFromDate(&date).to_string(),
                    request_line: format!("{}: {}", entry.project, exchange.request.start_line),
                    status,
                    entry,
                }
            })
            .collect();
        self.table().set_rows(
            rows.iter()
                .map(|r| vec![r.time.clone(), r.request_line.clone(), r.status.clone()])
                .collect(),
        );
        let row = selected
            .and_then(|at| rows.iter().position(|r| r.entry.exchange.started_at == at))
            .or_else(|| rows.len().checked_sub(1));
        *self.ivars().rows.borrow_mut() = rows;
        match row {
            Some(row) => self.select(row),
            None => {
                self.ivars().selected.set(None);
                set_text(self.ivars().request.get().expect("set in new()"), "");
                set_text(self.ivars().response.get().expect("set in new()"), "");
                self.tls_label().setStringValue(ns_string!(""));
            }
        }
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

    /// The selected exchange's TLS line, as displayed.
    pub fn tls_text(&self) -> String {
        self.tls_label().stringValue().to_string()
    }

    fn tls_label(&self) -> &NSTextField {
        self.ivars().tls.get().expect("set in new()")
    }

    pub fn set_revealed(&self, revealed: bool) {
        self.ivars().revealed.set(revealed);
        if let Some(row) = self.ivars().selected.get() {
            self.select(row);
        }
    }

    /// Shows exchange `row`: the request with `Authorization` masked unless revealed, and
    /// the response (or why there is none).
    pub fn select(&self, row: usize) {
        let Some(entry) = self.ivars().rows.borrow().get(row).map(|r| r.entry.clone()) else {
            return;
        };
        self.ivars().selected.set(Some(row));
        let exchange = &entry.exchange;
        let request = message_text(
            &exchange.request.start_line,
            &entry.request_headers(self.ivars().revealed.get()),
            &exchange.request.body,
        );
        let response = match (&exchange.response, &exchange.error) {
            (Some(r), _) => message_text(&r.start_line, &r.headers, &r.body),
            (None, Some(error)) => format!("No response: {error}\n"),
            (None, None) => String::new(),
        };
        set_text(self.ivars().request.get().expect("set in new()"), &request);
        set_text(
            self.ivars().response.get().expect("set in new()"),
            &response,
        );
        let tls = self.tls_label();
        let line = tls_line(&exchange.tls, exchange.response.is_some()).unwrap_or_default();
        tls.setStringValue(&NSString::from_str(&line));
        // A skipped check is a warning, as in the draft; anything else is a plain fact.
        let (colour, font) = if exchange.tls.verification_skipped {
            (
                NSColor::systemOrangeColor(),
                NSFont::boldSystemFontOfSize(12.0),
            )
        } else {
            (
                NSColor::secondaryLabelColor(),
                NSFont::systemFontOfSize(12.0),
            )
        };
        tls.setTextColor(Some(&colour));
        tls.setFont(Some(&font));
    }
}

/// A message as it went over the wire, with the body as text.
fn message_text(start_line: &str, headers: &[(String, String)], body: &[u8]) -> String {
    let mut text = format!("{start_line}\n");
    for (name, value) in headers {
        let _ = writeln!(text, "{name}: {value}");
    }
    text.push('\n');
    text.push_str(&xml::decode_lossy(body));
    text
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
    scroll.setAutohidesScrollers(true);
    if let Some(text) = text_view(&scroll) {
        text.setEditable(false);
        crate::editor::code_text(&text);
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
