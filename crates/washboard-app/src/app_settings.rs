//! The app's own settings (WP-FORMAT-XML): the indent width and format on save, kept in the
//! user defaults where macOS keeps app settings. The Settings window (`settings_window`) shows
//! and changes them.

use objc2_foundation::{NSString, NSUserDefaults};
use washboard_ui_model::{FormatSettings, INDENT_RANGE};

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
