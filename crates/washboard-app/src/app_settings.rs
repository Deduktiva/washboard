//! The app's own settings (WP-FORMAT-XML): the indent width and format on save, kept in the
//! user defaults where macOS keeps app settings, and the small window Washboard ▸ Settings…
//! shows them in. WP-SETTINGS-WINDOW replaces the window with one for the app and every open
//! project; the defaults keys stay.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSGridCellPlacement, NSGridRowAlignment, NSGridView, NSLayoutConstraintOrientation,
    NSLayoutPriorityDefaultHigh, NSPopUpButton, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSUserDefaults,
    ns_string,
};
use washboard_ui_model::{FormatSettings, INDENT_RANGE};

use crate::app::with_delegate;
use crate::layout;

/// Spaces per level, 1–8.
pub const INDENT_KEY: &str = "FormatIndent";
/// Format the open request on Save All.
pub const ON_SAVE_KEY: &str = "FormatOnSave";

/// The settings in `defaults`; a missing or out-of-range value reads as its default.
pub fn load(defaults: &NSUserDefaults) -> FormatSettings {
    let fallback = FormatSettings::default();
    let indent_key = NSString::from_str(INDENT_KEY);
    let indent = match defaults.objectForKey(&indent_key) {
        Some(_) => usize::try_from(defaults.integerForKey(&indent_key))
            .ok()
            .filter(|i| INDENT_RANGE.contains(i))
            .unwrap_or(fallback.indent),
        None => fallback.indent,
    };
    let on_save_key = NSString::from_str(ON_SAVE_KEY);
    let on_save = match defaults.objectForKey(&on_save_key) {
        Some(_) => defaults.boolForKey(&on_save_key),
        None => fallback.on_save,
    };
    FormatSettings { indent, on_save }
}

pub fn store(defaults: &NSUserDefaults, settings: FormatSettings) {
    let indent = isize::try_from(settings.indent).unwrap_or(2);
    defaults.setInteger_forKey(indent, &NSString::from_str(INDENT_KEY));
    defaults.setBool_forKey(settings.on_save, &NSString::from_str(ON_SAVE_KEY));
}

#[derive(Debug, Default)]
pub struct AppSettingsIvars {
    window: OnceCell<Retained<NSWindow>>,
    indent: OnceCell<Retained<NSPopUpButton>>,
    on_save: OnceCell<Retained<NSButton>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `AppSettings` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppSettingsIvars]
    #[derive(Debug)]
    pub struct AppSettings;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for AppSettings {}

    impl AppSettings {
        // SAFETY: an action method: takes the sender, returns nothing.
        #[unsafe(method(settingChanged:))]
        fn setting_changed(&self, _sender: Option<&AnyObject>) {
            let settings = self.shown();
            with_delegate(self.mtm(), |d| d.set_format_settings(settings));
        }
    }
);

impl AppSettings {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppSettingsIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let indent =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for width in INDENT_RANGE {
            let title = if width == 1 {
                "1 space".to_owned()
            } else {
                format!("{width} spaces")
            };
            indent.addItemWithTitle(&NSString::from_str(&title));
        }
        let target: &AnyObject = &this;
        // SAFETY: this object owns the controls through its window, so it outlives their weak
        // target references; `settingChanged:` takes the sender.
        let on_save = unsafe {
            indent.setTarget(Some(target));
            indent.setAction(Some(sel!(settingChanged:)));
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Format the open request on Save All (⌘S)"),
                Some(target),
                Some(sel!(settingChanged:)),
                mtm,
            )
        };
        let hint = NSTextField::wrappingLabelWithString(
            ns_string!(
                "Also used for new requests and responses. Autosave and Send never \
                 reformat a request."
            ),
            mtm,
        );
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        hint.setPreferredMaxLayoutWidth(300.0);

        let label = NSTextField::labelWithString(ns_string!("Indent:"), mtm);
        let rows: Vec<Retained<NSArray<NSView>>> = vec![
            NSArray::from_retained_slice(&[into_view(label), into_view(indent.clone())]),
            NSArray::from_retained_slice(&[NSView::new(mtm), into_view(on_save.clone())]),
            NSArray::from_retained_slice(&[NSView::new(mtm), into_view(hint)]),
        ];
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
        grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
        grid.setColumnSpacing(8.0);
        grid.setRowSpacing(10.0);
        grid.columnAtIndex(0)
            .setXPlacement(NSGridCellPlacement::Trailing);
        if let Some(label) = rows.first().and_then(|r| r.firstObject()) {
            label.setContentHuggingPriority_forOrientation(
                NSLayoutPriorityDefaultHigh,
                NSLayoutConstraintOrientation::Horizontal,
            );
        }

        let stack =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[into_view(grid)]), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setEdgeInsets(layout::insets(20.0, 20.0, 20.0, 20.0));

        let window = settings_window(mtm);
        window.setContentView(Some(&stack));
        let _ = this.ivars().window.set(window);
        let _ = this.ivars().indent.set(indent);
        let _ = this.ivars().on_save.set(on_save);
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    /// Shows the window with `settings`, the app's current ones.
    pub fn show(&self, settings: FormatSettings) {
        self.set_shown(settings);
        if !self.window().isVisible() {
            self.window().center();
        }
        self.window().makeKeyAndOrderFront(None);
    }

    pub fn indent_popup(&self) -> &NSPopUpButton {
        self.ivars().indent.get().expect("set in new()")
    }

    pub fn on_save_checkbox(&self) -> &NSButton {
        self.ivars().on_save.get().expect("set in new()")
    }

    /// What the controls show.
    pub fn shown(&self) -> FormatSettings {
        let index = usize::try_from(self.indent_popup().indexOfSelectedItem()).unwrap_or(1);
        FormatSettings {
            indent: INDENT_RANGE.start() + index,
            on_save: self.on_save_checkbox().state() == NSControlStateValueOn,
        }
    }

    fn set_shown(&self, settings: FormatSettings) {
        let index = settings.indent.saturating_sub(*INDENT_RANGE.start());
        self.indent_popup()
            .selectItemAtIndex(isize::try_from(index).unwrap_or(1));
        let state = if settings.on_save {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        };
        self.on_save_checkbox().setState(state);
    }
}

fn into_view<T: Message + AsRef<NSView>>(v: Retained<T>) -> Retained<NSView> {
    let v: &NSView = (*v).as_ref();
    v.retain()
}

fn settings_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(380.0, 140.0));
    // SAFETY: the designated initializer, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: `AppSettings` keeps the `Retained<NSWindow>`, so AppKit must not release it on
    // close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Settings"));
    window
}
