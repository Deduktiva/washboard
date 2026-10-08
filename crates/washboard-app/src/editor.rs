//! The request editor: an `NSTextView` on TextKit 1 with a line-number ruler and XML
//! highlighting (PLAN §4 "Editor").
//!
//! TextKit 1, because highlighting uses the layout manager's temporary attributes: they colour
//! glyphs without touching the text storage, so undo and the saved text never see them.
//! After each edit only the tokens reported as changed are recoloured, over whole lines. The
//! text view keeps its own undo manager.
//!
//! A project window's editor is bound to the model's [`Editor`](washboard_ui_model::Editor):
//! the text view owns the visible text and undo, each change (typing, paste, undo) goes to
//! `App::edit` as the UTF-16 range and string AppKit reports, and colours come from the model
//! on `TokensChanged`. An unbound editor (the response body) keeps its own `TokenBuffer`.
//!
//! A bound editor also completes and explains from the model's schema: `complete:` (⌥⎋, or
//! typing `<`, a space in a tag or `="`) asks `App::completions` for the range and the
//! items, and resting the mouse on an element name shows `App::hover` as a tool tip.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
use std::ops::Range;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::sel;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSColor, NSEvent, NSFont, NSFontAttributeName, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSLayoutManager, NSMenu, NSMenuItem, NSResponder,
    NSRulerOrientation, NSRulerView, NSScrollView, NSStringDrawing, NSText, NSTextDelegate,
    NSTextInputTraitType, NSTextView, NSTextViewDelegate, NSToolTipTag, NSTrackingArea,
    NSTrackingAreaOptions, NSUnderlineColorAttributeName, NSUnderlineStyle,
    NSUnderlineStyleAttributeName, NSView, NSWritingToolsBehavior,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSInteger, NSNotFound, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSUInteger,
};
use washboard_core::xml::{TokenBuffer, TokenKind, utf16::Utf16Cursor};
use washboard_ui_model::{App, Completions, ProjectKey};

use crate::app::with_delegate;
use crate::text::{changed_range, completion_kinds, hover_text, utf16_edit};

const FONT_SIZE: f64 = 12.0;
const RULER_WIDTH: f64 = 44.0;

/// Colour of a token kind; `None` keeps the text colour.
fn color(kind: TokenKind) -> Option<Retained<NSColor>> {
    Some(match kind {
        TokenKind::TagName => NSColor::systemBlueColor(),
        TokenKind::TagPrefix | TokenKind::AttrPrefix | TokenKind::NamespaceDecl => {
            NSColor::systemPurpleColor()
        }
        TokenKind::AttrName => NSColor::systemTealColor(),
        TokenKind::AttrValue => NSColor::systemRedColor(),
        TokenKind::Punct => NSColor::secondaryLabelColor(),
        TokenKind::Comment | TokenKind::XmlDecl | TokenKind::ProcessingInstruction => {
            NSColor::systemGrayColor()
        }
        TokenKind::Doctype | TokenKind::CData => NSColor::systemBrownColor(),
        TokenKind::EntityRef | TokenKind::CharRef => NSColor::systemOrangeColor(),
        TokenKind::Error => NSColor::systemPinkColor(),
        TokenKind::Text => return None,
    })
}

#[derive(Debug, Default)]
pub struct RulerIvars {
    /// UTF-16 offsets of line starts; line `n` (1-based) starts at `line_starts[n - 1]`.
    line_starts: RefCell<Vec<usize>>,
    /// 1-based lines with an error, sorted.
    error_lines: RefCell<Vec<usize>>,
}

define_class!(
    // SAFETY:
    // - NSRulerView has no subclassing requirements; `new` calls its designated initializer.
    // - `LineNumberRuler` does not implement `Drop`.
    #[unsafe(super(NSRulerView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RulerIvars]
    #[derive(Debug)]
    pub struct LineNumberRuler;

    impl LineNumberRuler {
        // SAFETY: the signature matches `drawHashMarksAndLabelsInRect:`.
        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        fn draw_labels(&self, _rect: NSRect) {
            self.draw_line_numbers();
        }
    }
);

