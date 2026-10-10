//! Status colours, one function per meaning, so every error, warning and success looks the same
//! and a change of mind is one line.
//!
//! Functions rather than cached values: the system colours are dynamic and resolve per
//! appearance and Increase Contrast, and each call returns AppKit's shared object.

use objc2::rc::Retained;
use objc2_app_kit::NSColor;

/// Errors: the request bar's "XML error", the issues bar's error count, the editor's ruler
/// markers and underlines, the sidebar's invalid-request marker.
pub fn error() -> Retained<NSColor> {
    NSColor::systemRedColor()
}

/// Warnings: the issues bar's "schema not checked" and warning count, the HTTP log's skipped
/// certificate check.
pub fn warning() -> Retained<NSColor> {
    NSColor::systemOrangeColor()
}

/// Success: "Well-formed" in the request bar, "✓ Valid" in the issues bar.
pub fn success() -> Retained<NSColor> {
    NSColor::systemGreenColor()
}
