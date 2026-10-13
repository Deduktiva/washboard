//! The request editor: an `NSTextView` on TextKit 1 with a line-number ruler and XML highlighting
//! (`docs/specs/editor.md`).
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
//! items, and resting the mouse on an element name shows `App::hover` as a tool tip. On an
//! underlined error the tool tip starts with the error's message.

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
    NSAutoresizingMaskOptions, NSColor, NSEvent, NSFontAttributeName,
    NSForegroundColorAttributeName, NSLayoutManager, NSMenu, NSMenuItem, NSResponder,
    NSRulerOrientation, NSRulerView, NSScrollView, NSStringDrawing, NSText, NSTextDelegate,
    NSTextView, NSTextViewDelegate, NSToolTipTag, NSTrackingArea, NSTrackingAreaOptions,
    NSUnderlineColorAttributeName, NSUnderlineStyle, NSUnderlineStyleAttributeName, NSView,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSInteger, NSNotFound, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSUInteger,
};
use washboard_core::xml::utf16::{Utf16Cursor, changed_range, utf16_edit};
use washboard_core::xml::{TokenBuffer, TokenKind};
use washboard_ui_model::{Completions, Issue, ProjectKey};

use crate::app::{ModelAccess, with_delegate};
use crate::text::{LineMarker, completion_kinds, hover_text, line_markers, text_tool_tip};
use crate::{colors, views};

const RULER_WIDTH: f64 = 44.0;

/// Every token kind with a colour of its own, for checking the palette's contrast in both
/// appearances.
pub fn highlight_palette() -> Vec<(TokenKind, Retained<NSColor>)> {
    [
        TokenKind::TagName,
        TokenKind::TagPrefix,
        TokenKind::AttrName,
        TokenKind::AttrValue,
        TokenKind::Punct,
        TokenKind::Comment,
        TokenKind::Doctype,
        TokenKind::EntityRef,
        TokenKind::CharRef,
        TokenKind::Error,
    ]
    .into_iter()
    .filter_map(|kind| Some((kind, color(kind)?)))
    .collect()
}