impl LineNumberRuler {
    fn new(scroll: &NSScrollView, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RulerIvars::default());
        // SAFETY: `initWithScrollView:orientation:` is NSRulerView's designated initializer.
        let this: Retained<Self> = unsafe {
            msg_send![super(this), initWithScrollView: scroll,
                orientation: NSRulerOrientation::VerticalRuler]
        };
        this.setRuleThickness(RULER_WIDTH);
        this
    }

    /// Lines (1-based) to mark with an error.
    pub fn set_error_lines(&self, mut lines: Vec<usize>) {
        lines.sort_unstable();
        lines.dedup();
        *self.ivars().error_lines.borrow_mut() = lines;
        self.setNeedsDisplay(true);
    }

    pub fn error_lines(&self) -> Vec<usize> {
        self.ivars().error_lines.borrow().clone()
    }

    /// Line numbers of the lines intersecting the visible rect, from the layout manager.
    pub fn visible_lines(&self) -> Range<usize> {
        let Some(text_view) = self.text_view() else {
            return 0..0;
        };
        let chars = visible_chars(&text_view);
        let starts = self.ivars().line_starts.borrow();
        let first = starts.partition_point(|&s| s <= chars.location).max(1);
        let last = starts
            .partition_point(|&s| s <= chars.location + chars.length)
            .max(first);
        first..last + 1
    }

    fn text_view(&self) -> Option<Retained<NSTextView>> {
        self.clientView()?.downcast::<NSTextView>().ok()
    }

    fn draw_line_numbers(&self) {
        let Some(text_view) = self.text_view() else {
            return;
        };
        // SAFETY: plain getters on a text view we built with a TextKit 1 stack.
        let Some(layout) = (unsafe { text_view.layoutManager() }) else {
            return;
        };
        let lines = self.visible_lines();
        let starts = self.ivars().line_starts.borrow();
        let errors = self.ivars().error_lines.borrow();
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(FONT_SIZE - 2.0, unsafe {
            // SAFETY: an immutable AppKit constant.
            NSFontWeightRegular
        });
        let origin_y = text_view.textContainerOrigin().y;
        let text_len = text_view.string().length();
        for line in lines {
            let Some(&start) = starts.get(line - 1) else {
                break;
            };
            let rect = if start >= text_len {
                layout.extraLineFragmentRect()
            } else {
                let glyph = layout.glyphIndexForCharacterAtIndex(start);
                // SAFETY: a null out-pointer is allowed; the glyph index is in range.
                unsafe {
                    layout
                        .lineFragmentRectForGlyphAtIndex_effectiveRange(glyph, std::ptr::null_mut())
                }
            };
            let point = self.convertPoint_fromView(
                NSPoint::new(0.0, rect.origin.y + origin_y),
                Some(&text_view),
            );
            let is_error = errors.binary_search(&line).is_ok();
            let color = if is_error {
                NSColor::systemOrangeColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            let label = NSString::from_str(&if is_error {
                format!("⚠{line}")
            } else {
                line.to_string()
            });
            let font_obj: &AnyObject = &font;
            let color_obj: &AnyObject = &color;
            // SAFETY: AppKit's attribute name constants are immutable statics.
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let attrs = NSDictionary::from_slices(&keys, &[font_obj, color_obj]);
            // SAFETY: the dictionary maps attribute names to a font and a colour.
            let size = unsafe { label.sizeWithAttributes(Some(&attrs)) };
            let x = RULER_WIDTH - size.width - 6.0;
            // SAFETY: as above; drawing happens inside AppKit's draw call.
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(x, point.y), Some(&attrs)) };
        }
    }
}

/// The UTF-16 character range laid out in the text view's visible rect.
fn visible_chars(text_view: &NSTextView) -> NSRange {
    // SAFETY: plain getters on a text view we built with a TextKit 1 stack.
    let (Some(layout), Some(container)) = (unsafe { text_view.layoutManager() }, unsafe {
        text_view.textContainer()
    }) else {
        return NSRange::new(0, 0);
    };
    let glyphs =
        layout.glyphRangeForBoundingRect_inTextContainer(text_view.visibleRect(), &container);
    // SAFETY: a null out-pointer is allowed.
    unsafe { layout.characterRangeForGlyphRange_actualGlyphRange(glyphs, std::ptr::null_mut()) }
}

