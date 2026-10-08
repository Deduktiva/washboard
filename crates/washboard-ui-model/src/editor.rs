//! The request editor's buffer and autosave (PLAN §4 "Save / autosave"). The text widget owns
//! the visible text and undo; it reports each edit here, and the buffer is what gets saved,
//! highlighted and (later) validated.

use std::ops::Range;
use std::time::Duration;

use washboard_core::model::RequestId;
use washboard_core::project::Project;
use washboard_core::xml::utf16::{Utf16Cursor, utf16_len, utf16_to_byte};
use washboard_core::xml::{TokenBuffer, TokenKind};

use crate::app::{App, ModelError, ProjectKey};
use crate::event::Event;
use crate::timers::TimerKind;

/// Autosave runs this long after the last edit.
pub(crate) const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// The selected request's text as edited, not necessarily as saved.
#[derive(Debug)]
pub struct Editor {
    request: RequestId,
    text: String,
    tokens: TokenBuffer,
    version: u64,
    dirty: bool,
}

impl Editor {
    pub(crate) fn load(project: &Project, request: RequestId) -> Result<Editor, ModelError> {
        let text = project.read_request(request)?;
        Ok(Editor {
            request,
            tokens: TokenBuffer::new(&text),
            text,
            version: 0,
            dirty: false,
        })
    }

    pub fn request(&self) -> RequestId {
        self.request
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Bumped by every edit, so results computed from an older text can be recognized.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Edited since the last save.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Length in UTF-16 units, as the text widget counts.
    pub fn utf16_len(&self) -> usize {
        utf16_len(&self.text)
    }

    /// Tokens overlapping the UTF-16 range `range`, with UTF-16 spans, for colouring.
    pub fn tokens_utf16(&self, range: Range<usize>) -> Vec<(Range<usize>, TokenKind)> {
        let mut cursor = Utf16Cursor::new(&self.text);
        let start = cursor.byte_at(range.start);
        let end = cursor.byte_at(range.end.max(range.start));
        let mut cursor = Utf16Cursor::new(&self.text);
        self.tokens
            .tokens_in(start..end)
            .iter()
            .map(|t| (cursor.utf16_range(t.span()), t.kind))
            .collect()
    }

    /// Replaces the UTF-16 range `range` with `new`; returns the UTF-16 range of the new text
    /// whose tokens changed. Out-of-range input is clamped.
    fn replace(&mut self, range: Range<usize>, new: &str) -> Range<usize> {
        let start = utf16_to_byte(&self.text, range.start);
        let end = utf16_to_byte(&self.text, range.end).max(start);
        self.text.replace_range(start..end, new);
        let changed = self.tokens.edit(&self.text, start..end, new.len());
        self.version += 1;
        Utf16Cursor::new(&self.text).utf16_range(changed)
    }
}

impl App {
    /// The text widget changed `range` (UTF-16, in the text before the change) to `text`.
    /// Starts or restarts the autosave timer.
    pub fn edit(
        &mut self,
        key: ProjectKey,
        range: Range<usize>,
        text: &str,
    ) -> Result<(), ModelError> {
        let window = self.window_mut(key).ok_or(ModelError::UnknownProject)?;
        let editor = window
            .editor
            .as_mut()
            .ok_or(ModelError::NoRequestSelected)?;
        let changed = editor.replace(range, text);
        let became_dirty = !editor.dirty;
        editor.dirty = true;
        let request = editor.request;
        if became_dirty {
            window.mark_dirty(request, true);
        }
        self.events.push(Event::TokensChanged {
            project: key,
            range: changed,
        });
        if became_dirty {
            self.events.push(Event::SidebarChanged { project: key });
            self.events.push(Event::EditedChanged { project: key });
        }
        self.restart_timer(key, TimerKind::Autosave, AUTOSAVE_DELAY);
        Ok(())
    }

    /// ⌘S: saves every edited request in every project. Failures are alerted; the buffers
    /// stay dirty so nothing is lost.
    pub fn save_all(&mut self) -> bool {
        let keys: Vec<_> = self.projects.iter().map(|(k, _)| *k).collect();
        let mut ok = true;
        for key in keys {
            ok &= self.flush_or_alert(key);
        }
        ok
    }

    /// The project's window stopped being key.
    pub fn window_resigned_key(&mut self, key: ProjectKey) {
        self.flush_or_alert(key);
    }

    /// The app was deactivated.
    pub fn app_deactivated(&mut self) {
        self.save_all();
    }

    pub(crate) fn flush_or_alert(&mut self, key: ProjectKey) -> bool {
        match self.flush(key) {
            Ok(()) | Err(ModelError::UnknownProject) => true,
            Err(e) => {
                self.alert_error("Could not save the request", &e);
                false
            }
        }
    }

    /// Saves the project's editor if it is dirty.
    pub(crate) fn flush(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let window = self.window_mut(key).ok_or(ModelError::UnknownProject)?;
        let saved = match &mut window.editor {
            Some(editor) if editor.dirty => {
                window.project.write_request(editor.request, &editor.text)?;
                editor.dirty = false;
                Some(editor.request)
            }
            _ => None,
        };
        if let Some(request) = saved {
            window.mark_dirty(request, false);
            self.events.push(Event::SidebarChanged { project: key });
            self.events.push(Event::EditedChanged { project: key });
        }
        self.stop_timer(key, TimerKind::Autosave);
        Ok(())
    }

    /// Replaces the editor with the selected request's text, or none. Unsaved edits must have
    /// been flushed (or deliberately dropped) before. The caller announces the change, if the
    /// window already exists.
    pub(crate) fn load_editor(&mut self, key: ProjectKey) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let loaded = window
            .selected_request()
            .map(|id| Editor::load(&window.project, id));
        window.editor = None;
        let error = match loaded {
            Some(Ok(editor)) => {
                window.editor = Some(editor);
                None
            }
            Some(Err(e)) => Some(e),
            None => None,
        };
        self.stop_timer(key, TimerKind::Autosave);
        if let Some(e) = error {
            self.alert_error("Could not open the request", &e);
        }
    }
}