/// Colour of a token kind; `None` keeps the text colour. Only dynamic system colours, which
/// AppKit resolves for the current appearance at draw time, so a switch to or from Dark Mode
/// needs no re-highlighting.
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
    /// Lines with issues, sorted by line.
    markers: RefCell<Vec<LineMarker>>,
    /// The tool tip rects of the markers drawn last, in ruler coordinates, and their lines.
    /// Rebuilt only when they move: re-adding a rect would hide a tool tip being shown.
    tool_tips: RefCell<Vec<(usize, NSRect)>>,
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

        // The tool tip rects' owner (`NSViewToolTipOwner`); `data` is the marker's line.
        // SAFETY: the signature matches `view:stringForToolTip:point:userData:`.
        #[unsafe(method_id(view:stringForToolTip:point:userData:))]
        fn string_for_tool_tip(
            &self,
            _view: &NSView,
            _tag: NSToolTipTag,
            _point: NSPoint,
            data: *mut c_void,
        ) -> Retained<NSString> {
            NSString::from_str(&self.tool_tip(data as usize).unwrap_or_default())
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

    /// Marks the lines of `issues`: a red `!` for errors, an orange `⚠` for lines with only
    /// warnings, so the two differ in more than colour. Hovering a marker lists its line's
    /// messages.
    pub fn set_issues(&self, issues: &[Issue]) {
        *self.ivars().markers.borrow_mut() = line_markers(issues);
        self.setNeedsDisplay(true);
    }

    /// Lines (1-based) marked with an error.
    pub fn error_lines(&self) -> Vec<usize> {
        self.marked_lines(true)
    }

    /// Lines (1-based) marked with a warning: they have warnings and no error.
    pub fn warning_lines(&self) -> Vec<usize> {
        self.marked_lines(false)
    }

    fn marked_lines(&self, error: bool) -> Vec<usize> {
        let markers = self.ivars().markers.borrow();
        markers
            .iter()
            .filter(|m| m.error == error)
            .map(|m| m.line)
            .collect()
    }

    /// The tool tip of line `line`'s marker: its issues' messages, errors first.
    pub fn tool_tip(&self, line: usize) -> Option<String> {
        let markers = self.ivars().markers.borrow();
        let at = markers.binary_search_by_key(&line, |m| m.line).ok()?;
        Some(markers[at].tool_tip.clone())
    }

    /// Lines with a tool tip rect, from the last draw: the visible markers.
    pub fn tool_tip_lines(&self) -> Vec<usize> {
        self.ivars()
            .tool_tips
            .borrow()
            .iter()
            .map(|(l, _)| *l)
            .collect()
    }

    /// Puts a tool tip rect on each drawn marker, the ruler's width by its line's height.
    /// The rects follow scrolling, so they are placed where the markers are drawn.
    fn set_tool_tips(&self, tips: Vec<(usize, NSRect)>) {
        if *self.ivars().tool_tips.borrow() == tips {
            return;
        }
        self.removeAllToolTips();
        let owner: &AnyObject = self;
        for &(line, rect) in &tips {
            // SAFETY: the ruler owns its tool tips, so it outlives their unretained owner
            // reference; it implements `view:stringForToolTip:point:userData:`, which reads
            // `userData` back as the line number and never dereferences it.
            unsafe {
                self.addToolTipRect_owner_userData(rect, owner, line as *mut c_void);
            }
        }
        *self.ivars().tool_tips.borrow_mut() = tips;
    }

    /// Line numbers of the lines intersecting the visible rect, from the layout manager.
    pub fn visible_lines(&self) -> Range<usize> {
        let Some(text_view) = self.text_view() else {
            return 0..0;
        };
        let chars = visible_chars(&text_view);
        let starts = self.ivars().line_starts.borrow();
        let first = starts.partition_point(|&s| s <= chars.location).max(1);
        let last = starts.partition_point(|&s| s <= chars.end()).max(first);
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
        let markers = self.ivars().markers.borrow();
        let mut tips = Vec::new();
        let font = views::code_font_for_ruler();
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
            let marker = markers
                .binary_search_by_key(&line, |m| m.line)
                .ok()
                .map(|at| &markers[at]);
            let color = match marker {
                Some(m) if m.error => colors::error(),
                Some(_) => colors::warning(),
                None => NSColor::secondaryLabelColor(),
            };
            if marker.is_some() {
                let line_rect = NSRect::new(
                    NSPoint::new(0.0, rect.origin.y + origin_y),
                    NSSize::new(1.0, rect.size.height),
                );
                let tip = self.convertRect_fromView(line_rect, Some(&text_view));
                tips.push((
                    line,
                    NSRect::new(
                        NSPoint::new(0.0, tip.origin.y),
                        NSSize::new(RULER_WIDTH, tip.size.height),
                    ),
                ));
            }
            let label = NSString::from_str(&match marker {
                Some(m) if m.error => format!("!{line}"),
                Some(_) => format!("⚠{line}"),
                None => line.to_string(),
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
        self.set_tool_tips(tips);
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
    /// The range the text's tool tip rect covers, and its text.
    tool_tip: RefCell<Option<(Range<usize>, String)>>,
    /// The underlined UTF-16 ranges as drawn, and their issues' messages.
    underlines: RefCell<Vec<(Range<usize>, String)>>,
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
                let range = Range::from(range);
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
            Some(c) => NSRange::from(c.replace),
            None => NSRange::new(NSNotFound as usize, 0),
        }
    }

    /// The hover text for the element name at UTF-16 offset `at`, if any.
    pub fn hover_text(&self, at: usize) -> Option<String> {
        let key = self.ivars().key.get()?;
        self.read(|app| app.hover(key, at).as_ref().map(hover_text))
            .flatten()
    }

    /// Puts the tool tip at `at` (what the mouse rests on) and returns its text: the messages
    /// of the underlined issues there, then the element name's hover. One rect per range:
    /// AppKit asks for a rect's text once while the mouse is in it.
    pub fn show_tool_tip_at(&self, at: usize) -> Option<String> {
        let key = self.ivars().key.get()?;
        let hover = self
            .read(|app| {
                app.hover(key, at)
                    .map(|h| (h.range.clone(), hover_text(&h)))
            })
            .flatten();
        let tip = text_tool_tip(&self.ivars().underlines.borrow(), hover, at);
        if *self.ivars().tool_tip.borrow() == tip {
            return tip.map(|(_, text)| text);
        }
        let text_view = self.text_view();
        text_view.removeAllToolTips();
        *self.ivars().tool_tip.borrow_mut() = tip.clone();
        let (range, text) = tip?;
        let layout = self.layout_manager();
        // SAFETY: plain getters on a TextKit 1 stack; a null actual range is allowed.
        let rect = unsafe {
            let container = text_view.textContainer()?;
            let glyphs = layout.glyphRangeForCharacterRange_actualCharacterRange(
                NSRange::from(range.clone()),
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
        let text = self
            .read_project(key, |w| w.editor().map(|e| e.text().to_owned()))
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
        let lines = Range::from(string.lineRangeForRange(NSRange::from(start..end)));
        let tokens = self
            .read_project(key, |w| w.editor().map(|e| e.tokens_utf16(lines.clone())))
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

    /// Replaces the UTF-16 range `range` with `text` as the user would, so it is one undo step
    /// named `action` and reaches the model as an edit, then selects `selection`.
    pub fn apply_edit(
        &self,
        range: Range<usize>,
        text: &str,
        selection: Range<usize>,
        action: &str,
    ) {
        let text_view = self.text_view();
        let range = NSRange::new(range.start, range.end.saturating_sub(range.start));
        let text = NSString::from_str(text);
        // Typing before and after stays out of this step's undo.
        text_view.breakUndoCoalescing();
        if !text_view.shouldChangeTextInRange_replacementString(range, Some(&text)) {
            return;
        }
        // SAFETY: a plain getter; the text view always has a text storage.
        let Some(storage) = (unsafe { text_view.textStorage() }) else {
            return;
        };
        storage.replaceCharactersInRange_withString(range, &text);
        text_view.didChangeText();
        text_view.breakUndoCoalescing();
        if let Some(undo) = text_view.undoManager() {
            undo.setActionName(&NSString::from_str(action));
        }
        self.select_range(selection);
    }

    /// Selects the UTF-16 range `range`, clamped to the text, and scrolls to it.
    pub fn select_range(&self, range: Range<usize>) {
        let len = self.text_view().string().length();
        let start = range.start.min(len);
        let range = NSRange::from(start..range.end.clamp(start, len));
        self.text_view().setSelectedRange(range);
        self.text_view().scrollRangeToVisible(range);
    }

    /// Underlines the UTF-16 ranges of `issues` as errors, replacing the previous underlines,
    /// and keeps their messages for the tool tip. An empty range marks the character it points
    /// at. Temporary attributes, like the colours.
    pub fn set_underlines(&self, issues: Vec<(Range<usize>, String)>) {
        let layout = self.layout_manager();
        let len = self.text_view().string().length();
        // SAFETY: immutable AppKit constants.
        let (style, color) =
            unsafe { (NSUnderlineStyleAttributeName, NSUnderlineColorAttributeName) };
        for key in [style, color] {
            layout.removeTemporaryAttribute_forCharacterRange(key, NSRange::new(0, len));
        }
        let single = NSNumber::new_isize(NSUnderlineStyle::Single.0);
        let error = colors::error();
        let mut drawn = Vec::new();
        for (range, message) in issues {
            let start = range.start.min(len);
            let end = range.end.max(start + 1).min(len);
            if end <= start {
                continue;
            }
            drawn.push((start..end, message));
            let range = NSRange::from(start..end);
            // SAFETY: the underline style is an `NSNumber`, the underline colour an `NSColor`.
            unsafe {
                layout.addTemporaryAttribute_value_forCharacterRange(style, &single, range);
                layout.addTemporaryAttribute_value_forCharacterRange(color, &error, range);
            }
        }
        *self.ivars().underlines.borrow_mut() = drawn;
        // The next mouse move puts up a tool tip with the new messages.
        self.text_view().removeAllToolTips();
        *self.ivars().tool_tip.borrow_mut() = None;
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
        let range = NSRange::from(start..end);
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
        let tokens = self.ivars().tokens.borrow().tokens_utf16(text, start..end);
        self.paint(lines, tokens);
    }

    /// Replaces the colours of the UTF-16 range `lines` with those of `tokens`.
    fn paint(&self, lines: Range<usize>, tokens: Vec<(Range<usize>, TokenKind)>) {
        let layout = self.layout_manager();
        // SAFETY: an immutable AppKit constant.
        let key = unsafe { NSForegroundColorAttributeName };
        layout.removeTemporaryAttribute_forCharacterRange(key, NSRange::from(lines));
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
                    NSRange::from(span),
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
    // Set rather than left to the defaults: these are the colours the highlighting palette is
    // checked against, in both appearances.
    text_view.setTextColor(Some(&NSColor::textColor()));
    text_view.setBackgroundColor(&NSColor::textBackgroundColor());
    text_view.setAllowsUndo(true);
    text_view.setUsesFindBar(true);
    views::configure_text_view_for_code(&text_view);
    text_view
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

impl ModelAccess for EditorController {}