#[derive(Debug, Default)]
pub struct EditorIvars {
    /// The project whose model editor this shows; `None` for a stand-alone text.
    key: Cell<Option<ProjectKey>>,
    /// Changes AppKit announced in `shouldChangeTextInRange…` and has not reported done yet:
    /// UTF-16 ranges of the text before them, and their replacements.
    pending: RefCell<Vec<(Range<usize>, String)>>,
    /// Set while the controller replaces the text itself.
    applying: Cell<bool>,
    /// Stand-alone only: the text as of the last highlight, to find what an edit changed.
    text: RefCell<String>,
    tokens: RefCell<TokenBuffer>,
    scroll: OnceCell<Retained<NSScrollView>>,
    text_view: OnceCell<Retained<NSTextView>>,
    ruler: OnceCell<Retained<LineNumberRuler>>,
    /// The element name the tool tip rect covers, and its text.
    tool_tip: RefCell<Option<(Range<usize>, String)>>,
}

define_class!(
    // SAFETY:
    // - NSTextView has no subclassing requirements; `text_view` calls its initializer.
    // - `EditorTextView` does not implement `Drop`.
    /// A text view whose completion range comes from the model, not from word boundaries:
    /// `cus:li` is replaced whole, and after `<` nothing is.
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    struct EditorTextView;

    impl EditorTextView {
        // SAFETY: the signature matches `rangeForUserCompletion`.
        #[unsafe(method(rangeForUserCompletion))]
        fn range_for_user_completion(&self) -> NSRange {
            let controller = self.delegate().and_then(|d| {
                let d: &AnyObject = d.as_ref();
                d.downcast_ref::<EditorController>().map(|c| c.retain())
            });
            match controller {
                Some(c) if c.ivars().key.get().is_some() => c.completion_range(),
                // SAFETY: calling the superclass's implementation of this very method.
                _ => unsafe { msg_send![super(self), rangeForUserCompletion] },
            }
        }
    }
);

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `EditorController` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = EditorIvars]
    #[derive(Debug)]
    pub struct EditorController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for EditorController {}

    // SAFETY: `NSTextDelegate` has no safety requirements.
    unsafe impl NSTextDelegate for EditorController {
        // SAFETY: the signature matches `textDidChange:`.
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            if self.ivars().applying.get() {
                return;
            }
            match self.ivars().key.get() {
                Some(key) => self.model_text_changed(key),
                None => self.text_changed(),
            }
        }
    }

    // SAFETY: `NSTextViewDelegate` has no safety requirements.
    unsafe impl NSTextViewDelegate for EditorController {
        // SAFETY: the signature matches `textView:menu:forEvent:atIndex:`.
        #[unsafe(method_id(textView:menu:forEvent:atIndex:))]
        fn context_menu(
            &self,
            _text_view: &NSTextView,
            menu: &NSMenu,
            _event: &NSEvent,
            _index: NSUInteger,
        ) -> Option<Retained<NSMenu>> {
            strip_prose_items(menu);
            Some(menu.retain())
        }

        // SAFETY: the signature matches `textView:shouldChangeTextInRange:replacementString:`.
        #[unsafe(method(textView:shouldChangeTextInRange:replacementString:))]
        fn should_change(
            &self,
            _text_view: &NSTextView,
            range: NSRange,
            replacement: Option<&NSString>,
        ) -> bool {
            // `None`: only attributes change.
            if let Some(replacement) = replacement
                && self.ivars().key.get().is_some()
                && !self.ivars().applying.get()
            {
                let range = range.location..range.location + range.length;
                self.ivars()
                    .pending
                    .borrow_mut()
                    .push((range, replacement.to_string()));
            }
            true
        }

        // SAFETY: the signature matches
        // `textView:completions:forPartialWordRange:indexOfSelectedItem:`.
        #[unsafe(method_id(textView:completions:forPartialWordRange:indexOfSelectedItem:))]
        fn completions_for(
            &self,
            _text_view: &NSTextView,
            words: &NSArray<NSString>,
            _range: NSRange,
            _index: *mut NSInteger,
        ) -> Retained<NSArray<NSString>> {
            self.completion_words(words)
        }
    }

    impl EditorController {
        // The tracking area added in `for_project`.
        // SAFETY: the signature matches `mouseMoved:`.
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let text_view = self.text_view();
            let point = text_view.convertPoint_fromView(event.locationInWindow(), None);
            let at = text_view.characterIndexForInsertionAtPoint(point);
            self.show_tool_tip_at(at);
        }

        // The tool tip rect's owner (`NSViewToolTipOwner`).
        // SAFETY: the signature matches `view:stringForToolTip:point:userData:`.
        #[unsafe(method_id(view:stringForToolTip:point:userData:))]
        fn string_for_tool_tip(
            &self,
            _view: &NSView,
            _tag: NSToolTipTag,
            _point: NSPoint,
            _data: *mut c_void,
        ) -> Retained<NSString> {
            let text = self.ivars().tool_tip.borrow();
            NSString::from_str(text.as_ref().map_or("", |(_, t)| t.as_str()))
        }
    }
);

