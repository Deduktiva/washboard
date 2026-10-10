//! Grouped forms, laid out as System Settings lays them out. Every Settings pane is built from
//! these helpers, so margins and alignment are decided once, here:
//!
//! - A pane is a column of sections, [`PAGE_MARGIN`] from the content area's edges, at most
//!   [`MAX_WIDTH`] wide and centred beyond that.
//! - A section is an optional [`header`], then groups, texts and footnotes, in that order.
//! - Every text in a pane starts on one leading line: headers, footnotes and the rows' labels
//!   are all [`ROW_INSET`] inside the group's edge.
//! - In a row, the label is on the leading edge and the control on the trailing edge. Text
//!   fields share [`FIELD_WIDTH`], so their leading edges line up down a group. Several
//!   controls in one row go in an [`hstack`].
//! - When there is not enough width, the label truncates, never the control; a read-only
//!   value ([`value_row`]) truncates before its label.
//!
//! Positions are explicit constraints, not NSStackView alignment. With vertical stacks set to
//! `Width` alignment and edge insets, the Settings panes' sections, headers and rows drifted to
//! either edge as if nothing fixed their horizontal position; and a stack that detaches a
//! hidden view drops the constraints added to it. Hiding a block goes through [`set_shown`] instead, which also takes its spacing away.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSLayoutConstraintOrientation, NSLayoutPriority,
    NSLayoutPriorityDefaultHigh, NSLayoutPriorityDefaultLow, NSLayoutPriorityRequired,
    NSLineBreakMode, NSResponder, NSStackView, NSTextField, NSTitlePosition,
    NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSObject, NSSize, NSString};

use crate::layout::{self, view};

/// From the content area's edges to a group's edges.
pub const PAGE_MARGIN: f64 = 20.0;
/// Groups grow with the window up to this width, then stay centred: a wide window should not
/// put a label and its switch half a screen apart.
pub const MAX_WIDTH: f64 = 640.0;
/// From a group's edges to its rows' content. Headers and footnotes are inset by the same
/// amount, so every text in a pane starts on one leading line.
pub const ROW_INSET: f64 = 10.0;
/// Above and below a row's tallest control.
const ROW_PADDING: f64 = 7.0;
/// The height of a row holding one ordinary control, so rows line up whatever they hold.
const ROW_MIN_HEIGHT: f64 = 36.0;
/// Between a label and its control, and between controls side by side.
const CONTROL_SPACING: f64 = 8.0;
/// Between a header, a group and a footnote of one section.
const SECTION_GAP: f64 = 6.0;
/// Between sections.
const SECTION_SPACING: f64 = 20.0;
/// Above the first section and below the last.
const PAGE_PADDING: f64 = 20.0;
/// Text fields in rows share one width, so their leading edges line up down a group.
pub const FIELD_WIDTH: f64 = 260.0;
/// The bar of +/− buttons under a list.
const BAR_HEIGHT: f64 = 28.0;

