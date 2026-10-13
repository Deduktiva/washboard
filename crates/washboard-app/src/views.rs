use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSFont, NSFontWeightRegular, NSScrollView, NSTextInputTraitType, NSTextView,
    NSWritingToolsBehavior,
};
use objc2_foundation::NSString;

/// A scrolling, read-only code text view without highlighting.
pub(crate) fn read_only_text(mtm: MainThreadMarker) -> Retained<NSScrollView> {
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setAutohidesScrollers(true);
    if let Some(text) = scroll_text_view(&scroll) {
        text.setEditable(false);
        configure_text_view_for_code(&text);
    }
    scroll
}

pub fn scroll_text_view(scroll: &NSScrollView) -> Option<Retained<NSTextView>> {
    scroll.documentView()?.downcast::<NSTextView>().ok()
}

// TODO: make configurable or use system defaults
const _CODE_FONT_SIZE: f64 = 12.0;

/// A font useful for code views/editors, also used wherever XML or headers are shown as text.
fn code_font() -> Retained<NSFont> {
    // SAFETY: an immutable AppKit constant.
    let weight = unsafe { NSFontWeightRegular };
    NSFont::monospacedSystemFontOfSize_weight(_CODE_FONT_SIZE, weight)
}

/// A font useful for the line ruler in code views/editors, matching code_font.
pub fn code_font_for_ruler() -> Retained<NSFont> {
    // SAFETY: an immutable AppKit constant.
    let weight = unsafe { NSFontWeightRegular };
    NSFont::monospacedDigitSystemFontOfSize_weight(_CODE_FONT_SIZE - 2.0, weight)
}

/// Turns off everything meant for prose in a text view that shows XML or other code: substitutions,
/// spelling and grammar checking, link and data detection, inline predictions and Writing
/// Tools. Each of them would change or flag markup, and Writing Tools offers to rewrite it.
pub(crate) fn configure_text_view_for_code(text_view: &NSTextView) {
    // `text_view` starts as rich text in the proportional system font; plain
    // text keeps the font for every `setString`.
    text_view.setRichText(false);
    text_view.setFont(Some(&code_font()));

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

/// Replaces the text of a [`read_only_text`] view.
pub(crate) fn read_only_text_set_text(scroll: &NSScrollView, text: &str) {
    if let Some(view) = scroll_text_view(scroll) {
        view.setString(&NSString::from_str(text));
    }
}

/// The text of a [`read_only_text`] view.
pub(crate) fn read_only_text_get_text(scroll: &NSScrollView) -> String {
    scroll_text_view(scroll)
        .map(|v| v.string().to_string())
        .unwrap_or_default()
}
