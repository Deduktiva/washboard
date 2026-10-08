//! Stack view helpers shared by the windows.
//!
//! NSStackView's default distribution (gravity areas) gives a view without an intrinsic size,
//! such as a scroll view, no height at all, so its neighbours draw over it. Columns that hold
//! scroll views use `Fill`, where the views that hug least (scroll views) take the slack.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh,
    NSLayoutPriorityDefaultLow, NSStackView, NSStackViewDistribution,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets};

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
