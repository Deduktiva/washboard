//! View and window helpers shared by the windows.
//!
//! NSStackView's default distribution (gravity areas) gives a view without an intrinsic size,
//! such as a scroll view, no height at all, so its neighbours draw over it. Columns that hold
//! scroll views use `Fill`, where the views that hug least (scroll views) take the slack.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont,
    NSLayoutAttribute, NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh,
    NSLayoutPriorityDefaultLow, NSLineBreakMode, NSScrollView, NSStackView,
    NSStackViewDistribution, NSTableCellView, NSTextField, NSTitlePosition,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};

/// `v` as a plain `NSView`, for stacks and grids, whatever its class's depth below `NSView`.
pub fn view<T: Message + AsRef<NSView>>(v: Retained<T>) -> Retained<NSView> {
    let v: &NSView = (*v).as_ref();
    v.retain()
}

/// A vertical stack whose views span its width and fill its height.
pub fn fill_column(views: &[Retained<NSView>], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Width);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack.setSpacing(0.0);
    // A stack hugs its content across its orientation at a high priority by default, which
    // beat a tab view's sizing of its page: the History page stayed at its fitting width.
    stack.setHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Horizontal,
    );
    stack
}

/// A horizontal row of controls, vertically centred. It keeps its controls' height in a
/// [`fill_column`], leaving the slack to the scroll views.
pub fn row(views: &[Retained<NSView>], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setAlignment(NSLayoutAttribute::CenterY);
    hug_vertically(&stack);
    stack
}

/// Keeps `stack` at its content's height in a [`fill_column`].
pub fn hug_vertically(stack: &NSStackView) {
    stack.setHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultHigh,
        NSLayoutConstraintOrientation::Vertical,
    );
}

pub fn insets(top: f64, left: f64, bottom: f64, right: f64) -> NSEdgeInsets {
    NSEdgeInsets {
        top,
        left,
        bottom,
        right,
    }
}

/// Pins `view`'s height, for a list that should not compete with the editor for space.
pub fn set_height(view: &NSView, height: f64) {
    view.heightAnchor()
        .constraintEqualToConstant(height)
        .setActive(true);
}

/// A tab view item's view: a plain container that the tab view resizes, with `content` pinned
/// to its edges. A stack view handed to the tab view directly kept its fitting size when its
/// tab was selected after the window's first layout.
pub fn tab_page(content: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let page = NSView::new(mtm);
    page.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    page.addSubview(content);
    for (a, b) in [
        (content.leadingAnchor(), page.leadingAnchor()),
        (content.trailingAnchor(), page.trailingAnchor()),
    ] {
        a.constraintEqualToAnchor(&b).setActive(true);
    }
    for (a, b) in [
        (content.topAnchor(), page.topAnchor()),
        (content.bottomAnchor(), page.bottomAnchor()),
    ] {
        a.constraintEqualToAnchor(&b).setActive(true);
    }
    page
}

/// A table or outline cell around `content`: centred vertically and spanning the column, as
/// AppKit's own cells are. Returning a bare label instead leaves its text at the top of a
/// taller row and lets a long value run past the column. `label` becomes the cell's text
/// field, which AppKit recolours on selection and edits for inline rename.
pub fn cell(
    content: &NSView,
    label: Option<&NSTextField>,
    mtm: MainThreadMarker,
) -> Retained<NSTableCellView> {
    let cell = NSTableCellView::new(mtm);
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    cell.addSubview(content);
    for constraint in [
        content
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&cell.leadingAnchor(), CELL_INSET),
        content
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&cell.trailingAnchor(), -CELL_INSET),
        content
            .centerYAnchor()
            .constraintEqualToAnchor(&cell.centerYAnchor()),
    ] {
        constraint.setActive(true);
    }
    // SAFETY: the label is `content` or inside it, so the cell keeps it alive for as long as
    // its unretained `textField` reference is used.
    unsafe { cell.setTextField(label) };
    cell
}

/// From a table cell's edges to its content.
pub const CELL_INSET: f64 = 2.0;

/// Lets a label give up width before its neighbours do, shortening its text with an ellipsis
/// (`mode`) and showing the whole of it in a tool tip on hover.
pub fn truncating(label: &NSTextField, mode: NSLineBreakMode) {
    label.setLineBreakMode(mode);
    label.setAllowsExpansionToolTips(true);
    label.setContentCompressionResistancePriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Horizontal,
    );
}

/// A label in the small (11 pt) system font, for summaries, markers and secondary lines.
pub fn small_label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(SMALL_FONT_SIZE)));
    label
}

const SMALL_FONT_SIZE: f64 = 11.0;

/// Status text beside other controls (the request bar's well-formedness, the HTTP log's TLS
/// line): a step above [`small_label`], still below the body size.
pub const STATUS_FONT_SIZE: f64 = 12.0;

/// A chip, as `docs/gui-draft.html` draws them: small secondary text in a rounded outline. For
/// facts beside a name (a port's SOAP version, a request's operation). Returns the chip and
/// its label, whose text the caller may change; the chip follows the label's size.
pub fn chip(text: &str, mtm: MainThreadMarker) -> (Retained<NSBox>, Retained<NSTextField>) {
    let label = small_label(text, mtm);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let chip = NSBox::new(mtm);
    chip.setBoxType(NSBoxType::Custom);
    chip.setTitlePosition(NSTitlePosition::NoTitle);
    // Semantic colours, so the outline follows the appearance (a CALayer border would not).
    chip.setBorderColor(&NSColor::separatorColor());
    chip.setBorderWidth(1.0);
    chip.setCornerRadius(4.0);
    chip.setContentViewMargins(NSSize::new(0.0, 0.0));
    if let Some(content) = chip.contentView() {
        label.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&label);
        for constraint in [
            label
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), CHIP_PADDING),
            content
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&label.trailingAnchor(), CHIP_PADDING),
            label
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), 1.0),
            content
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&label.bottomAnchor(), 1.0),
        ] {
            constraint.setActive(true);
        }
    }
    // The box has no size of its own; its label's decides. A chip is short and stays whole:
    // a truncating name beside it gives up width first, and it does not stretch.
    label.setContentHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultHigh,
        NSLayoutConstraintOrientation::Horizontal,
    );
    (chip, label)
}

const CHIP_PADDING: f64 = 5.0;

/// A scroll view around `document` that scrolls vertically.
pub fn vertical_scroll(document: &NSView, mtm: MainThreadMarker) -> Retained<NSScrollView> {
    let scroll = NSScrollView::new(mtm);
    scroll.setDocumentView(Some(document));
    scroll.setHasVerticalScroller(true);
    // Shown only when the content does not fit, also with legacy (always-on) scrollers.
    scroll.setAutohidesScrollers(true);
    scroll
}

/// A titled window of content size `size`, for a controller that keeps it in a `Retained`:
/// closing it only hides it, and the controller decides when it goes.
pub fn owned_window(
    title: &NSString,
    size: NSSize,
    style: NSWindowStyleMask,
    mtm: MainThreadMarker,
) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // SAFETY: the designated initializer, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the caller keeps the `Retained<NSWindow>`, so AppKit must not release it on
    // close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(title);
    window
}