/// View identifiers, so tests can find a pane's parts and check their geometry.
pub const HEADER_ID: &str = "form.header";
pub const GROUP_ID: &str = "form.group";
pub const TEXT_ID: &str = "form.text";
const COLLAPSED_ID: &str = "form.collapsed";

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond its designated initializers, which we
    //   inherit.
    // - `FlippedView` does not implement `Drop`.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    struct FlippedView;

    impl FlippedView {
        // A scroll view's document starts at the top only when it is flipped.
        // SAFETY: the signature matches `isFlipped`.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// A pane: `sections` top to bottom, in a column that scrolls when the window is short.
pub fn page(sections: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let blocks = sections
        .into_iter()
        .enumerate()
        .map(|(i, view)| Block {
            view,
            inset: 0.0,
            gap: if i == 0 { 0.0 } else { SECTION_SPACING },
        })
        .collect();
    let content = column(blocks, PAGE_PADDING, mtm);
    // SAFETY: `init` is NSView's designated initializer for code-built views.
    let document: Retained<FlippedView> = unsafe { msg_send![FlippedView::alloc(mtm), init] };
    document.setTranslatesAutoresizingMaskIntoConstraints(false);
    document.addSubview(&content);
    let width = content
        .widthAnchor()
        .constraintEqualToAnchor_constant(&document.widthAnchor(), -2.0 * PAGE_MARGIN);
    width.setPriority(NSLayoutPriorityDefaultHigh);
    activate([
        content
            .topAnchor()
            .constraintEqualToAnchor(&document.topAnchor()),
        content
            .bottomAnchor()
            .constraintEqualToAnchor(&document.bottomAnchor()),
        content
            .centerXAnchor()
            .constraintEqualToAnchor(&document.centerXAnchor()),
        content
            .leadingAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&document.leadingAnchor(), PAGE_MARGIN),
        content
            .widthAnchor()
            .constraintLessThanOrEqualToConstant(MAX_WIDTH),
        width,
    ]);
    let scroll = layout::vertical_scroll(&document, mtm);
    scroll.setDrawsBackground(false);
    let clip = scroll.contentView();
    activate([
        document
            .topAnchor()
            .constraintEqualToAnchor(&clip.topAnchor()),
        document
            .leadingAnchor()
            .constraintEqualToAnchor(&clip.leadingAnchor()),
        document
            .widthAnchor()
            .constraintEqualToAnchor(&clip.widthAnchor()),
    ]);
    view(scroll)
}

/// One section of a [`page`], built top to bottom.
pub struct Section {
    blocks: Vec<Block>,
    mtm: MainThreadMarker,
}

impl Section {
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self {
            blocks: Vec::new(),
            mtm,
        }
    }

    /// The section's title, made with [`header`].
    pub fn header(self, label: &NSTextField) -> Self {
        self.push(view(label.retain()), ROW_INSET)
    }

    /// A group made with [`group`].
    pub fn group(self, group: &NSBox) -> Self {
        self.push(view(group.retain()), 0.0)
    }

    /// An explanation in secondary text: under a group, what it does; alone, what the pane is.
    pub fn text(self, text: &str) -> Self {
        let label = footnote(text, self.mtm);
        self.push(view(label), ROW_INSET)
    }

    pub fn build(self) -> Retained<NSView> {
        column(self.blocks, 0.0, self.mtm)
    }

    fn push(mut self, view: Retained<NSView>, inset: f64) -> Self {
        let gap = if self.blocks.is_empty() {
            0.0
        } else {
            SECTION_GAP
        };
        self.blocks.push(Block { view, inset, gap });
        self
    }
}

/// A section's title, above its group. The caller may change its text later.
pub fn header(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
    label.setIdentifier(Some(&NSString::from_str(HEADER_ID)));
    label
}

/// A grouped box: `rows` in a rounded inset box on the window background, a hairline between
/// rows. Its rows can be replaced with [`set_rows`].
pub fn group(rows: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSBox> {
    let group = NSBox::new(mtm);
    group.setBoxType(NSBoxType::Custom);
    group.setTitlePosition(NSTitlePosition::NoTitle);
    // Semantic colours, so the group follows the appearance.
    group.setFillColor(&NSColor::quaternarySystemFillColor());
    group.setBorderColor(&NSColor::separatorColor());
    group.setBorderWidth(0.5);
    group.setCornerRadius(10.0);
    group.setContentViewMargins(NSSize::new(0.0, 0.0));
    group.setIdentifier(Some(&NSString::from_str(GROUP_ID)));
    set_rows(&group, rows, mtm);
    group
}

/// Replaces the rows of a [`group`].
pub fn set_rows(group: &NSBox, rows: Vec<Retained<NSView>>, mtm: MainThreadMarker) {
    let Some(content) = group.contentView() else {
        return;
    };
    for old in content.subviews().iter() {
        old.removeFromSuperview();
    }
    let mut blocks = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            blocks.push(Block {
                view: separator(mtm),
                inset: ROW_INSET,
                gap: 0.0,
            });
        }
        blocks.push(Block {
            view: row,
            inset: 0.0,
            gap: 0.0,
        });
    }
    fill(&content, &column(blocks, 0.0, mtm));
}

