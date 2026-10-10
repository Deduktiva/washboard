//! Format XML (⌃I) and its settings (PLAN §4, `docs/TASKS.md` WP-FORMAT-XML). The model
//! computes the new text; the front end applies it through the text widget so it is one undo
//! step, and the widget reports it back as an ordinary edit.

use std::ops::Range;

use washboard_core::xml::{self, utf16::Utf16Cursor, utf16::utf16_edit, utf16::utf16_to_byte};

use crate::app::{App, ModelError, ProjectKey};

pub use washboard_core::xml::INDENT_RANGE;

/// The app's formatting settings. The front end keeps them where its platform keeps settings
/// (the user defaults on macOS) and hands them over at launch and on change; the model stores
/// none of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatSettings {
    /// Spaces per level, for Format XML, new requests and responses alike.
    pub indent: usize,
    /// Format the open request on Save All (⌘S); never on autosave or send.
    pub on_save: bool,
}

impl Default for FormatSettings {
    fn default() -> Self {
        FormatSettings {
            indent: xml::DEFAULT_INDENT,
            on_save: false,
        }
    }
}

/// The edit that formats the open request: replace the UTF-16 `range` of the current text
/// with `text`, then select `selection` (UTF-16, in the result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reformat {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Range<usize>,
}

impl App {
    pub fn format_settings(&self) -> FormatSettings {
        self.format
    }

    /// Takes effect for the next format, new request and response shown; reformats nothing
    /// by itself. An indent outside [`INDENT_RANGE`] is clamped into it.
    pub fn set_format_settings(&mut self, settings: FormatSettings) {
        self.format = FormatSettings {
            indent: settings
                .indent
                .clamp(*INDENT_RANGE.start(), *INDENT_RANGE.end()),
            ..settings
        };
    }

    /// Format XML for `key`'s open request, keeping `selection` (UTF-16) on the same text.
    /// `None` if the text is already formatted. A request that is not well-formed is refused;
    /// its error is already in the issues list.
    pub fn format_request(
        &self,
        key: ProjectKey,
        selection: Range<usize>,
    ) -> Result<Option<Reformat>, ModelError> {
        let editor = self
            .project(key)
            .ok_or(ModelError::UnknownProject)?
            .editor()
            .ok_or(ModelError::NoRequestSelected)?;
        let old = editor.text();
        let new = xml::pretty_print(old, self.format.indent)
            .map_err(|d| ModelError::NotWellFormed(d.message))?;
        if new == old {
            return Ok(None);
        }
        let (range, text) = utf16_edit(old, &new);
        let start = map_offset(old, &new, utf16_to_byte(old, selection.start));
        let end = map_offset(old, &new, utf16_to_byte(old, selection.end)).max(start);
        let selection = Utf16Cursor::new(&new).utf16_range(start..end);
        Ok(Some(Reformat {
            range,
            text,
            selection,
        }))
    }

    /// The projects whose open request Save All formats before saving: none unless format on
    /// save is on, and only requests with unsaved edits. The front end applies
    /// [`App::format_request`] to each, as for ⌃I; a request that is not well-formed is saved
    /// as written.
    pub fn format_on_save(&self) -> Vec<ProjectKey> {
        if !self.format.on_save {
            return Vec::new();
        }
        self.projects
            .iter()
            .filter(|(_, w)| w.editor().is_some_and(|e| e.dirty()))
            .map(|(key, _)| *key)
            .collect()
    }
}

/// Where byte `at` of `old` is in `new`. Formatting changes only whitespace, so the position
/// before the n-th non-whitespace character stays before it. Falls back to the same offset,
/// clamped, if the texts differ in more than whitespace.
fn map_offset(old: &str, new: &str, at: usize) -> usize {
    let is_ws = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    let solid = |s: &str| s.chars().filter(|c| !is_ws(*c)).collect::<String>();
    let at = at.min(old.len());
    if solid(old) != solid(new) {
        let mut at = at.min(new.len());
        while !new.is_char_boundary(at) {
            at -= 1;
        }
        return at;
    }
    let before = old[..at].chars().filter(|c| !is_ws(*c)).count();
    new.char_indices()
        .filter(|(_, c)| !is_ws(*c))
        .nth(before)
        .map_or(new.len(), |(i, _)| i)
}
