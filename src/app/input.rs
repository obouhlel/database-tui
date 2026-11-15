//! Single-line text input (filters, command line, password prompt).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Cursor position in characters.
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    Changed,
    Moved,
    Submit,
    Cancel,
    Ignored,
}

impl LineInput {
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Display width of the text before the cursor.
    pub fn cursor_width(&self) -> usize {
        self.text[..self.byte_idx(self.cursor)].width()
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.insert(c);
        }
    }

    fn insert(&mut self, c: char) {
        let i = self.byte_idx(self.cursor);
        self.text.insert(i, c);
        self.cursor += 1;
    }

    fn byte_idx(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn remove_range(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte_idx(from), self.byte_idx(to));
        self.text.replace_range(a..b, "");
        self.cursor = from;
    }

    /// Start of the word before the cursor (for ctrl+w).
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    pub fn handle_key(&mut self, ev: &KeyEvent) -> InputOutcome {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Enter => InputOutcome::Submit,
            KeyCode::Esc => InputOutcome::Cancel,
            KeyCode::Char('c') if ctrl => InputOutcome::Cancel,
            KeyCode::Char('a') if ctrl => self.move_to(0),
            KeyCode::Char('e') if ctrl => self.move_to(self.len()),
            KeyCode::Char('u') if ctrl => {
                self.remove_range(0, self.cursor);
                InputOutcome::Changed
            }
            KeyCode::Char('k') if ctrl => {
                let len = self.len();
                self.remove_range(self.cursor, len);
                InputOutcome::Changed
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                self.remove_range(start, self.cursor);
                InputOutcome::Changed
            }
            KeyCode::Char(c) if !ctrl => {
                self.insert(c);
                InputOutcome::Changed
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.remove_range(self.cursor - 1, self.cursor);
                InputOutcome::Changed
            }
            KeyCode::Delete if self.cursor < self.len() => {
                self.remove_range(self.cursor, self.cursor + 1);
                self.cursor = self.cursor.min(self.len());
                InputOutcome::Changed
            }
            KeyCode::Left if self.cursor > 0 => self.move_to(self.cursor - 1),
            KeyCode::Right if self.cursor < self.len() => self.move_to(self.cursor + 1),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.len()),
            _ => InputOutcome::Ignored,
        }
    }

    fn move_to(&mut self, pos: usize) -> InputOutcome {
        self.cursor = pos;
        InputOutcome::Moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(s: &str) -> LineInput {
        let mut i = LineInput::default();
        for c in s.chars() {
            i.handle_key(&key(KeyCode::Char(c)));
        }
        i
    }

    #[test]
    fn typing_and_editing() {
        let mut i = typed("héllo");
        assert_eq!(i.text(), "héllo");
        i.handle_key(&key(KeyCode::Left));
        i.handle_key(&key(KeyCode::Backspace));
        assert_eq!(i.text(), "hélo");
        i.handle_key(&key(KeyCode::Home));
        i.handle_key(&key(KeyCode::Delete));
        assert_eq!(i.text(), "élo");
        assert_eq!(i.handle_key(&key(KeyCode::Enter)), InputOutcome::Submit);
    }

    #[test]
    fn ctrl_shortcuts() {
        let mut i = typed("select foo bar");
        i.handle_key(&ctrl('w'));
        assert_eq!(i.text(), "select foo ");
        i.handle_key(&ctrl('w'));
        assert_eq!(i.text(), "select ");
        i.handle_key(&ctrl('u'));
        assert!(i.is_empty());
    }

    #[test]
    fn wide_chars_cursor_width() {
        let i = typed("日本");
        assert_eq!(i.cursor_width(), 4);
    }

    #[test]
    fn paste_strips_control_chars() {
        let mut i = LineInput::default();
        i.insert_str("a\nb\tc");
        assert_eq!(i.text(), "abc");
    }
}