/// Shows or hides `block`, a section or anything else placed by [`page`] or [`Section`],
/// together with the space above it.
pub fn set_shown(block: &NSView, shown: bool) {
    block.setHidden(!shown);
    // SAFETY: `superview` returns the superview retained, or nil; the block is in a window's
    // view tree on the main thread.
    let Some(slot) = (unsafe { block.superview() }) else {
        return;
    };
    let id = NSString::from_str(COLLAPSED_ID);
    let collapsed = slot
        .constraints()
        .iter()
        .find(|c| c.identifier().is_some_and(|i| *i == *id));
    // A collapsed slot is shorter than the hidden block, which must not take clicks or draw.
    slot.setClipsToBounds(!shown);
    match (collapsed, shown) {
        (Some(c), true) => c.setActive(false),
        (None, false) => {
            let c = slot.heightAnchor().constraintEqualToConstant(0.0);
            c.setIdentifier(Some(&id));
            c.setActive(true);
        }
        _ => {}
    }
}

/// A setting: `label` on the leading edge, `control` on the trailing edge. The label truncates
/// when the row is narrow.
pub fn control_row(label: &str, control: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let label = NSTextField::labelWithString(&NSString::from_str(label), mtm);
    layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
    row(&label, control, mtm)
}

/// A text field with its label; every field in a group is [`FIELD_WIDTH`] wide while the
/// group has room.
pub fn field_row(label: &str, field: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    set_width(field, FIELD_WIDTH, NSLayoutPriorityDefaultHigh);
    control_row(label, field, mtm)
}

/// A read-only `value` with its label. The value truncates first, so the label stays readable.
pub fn value_row(label: &str, value: &NSTextField, mtm: MainThreadMarker) -> Retained<NSView> {
    let label = NSTextField::labelWithString(&NSString::from_str(label), mtm);
    value.setAlignment(objc2_app_kit::NSTextAlignment::Right);
    row(&label, value, mtm)
}

/// `label` with `content` under it, both across the row: for content too tall or too wide to
/// sit beside a label, such as a list of files.
pub fn stacked_row(label: &str, content: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let label = NSTextField::labelWithString(&NSString::from_str(label), mtm);
    let row = new_view(mtm);
    for v in [&*label, content] {
        add(&row, v);
        activate([v
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET)]);
    }
    // The content spans the row, so that wrapping text knows its width.
    activate([
        label
            .trailingAnchor()
            .constraintLessThanOrEqualToAnchor_constant(&row.trailingAnchor(), -ROW_INSET),
        content
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&row.trailingAnchor(), -ROW_INSET),
        label
            .topAnchor()
            .constraintEqualToAnchor_constant(&row.topAnchor(), ROW_PADDING),
        content
            .topAnchor()
            .constraintEqualToAnchor_constant(&label.bottomAnchor(), 4.0),
        content
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&row.bottomAnchor(), -ROW_PADDING),
    ]);
    row
}