impl EditorController {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EditorIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(true);
        // Shown only when the content does not fit, also with legacy (always-on) scrollers.
        scroll.setAutohidesScrollers(true);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        let text_view = text_view(mtm);
        // SAFETY: this controller owns the text view (through the scroll view), so it outlives
        // the text view's weak delegate reference.
        text_view.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        scroll.setDocumentView(Some(&text_view));

        let ruler = LineNumberRuler::new(&scroll, mtm);
        ruler.setClientView(Some(&text_view));
        scroll.setVerticalRulerView(Some(&ruler));
        scroll.setHasVerticalRuler(true);
        scroll.setRulersVisible(true);

        let _ = this.ivars().scroll.set(scroll);
        let _ = this.ivars().text_view.set(text_view);
        let _ = this.ivars().ruler.set(ruler);
        this
    }

    /// An editor for `key`'s request: edits go to the model and colours come from it.
    pub fn for_project(key: ProjectKey, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::new(mtm);
        this.ivars().key.set(Some(key));
        this.text_view().setEditable(false);
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        let owner: &AnyObject = &this;
        // SAFETY: this controller owns the text view and so the tracking area, which holds
        // its owner weakly; `mouseMoved:` is implemented above.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(owner),
                None,
            )
        };
        this.text_view().addTrackingArea(&area);
        this
    }

    /// The model's completions at the insertion point; `None` with a selection.
    pub fn completions_at_cursor(&self) -> Option<Completions> {
        let key = self.ivars().key.get()?;
        let selected = self.text_view().selectedRange();
        if selected.length > 0 {
            return None;
        }
        self.read(|app| app.completions(key, selected.location))
            .flatten()
            .filter(|c| !c.items.is_empty())
    }

    /// The completion list: the model's items for a bound editor, AppKit's words otherwise.
    fn completion_words(&self, words: &NSArray<NSString>) -> Retained<NSArray<NSString>> {
        if self.ivars().key.get().is_none() {
            return words.retain();
        }
        let items: Vec<Retained<NSString>> = self
            .completions_at_cursor()
            .map(|c| {
                c.items
                    .iter()
                    .map(|i| NSString::from_str(&i.text))
                    .collect()
            })
            .unwrap_or_default();
        NSArray::from_retained_slice(&items)
    }

    /// What `rangeForUserCompletion` answers: the model's range, or none to complete.
    fn completion_range(&self) -> NSRange {
        match self.completions_at_cursor() {
            Some(c) => NSRange::new(c.replace.start, c.replace.len()),
            None => NSRange::new(NSNotFound as usize, 0),
        }
    }

    /// The hover text for the element name at UTF-16 offset `at`, if any.
    pub fn hover_text(&self, at: usize) -> Option<String> {
        let key = self.ivars().key.get()?;
        self.read(|app| app.hover(key, at).as_ref().map(hover_text))
            .flatten()
    }

    /// Puts the tool tip on the element name at `at` (what the mouse rests on) and returns its
    /// text. One rect per name: AppKit asks for a rect's text once while the mouse is in it.
    pub fn show_tool_tip_at(&self, at: usize) -> Option<String> {
        let key = self.ivars().key.get()?;
        let hover = self
            .read(|app| {
                app.hover(key, at)
                    .map(|h| (h.range.clone(), hover_text(&h)))
            })
            .flatten();
        let shown = self
            .ivars()
            .tool_tip
            .borrow()
            .as_ref()
            .map(|(r, _)| r.clone());
        if hover.as_ref().map(|(r, _)| r) == shown.as_ref() {
            return hover.map(|(_, text)| text);
        }
        let text_view = self.text_view();
        text_view.removeAllToolTips();
        *self.ivars().tool_tip.borrow_mut() = hover.clone();
        let (range, text) = hover?;
        let layout = self.layout_manager();
        // SAFETY: plain getters on a TextKit 1 stack; a null actual range is allowed.
        let rect = unsafe {
            let container = text_view.textContainer()?;
            let glyphs = layout.glyphRangeForCharacterRange_actualCharacterRange(
                NSRange::new(range.start, range.len()),
                std::ptr::null_mut(),
            );
            layout.boundingRectForGlyphRange_inTextContainer(glyphs, &container)
        };
        let origin = text_view.textContainerOrigin();
        let rect = NSRect::new(
            NSPoint::new(rect.origin.x + origin.x, rect.origin.y + origin.y),
            rect.size,
        );
        let owner: &AnyObject = self;
        // SAFETY: this controller owns the text view, so it outlives the tool tip's
        // unretained owner reference; it implements `view:stringForToolTip:point:userData:`.
        unsafe {
            text_view.addToolTipRect_owner_userData(rect, owner, std::ptr::null_mut());
        }
        Some(text)
    }

    /// Shows the model's editor text, or nothing without a selected request
    /// (`EditorReplaced`). Starts a new undo history.
    pub fn show_model_text(&self) {
        let Some(key) = self.ivars().key.get() else {
            return;
        };
        let text = with_delegate(self.mtm(), |d| {
            d.read(|app| {
                app.project(key)
                    .and_then(|w| w.editor())
                    .map(|e| e.text().to_owned())
            })
        })
        .flatten()
        .flatten();
        let text_view = self.text_view();
        self.ivars().pending.borrow_mut().clear();
        let was = self.ivars().applying.replace(true);
        text_view.setString(&NSString::from_str(text.as_deref().unwrap_or("")));
        self.ivars().applying.set(was);
        text_view.setEditable(text.is_some());
        if let Some(undo) = text_view.undoManager() {
            undo.removeAllActions();
        }
        let text = text.unwrap_or_default();
        self.update_line_starts(&text);
        self.recolor(0..text_view.string().length());
    }

    /// Recolours the UTF-16 range `range`, widened to whole lines, from the model's tokens
    /// (`TokensChanged`).
    pub fn recolor(&self, range: Range<usize>) {
        let Some(key) = self.ivars().key.get() else {
            return;
        };
        let string = self.text_view().string();
        let len = string.length();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let lines = string.lineRangeForRange(NSRange::new(start, end - start));
        let lines = lines.location..lines.location + lines.length;
        let tokens = with_delegate(self.mtm(), |d| {
            d.read(|app| {
                app.project(key)
                    .and_then(|w| w.editor())
                    .map(|e| e.tokens_utf16(lines.clone()))
            })
        })
        .flatten()
        .flatten()
        .unwrap_or_default();
        self.paint(lines, tokens);
    }

    /// The scroll view to put into a window.
    pub fn view(&self) -> &NSScrollView {
        self.ivars().scroll.get().expect("set in new()")
    }

    pub fn text_view(&self) -> &NSTextView {
        self.ivars().text_view.get().expect("set in new()")
    }

    pub fn ruler(&self) -> &LineNumberRuler {
        self.ivars().ruler.get().expect("set in new()")
    }

    pub fn layout_manager(&self) -> Retained<NSLayoutManager> {
        // SAFETY: a plain getter; the view was built with a TextKit 1 stack.
        unsafe { self.text_view().layoutManager() }.expect("TextKit 1 text view")
    }

    /// Replaces the whole text (opening a request), outside undo, and highlights it.
    pub fn set_text(&self, text: &str) {
        self.text_view().setString(&NSString::from_str(text));
        *self.ivars().tokens.borrow_mut() = TokenBuffer::new(text);
        *self.ivars().text.borrow_mut() = text.to_owned();
        self.update_line_starts(text);
        self.highlight(text, 0..text.len());
    }

    /// Selects the UTF-16 range `range`, clamped to the text, and scrolls to it.
    pub fn select_range(&self, range: Range<usize>) {
        let len = self.text_view().string().length();
        let start = range.start.min(len);
        let range = NSRange::new(start, range.end.clamp(start, len) - start);
        self.text_view().setSelectedRange(range);
        self.text_view().scrollRangeToVisible(range);
    }

    /// Underlines the UTF-16 ranges `ranges` as errors, replacing the previous underlines. An
    /// empty range marks the character it points at. Temporary attributes, like the colours.
    pub fn set_underlines(&self, ranges: Vec<Range<usize>>) {
        let layout = self.layout_manager();
        let len = self.text_view().string().length();
        // SAFETY: immutable AppKit constants.
        let (style, color) =
            unsafe { (NSUnderlineStyleAttributeName, NSUnderlineColorAttributeName) };
        for key in [style, color] {
            layout.removeTemporaryAttribute_forCharacterRange(key, NSRange::new(0, len));
        }
        let single = NSNumber::new_isize(NSUnderlineStyle::Single.0);
        let orange = NSColor::systemOrangeColor();
        for range in ranges {
            let start = range.start.min(len);
            let end = range.end.max(start + 1).min(len);
            if end <= start {
                continue;
            }
            let range = NSRange::new(start, end - start);
            // SAFETY: the underline style is an `NSNumber`, the underline colour an `NSColor`.
            unsafe {
                layout.addTemporaryAttribute_value_forCharacterRange(style, &single, range);
                layout.addTemporaryAttribute_value_forCharacterRange(color, &orange, range);
            }
        }
    }

    /// Selects line `line` (1-based) and scrolls to it; used by the issues bar.
    pub fn select_line(&self, line: usize) {
        let starts = self.ruler().ivars().line_starts.borrow();
        let Some(&start) = starts.get(line.saturating_sub(1)) else {
            return;
        };
        let end = starts
            .get(line)
            .copied()
            .unwrap_or_else(|| self.text_view().string().length());
        let range = NSRange::new(start, end - start);
        self.text_view().setSelectedRange(range);
        self.text_view().scrollRangeToVisible(range);
    }

    /// Sends what AppKit changed to the model. Changes it announced are sent as announced, in
    /// reverse order so each range is still valid; otherwise, or if the model's text
    /// disagrees afterwards, the difference to the model's text is sent instead.
    fn model_text_changed(&self, key: ProjectKey) {
        let mut edits = std::mem::take(&mut *self.ivars().pending.borrow_mut());
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        let view = self.text_view().string().to_string();
        self.update_line_starts(&view);
        let Some(delegate) = with_delegate(self.mtm(), |d| d.retain()) else {
            return;
        };
        let title = "Could not edit the request";
        let typed = match edits.as_slice() {
            [(range, text)] if range.is_empty() => Some((range.start, text.clone())),
            _ => None,
        };
        if !edits.is_empty() {
            delegate.command(title, |app| {
                edits
                    .iter()
                    .try_for_each(|(range, text)| app.edit(key, range.clone(), text))
            });
        }
        let fix = delegate
            .read(|app| {
                let model = app.project(key)?.editor()?.text();
                (model != view).then(|| utf16_edit(model, &view))
            })
            .flatten();
        if let Some((range, text)) = fix {
            delegate.command(title, |app| app.edit(key, range, &text));
        }
        if let Some((at, text)) = typed {
            self.complete_after_typing(at, &text);
        }
    }

    /// Opens the completion list after `<`, a space in a tag or `="` was typed at `at`, if the
    /// model has completions of the matching kind there. Not on undo or redo.
    fn complete_after_typing(&self, at: usize, typed: &str) {
        let text_view = self.text_view();
        if text_view
            .undoManager()
            .is_some_and(|u| u.isUndoing() || u.isRedoing())
        {
            return;
        }
        let string = text_view.string();
        let before = at
            .checked_sub(1)
            .and_then(|i| char::from_u32(string.characterAtIndex(i).into()));
        let kinds = completion_kinds(before, typed);
        if kinds.is_empty() {
            return;
        }
        let offered = self
            .completions_at_cursor()
            .is_some_and(|c| c.items.iter().any(|i| kinds.contains(&i.kind)));
        if offered {
            // After this edit has finished: `complete:` edits the text itself.
            // SAFETY: `complete:` takes the sender; a nil sender is allowed.
            unsafe {
                let _: () = msg_send![
                    text_view,
                    performSelector: sel!(complete:),
                    withObject: None::<&AnyObject>,
                    afterDelay: 0.0f64
                ];
            }
        }
    }

    fn read<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        with_delegate(self.mtm(), |d| d.read(f)).flatten()
    }

    fn text_changed(&self) {
        let new = self.text_view().string().to_string();
        let old = std::mem::take(&mut *self.ivars().text.borrow_mut());
        let (old_range, new_len) = changed_range(&old, &new);
        let changed = self
            .ivars()
            .tokens
            .borrow_mut()
            .edit(&new, old_range, new_len);
        self.update_line_starts(&new);
        self.highlight(&new, changed);
        *self.ivars().text.borrow_mut() = new;
    }

    fn update_line_starts(&self, text: &str) {
        let mut starts = vec![0];
        let mut cursor = Utf16Cursor::new(text);
        starts.extend(
            text.match_indices('\n')
                .map(|(i, _)| cursor.utf16_at(i + 1)),
        );
        *self.ruler().ivars().line_starts.borrow_mut() = starts;
        self.ruler().setNeedsDisplay(true);
    }

    /// Recolours `bytes` of `text`, widened to whole lines.
    fn highlight(&self, text: &str, bytes: Range<usize>) {
        let start = text[..bytes.start.min(text.len())]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let end = text[bytes.end.min(text.len())..]
            .find('\n')
            .map_or(text.len(), |i| bytes.end + i);
        let lines = Utf16Cursor::new(text).utf16_range(start..end);
        let mut cursor = Utf16Cursor::new(text);
        let tokens = self
            .ivars()
            .tokens
            .borrow()
            .tokens_in(start..end)
            .iter()
            .map(|t| (cursor.utf16_range(t.span()), t.kind))
            .collect();
        self.paint(lines, tokens);
    }

    /// Replaces the colours of the UTF-16 range `lines` with those of `tokens`.
    fn paint(&self, lines: Range<usize>, tokens: Vec<(Range<usize>, TokenKind)>) {
        let layout = self.layout_manager();
        // SAFETY: an immutable AppKit constant.
        let key = unsafe { NSForegroundColorAttributeName };
        layout.removeTemporaryAttribute_forCharacterRange(
            key,
            NSRange::new(lines.start, lines.end - lines.start),
        );
        for (span, kind) in tokens {
            let Some(color) = color(kind) else {
                continue;
            };
            let value: &AnyObject = &color;
            // SAFETY: the value for the foreground colour attribute is an `NSColor`.
            unsafe {
                layout.addTemporaryAttribute_value_forCharacterRange(
                    key,
                    value,
                    NSRange::new(span.start, span.end - span.start),
                )
            };
        }
    }
}

