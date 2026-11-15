//! Minimal multi-line text editor (long cell values, JSON, generated SQL).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaOutcome {
    Changed,
    Moved,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea {
    lines: Vec<String>,
    /// Cursor line and character index in that line.
    pub row: usize,
    pub col: usize,
    /// First visible line, kept up to date by the renderer.
    pub scroll: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextArea {
    pub fn new(text: &str) -> Self {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        Self {
            lines,
            row: 0,
            col: 0,
            scroll: 0,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte(&self, row: usize, col: usize) -> usize {
        self.lines[row]
            .char_indices()
            .nth(col)
            .map_or(self.lines[row].len(), |(i, _)| i)
    }

    /// Display column of the cursor in its line.
    pub fn cursor_width(&self) -> usize {
        self.lines[self.row][..self.byte(self.row, self.col)].width()
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            match c {
                '\n' => self.newline(),
                '\r' => {}
                c => self.insert(c),
            }
        }
    }

    fn insert(&mut self, c: char) {
        let b = self.byte(self.row, self.col);
        self.lines[self.row].insert(b, c);
        self.col += 1;
    }

    fn newline(&mut self) {
        let b = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(b);
        // Keep the indentation of the current line.
        let indent: String = self.lines[self.row]
            .chars()
            .take_while(|c| *c == ' ')
            .collect();
        self.row += 1;
        self.col = indent.chars().count();
        self.lines.insert(self.row, indent + &rest);
    }

    fn clamp_col(&mut self) {
        self.col = self.col.min(self.line_len(self.row));
    }

    /// Keeps the cursor in a `height`-line window.
    pub fn scroll_into_view(&mut self, height: usize) {
        let h = height.max(1);
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + h {
            self.scroll = self.row + 1 - h;
        }
    }

    pub fn handle_key(&mut self, ev: &KeyEvent) -> AreaOutcome {
        use AreaOutcome::*;
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Char(c) if !ctrl => {
                self.insert(c);
                Changed
            }
            KeyCode::Tab => {
                self.insert_str("  ");
                Changed
            }
            KeyCode::Enter => {
                self.newline();
                Changed
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    let b = self.byte(self.row, self.col - 1);
                    self.lines[self.row].remove(b);
                    self.col -= 1;
                    Changed
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                    self.lines[self.row].push_str(&line);
                    Changed
                } else {
                    Ignored
                }
            }
            KeyCode::Delete => {
                if self.col < self.line_len(self.row) {
                    let b = self.byte(self.row, self.col);
                    self.lines[self.row].remove(b);
                    Changed
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                    Changed
                } else {
                    Ignored
                }
            }
            KeyCode::Char('u') if ctrl => {
                let b = self.byte(self.row, self.col);
                self.lines[self.row].replace_range(..b, "");
                self.col = 0;
                Changed
            }
            KeyCode::Char('k') if ctrl => {
                let b = self.byte(self.row, self.col);
                self.lines[self.row].truncate(b);
                Changed
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
                Moved
            }
            KeyCode::Right => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
                Moved
            }
            KeyCode::Up if self.row > 0 => {
                self.row -= 1;
                self.clamp_col();
                Moved
            }
            KeyCode::Down if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.clamp_col();
                Moved
            }
            KeyCode::PageUp => {
                self.row = self.row.saturating_sub(10);
                self.clamp_col();
                Moved
            }
            KeyCode::PageDown => {
                self.row = (self.row + 10).min(self.lines.len() - 1);
                self.clamp_col();
                Moved
            }
            KeyCode::Home => {
                self.col = 0;
                Moved
            }
            KeyCode::Char('a') if ctrl => {
                self.col = 0;
                Moved
            }
            KeyCode::End => {
                self.col = self.line_len(self.row);
                Moved
            }
            KeyCode::Char('e') if ctrl => {
                self.col = self.line_len(self.row);
                Moved
            }
            _ => Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_newlines_and_joins() {
        let mut t = TextArea::new("{\n  \"a\": 1");
        t.row = 1;
        t.col = 9;
        t.handle_key(&key(KeyCode::Char(',')));
        t.handle_key(&key(KeyCode::Enter));
        t.insert_str("\"é\": 2");
        assert_eq!(t.text(), "{\n  \"a\": 1,\n  \"é\": 2");
        assert_eq!((t.row, t.col), (2, 8));
        t.handle_key(&key(KeyCode::Home));
        t.handle_key(&key(KeyCode::Backspace));
        assert_eq!(t.text(), "{\n  \"a\": 1,  \"é\": 2");
        t.handle_key(&key(KeyCode::End));
        t.handle_key(&key(KeyCode::Delete));
        assert_eq!(t.lines().len(), 2);
    }

    #[test]
    fn vertical_moves_clamp_the_column() {
        let mut t = TextArea::new("long line\nab");
        t.col = 8;
        t.handle_key(&key(KeyCode::Down));
        assert_eq!((t.row, t.col), (1, 2));
        t.scroll_into_view(1);
        assert_eq!(t.scroll, 1);
    }
}