/// A title with a secondary line under it on the leading edge, `control` on the trailing edge:
/// an item in a list of things to act on.
pub fn subtitle_row(
    title: &str,
    subtitle: &str,
    control: &NSView,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let title = NSTextField::labelWithString(&NSString::from_str(title), mtm);
    layout::truncating(&title, NSLineBreakMode::ByTruncatingTail);
    let subtitle = small_secondary(subtitle, mtm);
    layout::truncating(&subtitle, NSLineBreakMode::ByTruncatingMiddle);
    let row = new_view(mtm);
    place_trailing(&row, control);
    for v in [&*title, &*subtitle] {
        add(&row, v);
        activate([
            v.leadingAnchor()
                .constraintEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET),
            v.trailingAnchor()
                .constraintLessThanOrEqualToAnchor_constant(
                    &control.leadingAnchor(),
                    -CONTROL_SPACING,
                ),
        ]);
    }
    activate([
        title
            .topAnchor()
            .constraintEqualToAnchor_constant(&row.topAnchor(), ROW_PADDING),
        subtitle
            .topAnchor()
            .constraintEqualToAnchor_constant(&title.bottomAnchor(), 2.0),
        subtitle
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&row.bottomAnchor(), -ROW_PADDING),
    ]);
    row
}

/// `buttons` side by side on the row's trailing edge, as under a form in System Settings.
pub fn button_row(buttons: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let buttons = hstack(buttons, mtm);
    let row = new_view(mtm);
    place_trailing(&row, &buttons);
    activate([buttons
        .leadingAnchor()
        .constraintGreaterThanOrEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET)]);
    min_height(&row);
    row
}

/// `content` from edge to edge of its group, with no padding: a list.
pub fn flush_row(content: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = new_view(mtm);
    fill(&row, content);
    row
}

/// The small +/− buttons under a list, on its leading edge.
pub fn bar_row(buttons: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let buttons = hstack(buttons, mtm);
    buttons.setSpacing(0.0);
    let row = new_view(mtm);
    add(&row, &buttons);
    activate([
        buttons
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET / 2.0),
        buttons
            .centerYAnchor()
            .constraintEqualToAnchor(&row.centerYAnchor()),
        row.heightAnchor().constraintEqualToConstant(BAR_HEIGHT),
    ]);
    row
}

/// Controls side by side, vertically centred, for one row: radio buttons, or a field and its
/// unit. It keeps its controls' size: it neither stretches across the row nor squeezes a text
/// field's height.
pub fn hstack(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::CenterY);
    stack.setSpacing(CONTROL_SPACING);
    stack.setHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultHigh,
        NSLayoutConstraintOrientation::Horizontal,
    );
    // Below a text field's compression resistance: at the same priority, the stack's hugging
    // won and the timeout field was drawn shorter than its text.
    stack.setHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Vertical,
    );
    stack
}

/// A read-only value that can be selected and copied, in secondary text.
pub fn value_label(
    text: &str,
    mode: NSLineBreakMode,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setSelectable(true);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    layout::truncating(&label, mode);
    label
}

/// Several lines of read-only secondary text, wrapped to the row, that can be selected.
pub fn value_lines(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setSelectable(true);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label
}

pub fn set_width(v: &NSView, width: f64, priority: NSLayoutPriority) {
    let c = v.widthAnchor().constraintEqualToConstant(width);
    c.setPriority(priority);
    c.setActive(true);
}

/// A view of a column: placed `gap` below the previous one, `inset` from both edges.
struct Block {
    view: Retained<NSView>,
    inset: f64,
    gap: f64,
}

/// `blocks` top to bottom, each spanning the column's width less its inset, with `padding`
/// above the first and below the last. Each block sits in a slot of its own, which holds its
/// gap, so that [`set_shown`] can collapse both.
fn column(blocks: Vec<Block>, padding: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let column = new_view(mtm);
    let mut above: Option<Retained<NSView>> = None;
    for block in blocks {
        let slot = new_view(mtm);
        add(&column, &slot);
        add(&slot, &block.view);
        // Not required, so that a collapsed slot can be shorter than its hidden block.
        let bottom = block
            .view
            .bottomAnchor()
            .constraintEqualToAnchor(&slot.bottomAnchor());
        bottom.setPriority(NSLayoutPriorityRequired - 1.0);
        activate([
            block
                .view
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&slot.leadingAnchor(), block.inset),
            block
                .view
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&slot.trailingAnchor(), -block.inset),
            block
                .view
                .topAnchor()
                .constraintEqualToAnchor_constant(&slot.topAnchor(), block.gap),
            bottom,
            slot.leadingAnchor()
                .constraintEqualToAnchor(&column.leadingAnchor()),
            slot.trailingAnchor()
                .constraintEqualToAnchor(&column.trailingAnchor()),
            match &above {
                None => slot
                    .topAnchor()
                    .constraintEqualToAnchor_constant(&column.topAnchor(), padding),
                Some(above) => slot
                    .topAnchor()
                    .constraintEqualToAnchor(&above.bottomAnchor()),
            },
        ]);
        above = Some(slot);
    }
    match above {
        Some(last) => activate([last
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&column.bottomAnchor(), -padding)]),
        None => activate([column.heightAnchor().constraintEqualToConstant(0.0)]),
    }
    column
}

