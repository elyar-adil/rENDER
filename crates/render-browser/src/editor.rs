//! UTF-8-safe single-line editing for the address field.

use std::collections::VecDeque;

const HISTORY_LIMIT: usize = 100;

/// Editing actions shared by keyboard shortcuts and the address context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
}

/// Minimal clipboard surface so editing can be tested without a desktop session.
pub trait Clipboard {
    fn read_text(&mut self) -> Option<String>;
    fn write_text(&mut self, value: &str) -> bool;
}

/// Lazily initialized native clipboard adapter. Construction remains headless-safe.
#[derive(Default)]
pub struct NativeClipboard {
    backend: Option<arboard::Clipboard>,
    initialization_attempted: bool,
}

impl NativeClipboard {
    fn backend(&mut self) -> Option<&mut arboard::Clipboard> {
        if !self.initialization_attempted {
            self.initialization_attempted = true;
            self.backend = arboard::Clipboard::new().ok();
        }
        self.backend.as_mut()
    }
}

impl Clipboard for NativeClipboard {
    fn read_text(&mut self) -> Option<String> {
        self.backend()?.get_text().ok()
    }

    fn write_text(&mut self, value: &str) -> bool {
        self.backend()
            .is_some_and(|backend| backend.set_text(value.to_owned()).is_ok())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditSnapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressEditor {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    focused: bool,
    preedit: String,
    undo: VecDeque<EditSnapshot>,
    redo: VecDeque<EditSnapshot>,
}

impl AddressEditor {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    #[must_use]
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    #[must_use]
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| ordered(anchor, self.cursor))
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(start, end)| &self.text[start..end])
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub fn command_is_enabled(&self, command: AddressCommand, paste_available: bool) -> bool {
        match command {
            AddressCommand::Undo => self.can_undo(),
            AddressCommand::Redo => self.can_redo(),
            AddressCommand::Cut | AddressCommand::Copy | AddressCommand::Delete => {
                self.selection().is_some()
            }
            AddressCommand::Paste => paste_available,
            AddressCommand::SelectAll => !self.text.is_empty(),
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.anchor = None;
        self.preedit.clear();
        self.undo.clear();
        self.redo.clear();
    }

    pub const fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.anchor = None;
        }
    }

    pub fn set_preedit(&mut self, preedit: impl Into<String>) {
        self.preedit = preedit.into();
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Places the caret at a byte offset, clamped down to a UTF-8 boundary.
    /// When `selecting` is true, the existing caret becomes the selection anchor.
    pub fn place_cursor(&mut self, position: usize, selecting: bool) {
        let position = clamp_boundary(&self.text, position);
        self.begin_selection(selecting);
        self.cursor = position;
        self.end_selection(selecting);
    }

    /// Starts mouse selection while retaining a zero-width anchor for a later drag.
    pub fn begin_pointer_selection(&mut self, position: usize, extend: bool) {
        let position = clamp_boundary(&self.text, position);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = Some(position);
        }
        self.cursor = position;
        self.preedit.clear();
    }

    /// Extends a selection started by [`Self::begin_pointer_selection`].
    pub fn extend_pointer_selection(&mut self, position: usize) {
        let position = clamp_boundary(&self.text, position);
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = position;
        self.preedit.clear();
    }

    pub fn finish_pointer_selection(&mut self) {
        if self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
    }

    /// Selects the run of word, whitespace, or punctuation characters at a caret offset.
    pub fn select_word_at(&mut self, position: usize) {
        if self.text.is_empty() {
            self.cursor = 0;
            self.anchor = None;
            return;
        }
        let position = clamp_boundary(&self.text, position);
        let character_start = if position == self.text.len() {
            previous_boundary(&self.text, position)
        } else {
            position
        };
        let Some(class) = self.text[character_start..]
            .chars()
            .next()
            .map(character_class)
        else {
            return;
        };

        let mut start = character_start;
        while start > 0 {
            let previous = previous_boundary(&self.text, start);
            let Some(character) = self.text[previous..start].chars().next() else {
                break;
            };
            if character_class(character) != class {
                break;
            }
            start = previous;
        }
        let mut end = next_boundary(&self.text, character_start);
        while end < self.text.len() {
            let Some(character) = self.text[end..].chars().next() else {
                break;
            };
            if character_class(character) != class {
                break;
            }
            end = next_boundary(&self.text, end);
        }
        self.anchor = Some(start);
        self.cursor = end;
        self.preedit.clear();
    }

    pub fn insert(&mut self, value: &str) {
        let filtered: String = value
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        if filtered.is_empty() {
            return;
        }
        let before = self.snapshot();
        self.delete_selection_raw();
        self.text.insert_str(self.cursor, &filtered);
        self.cursor += filtered.len();
        self.preedit.clear();
        self.record_edit(before);
    }

    pub fn backspace(&mut self) {
        if self.selection().is_none() && self.cursor == 0 {
            return;
        }
        let before = self.snapshot();
        if !self.delete_selection_raw() {
            let previous = previous_boundary(&self.text, self.cursor);
            self.text.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
        self.preedit.clear();
        self.record_edit(before);
    }

    pub fn delete(&mut self) {
        if self.selection().is_none() && self.cursor == self.text.len() {
            return;
        }
        let before = self.snapshot();
        if !self.delete_selection_raw() {
            let next = next_boundary(&self.text, self.cursor);
            self.text.replace_range(self.cursor..next, "");
        }
        self.preedit.clear();
        self.record_edit(before);
    }

    pub fn move_left(&mut self, selecting: bool) {
        if !selecting && let Some((start, _)) = self.selection() {
            self.cursor = start;
            self.anchor = None;
            return;
        }
        self.begin_selection(selecting);
        self.cursor = previous_boundary(&self.text, self.cursor);
        self.end_selection(selecting);
    }

    pub fn move_right(&mut self, selecting: bool) {
        if !selecting && let Some((_, end)) = self.selection() {
            self.cursor = end;
            self.anchor = None;
            return;
        }
        self.begin_selection(selecting);
        self.cursor = next_boundary(&self.text, self.cursor);
        self.end_selection(selecting);
    }

    pub fn move_home(&mut self, selecting: bool) {
        self.begin_selection(selecting);
        self.cursor = 0;
        self.end_selection(selecting);
    }

    pub fn move_end(&mut self, selecting: bool) {
        self.begin_selection(selecting);
        self.cursor = self.text.len();
        self.end_selection(selecting);
    }

    /// Executes one shared edit command. Returns whether it changed editor or clipboard state.
    pub fn execute(&mut self, command: AddressCommand, clipboard: &mut impl Clipboard) -> bool {
        match command {
            AddressCommand::Undo => self.undo(),
            AddressCommand::Redo => self.redo(),
            AddressCommand::Copy => self
                .selected_text()
                .is_some_and(|selection| clipboard.write_text(selection)),
            AddressCommand::Cut => {
                let Some(selection) = self.selected_text().map(str::to_owned) else {
                    return false;
                };
                if !clipboard.write_text(&selection) {
                    return false;
                }
                let before = self.snapshot();
                self.delete_selection_raw();
                self.record_edit(before);
                true
            }
            AddressCommand::Paste => {
                let Some(value) = clipboard.read_text() else {
                    return false;
                };
                let before = self.snapshot();
                self.insert(&value);
                self.snapshot() != before
            }
            AddressCommand::Delete => {
                if self.selection().is_none() {
                    return false;
                }
                self.delete();
                true
            }
            AddressCommand::SelectAll => {
                if self.text.is_empty() {
                    return false;
                }
                self.select_all();
                true
            }
        }
    }

    fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        let current = self.snapshot();
        push_bounded(&mut self.redo, current);
        self.restore(previous);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        let current = self.snapshot();
        push_bounded(&mut self.undo, current);
        self.restore(next);
        true
    }

    fn begin_selection(&mut self, selecting: bool) {
        if selecting && self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
    }

    fn end_selection(&mut self, selecting: bool) {
        if !selecting || self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
    }

    fn delete_selection_raw(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.anchor = None;
        true
    }

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    fn restore(&mut self, snapshot: EditSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.anchor = snapshot.anchor;
        self.preedit.clear();
    }

    fn record_edit(&mut self, before: EditSnapshot) {
        if self.snapshot() != before {
            push_bounded(&mut self.undo, before);
            self.redo.clear();
        }
    }
}