fn text_view(mtm: MainThreadMarker) -> Retained<NSTextView> {
    let this = EditorTextView::alloc(mtm).set_ivars(());
    // SAFETY: `initUsingTextLayoutManager:` is a designated initializer of NSTextView.
    let text_view: Retained<EditorTextView> =
        unsafe { msg_send![super(this), initUsingTextLayoutManager: false] };
    let text_view = Retained::into_super(text_view);
    text_view.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(600.0, 400.0),
    ));
    // No wrapping: XML lines keep their shape; the view grows in both directions.
    text_view.setHorizontallyResizable(true);
    text_view.setVerticallyResizable(true);
    text_view.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    text_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    // SAFETY: a plain getter; `initUsingTextLayoutManager(false)` builds a TextKit 1 stack.
    if let Some(container) = unsafe { text_view.textContainer() } {
        container.setContainerSize(NSSize::new(f64::MAX, f64::MAX));
        container.setWidthTracksTextView(false);
    }
    // SAFETY: an immutable AppKit constant.
    let weight = unsafe { NSFontWeightRegular };
    text_view.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        FONT_SIZE, weight,
    )));
    text_view.setRichText(false);
    text_view.setAllowsUndo(true);
    text_view.setUsesFindBar(true);
    code_text(&text_view);
    text_view
}