/// `leading` on the row's leading edge and `trailing` on its trailing edge, both vertically
/// centred, at least [`CONTROL_SPACING`] apart.
fn row(leading: &NSView, trailing: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = new_view(mtm);
    add(&row, leading);
    activate([
        leading
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&row.leadingAnchor(), ROW_INSET),
        leading
            .centerYAnchor()
            .constraintEqualToAnchor(&row.centerYAnchor()),
        leading
            .topAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&row.topAnchor(), ROW_PADDING),
    ]);
    place_trailing(&row, trailing);
    activate([trailing
        .leadingAnchor()
        .constraintGreaterThanOrEqualToAnchor_constant(
            &leading.trailingAnchor(),
            CONTROL_SPACING,
        )]);
    min_height(&row);
    row
}

/// Puts `control` on `row`'s trailing edge, vertically centred, clear of its top and bottom.
fn place_trailing(row: &NSView, control: &NSView) {
    add(row, control);
    activate([
        control
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&row.trailingAnchor(), -ROW_INSET),
        control
            .centerYAnchor()
            .constraintEqualToAnchor(&row.centerYAnchor()),
        control
            .topAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&row.topAnchor(), ROW_PADDING),
    ]);
}

/// A row is [`ROW_MIN_HEIGHT`] unless its content needs more. The pull towards the minimum is
/// weaker than any control's compression resistance, so it never squeezes one.
fn min_height(row: &NSView) {
    let hug = row.heightAnchor().constraintEqualToConstant(ROW_MIN_HEIGHT);
    hug.setPriority(NSLayoutPriorityDefaultLow);
    activate([
        row.heightAnchor()
            .constraintGreaterThanOrEqualToConstant(ROW_MIN_HEIGHT),
        hug,
    ]);
}

fn footnote(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    label.setIdentifier(Some(&NSString::from_str(TEXT_ID)));
    label
}

fn small_secondary(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = layout::small_label(text, mtm);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setSelectable(true);
    label
}

/// A hairline between a group's rows.
fn separator(mtm: MainThreadMarker) -> Retained<NSView> {
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line.heightAnchor()
        .constraintEqualToConstant(1.0)
        .setActive(true);
    view(line)
}

/// Pins `view` to `container`'s edges.
pub fn fill(container: &NSView, view: &NSView) {
    add(container, view);
    activate([
        view.leadingAnchor()
            .constraintEqualToAnchor(&container.leadingAnchor()),
        view.trailingAnchor()
            .constraintEqualToAnchor(&container.trailingAnchor()),
        view.topAnchor()
            .constraintEqualToAnchor(&container.topAnchor()),
        view.bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
    ]);
}

fn new_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let v = NSView::new(mtm);
    v.setTranslatesAutoresizingMaskIntoConstraints(false);
    v
}

fn add(container: &NSView, view: &NSView) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(view);
}

fn activate<const N: usize>(constraints: [Retained<objc2_app_kit::NSLayoutConstraint>; N]) {
    for c in constraints {
        c.setActive(true);
    }
}