fn push_bounded(history: &mut VecDeque<EditSnapshot>, snapshot: EditSnapshot) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(snapshot);
}

fn clamp_boundary(text: &str, position: usize) -> usize {
    let mut position = position.min(text.len());
    while !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

fn previous_boundary(text: &str, position: usize) -> usize {
    text[..position]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, position: usize) -> usize {
    text[position..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(offset, _)| position + offset)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterClass {
    Word,
    Whitespace,
    Punctuation,
}

fn character_class(character: char) -> CharacterClass {
    if character.is_alphanumeric() || character == '_' {
        CharacterClass::Word
    } else if character.is_whitespace() {
        CharacterClass::Whitespace
    } else {
        CharacterClass::Punctuation
    }
}

const fn ordered(first: usize, second: usize) -> (usize, usize) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
mod tests {
    use super::{AddressCommand, AddressEditor, Clipboard, HISTORY_LIMIT};

    #[derive(Default)]
    struct MemoryClipboard(Option<String>);

    impl Clipboard for MemoryClipboard {
        fn read_text(&mut self) -> Option<String> {
            self.0.clone()
        }

        fn write_text(&mut self, value: &str) -> bool {
            self.0 = Some(value.to_owned());
            true
        }
    }

    #[test]
    fn editing_preserves_utf8_boundaries() {
        let mut editor = AddressEditor::new("a中🙂");
        editor.move_left(false);
        editor.backspace();
        assert_eq!(editor.text(), "a🙂");
        editor.delete();
        assert_eq!(editor.text(), "a");
    }

    #[test]
    fn selection_is_replaced_by_committed_text() {
        let mut editor = AddressEditor::new("example.test");
        editor.select_all();
        editor.insert("rust.test");
        assert_eq!(editor.text(), "rust.test");
        assert_eq!(editor.selection(), None);
    }

    #[test]
    fn shift_and_pointer_selection_share_anchor_semantics() {
        let mut editor = AddressEditor::new("abc中");
        editor.move_left(true);
        editor.move_left(true);
        assert_eq!(editor.selection(), Some((2, 6)));
        editor.begin_pointer_selection(1, true);
        assert_eq!(editor.selection(), Some((1, 6)));
        editor.extend_pointer_selection(usize::MAX);
        assert_eq!(editor.selection(), None);
        editor.finish_pointer_selection();
    }

    #[test]
    fn pointer_offsets_are_clamped_to_utf8_boundaries() {
        let mut editor = AddressEditor::new("中a");
        editor.begin_pointer_selection(2, false);
        assert_eq!(editor.cursor(), 0);
        editor.extend_pointer_selection(usize::MAX);
        assert_eq!(editor.selection(), Some((0, 4)));
    }

    #[test]
    fn double_click_word_selection_handles_ascii_cjk_and_punctuation() {
        let mut editor = AddressEditor::new("alpha 中日/path");
        editor.select_word_at(2);
        assert_eq!(editor.selected_text(), Some("alpha"));
        editor.select_word_at(7);
        assert_eq!(editor.selected_text(), Some("中日"));
        editor.select_word_at(12);
        assert_eq!(editor.selected_text(), Some("/"));
    }

    #[test]
    fn clipboard_commands_preserve_utf8_selection_and_are_undoable() {
        let mut editor = AddressEditor::new("a中🙂z");
        let mut clipboard = MemoryClipboard::default();
        editor.begin_pointer_selection(1, false);
        editor.extend_pointer_selection(8);
        assert!(editor.execute(AddressCommand::Cut, &mut clipboard));
        assert_eq!(clipboard.0.as_deref(), Some("中🙂"));
        assert_eq!(editor.text(), "az");
        assert!(editor.execute(AddressCommand::Undo, &mut clipboard));
        assert_eq!(editor.text(), "a中🙂z");
        assert_eq!(editor.selection(), Some((1, 8)));
        assert!(editor.execute(AddressCommand::Redo, &mut clipboard));
        assert_eq!(editor.text(), "az");
        editor.place_cursor(1, false);
        assert!(editor.execute(AddressCommand::Paste, &mut clipboard));
        assert_eq!(editor.text(), "a中🙂z");
    }

    #[test]
    fn new_edit_clears_redo_and_history_is_bounded() {
        let mut editor = AddressEditor::new("");
        let mut clipboard = MemoryClipboard::default();
        for _ in 0..HISTORY_LIMIT + 8 {
            editor.insert("x");
        }
        for _ in 0..HISTORY_LIMIT {
            assert!(editor.execute(AddressCommand::Undo, &mut clipboard));
        }
        assert!(!editor.execute(AddressCommand::Undo, &mut clipboard));
        assert_eq!(editor.text(), "xxxxxxxx");
        assert!(editor.execute(AddressCommand::Redo, &mut clipboard));
        editor.insert("y");
        assert!(!editor.can_redo());
    }

    #[test]
    fn control_only_paste_does_not_delete_selection_or_create_history() {
        let mut editor = AddressEditor::new("keep");
        editor.select_all();
        let mut clipboard = MemoryClipboard(Some("\n\r\t".to_owned()));
        assert!(!editor.execute(AddressCommand::Paste, &mut clipboard));
        assert_eq!(editor.text(), "keep");
        assert_eq!(editor.selection(), Some((0, 4)));
        assert!(!editor.can_undo());
    }
}
