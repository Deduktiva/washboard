//! Status colours, one function per meaning, so every error, warning and success looks the same
//! and a change of mind is one line.
//!
//! Functions rather than cached values: the system colours are dynamic and resolve per
//! appearance and Increase Contrast, and each call returns AppKit's shared object.

use objc2::rc::Retained;
use objc2_app_kit::NSColor;

/// Errors: the validity widget's error count, the editor's ruler markers and underlines, the
/// sidebar's invalid-request marker.
pub fn error() -> Retained<NSColor> {
    NSColor::systemRedColor()
}

/// Warnings: the validity widget's "schema not checked" and warning count, the editor's ruler
/// markers for lines with only warnings.
pub fn warning() -> Retained<NSColor> {
    NSColor::systemOrangeColor()
}

/// Success: the validity widget's "✓ Valid".
pub fn success() -> Retained<NSColor> {
    NSColor::systemGreenColor()
}