/// Turns off everything meant for prose in a text view that shows XML: substitutions,
/// spelling and grammar checking, link and data detection, inline predictions and Writing
/// Tools. Each of them would change or flag markup, and Writing Tools offers to rewrite it.
pub(crate) fn code_text(text_view: &NSTextView) {
    text_view.setAutomaticQuoteSubstitutionEnabled(false);
    text_view.setAutomaticDashSubstitutionEnabled(false);
    text_view.setAutomaticTextReplacementEnabled(false);
    text_view.setAutomaticSpellingCorrectionEnabled(false);
    text_view.setContinuousSpellCheckingEnabled(false);
    text_view.setGrammarCheckingEnabled(false);
    text_view.setAutomaticLinkDetectionEnabled(false);
    text_view.setAutomaticDataDetectionEnabled(false);
    text_view.setSmartInsertDeleteEnabled(false);
    text_view.setInlinePredictionType(NSTextInputTraitType::No);
    text_view.setWritingToolsBehavior(NSWritingToolsBehavior::None);
}

/// Context menu actions whose submenus are for prose: Spelling and Grammar, Substitutions,
/// Transformations, Font and Layout Orientation.
const PROSE_ACTIONS: &[&str] = &[
    "showGuessPanel:",
    "checkSpelling:",
    "toggleContinuousSpellChecking:",
    "orderFrontSubstitutionsPanel:",
    "toggleSmartInsertDelete:",
    "uppercaseWord:",
    "orderFrontFontPanel:",
    "changeLayoutOrientation:",
];

/// Removes the prose submenus from a text view's context menu, leaving cut, copy, paste,
/// Look Up, Share and Speech.
fn strip_prose_items(menu: &NSMenu) {
    let prose = |item: &NSMenuItem| {
        item.submenu().is_some_and(|sub| {
            sub.itemArray().iter().any(|i| {
                i.action()
                    .is_some_and(|a| PROSE_ACTIONS.contains(&a.name().to_str().unwrap_or("")))
            })
        })
    };
    for index in (0..menu.numberOfItems()).rev() {
        if menu.itemAtIndex(index).is_some_and(|item| prose(&item)) {
            menu.removeItemAtIndex(index);
        }
    }
    // A separator left first, last or next to another one.
    let mut previous_separator = true;
    let mut index = 0;
    while index < menu.numberOfItems() {
        let separator = menu.itemAtIndex(index).is_some_and(|i| i.isSeparatorItem());
        if separator && previous_separator {
            menu.removeItemAtIndex(index);
        } else {
            previous_separator = separator;
            index += 1;
        }
    }
    let last = menu.numberOfItems() - 1;
    if last >= 0 && menu.itemAtIndex(last).is_some_and(|i| i.isSeparatorItem()) {
        menu.removeItemAtIndex(last);
    }
}
