//! The SQL editor buffer: multi-line text, cursor, selection, undo / redo,
//! auto-indent, and two key sets chosen in `[editor] mode`:
//!
//! - `classic`: always inserting; Shift+arrows select, Ctrl+z / Ctrl+y undo
//!   / redo, Ctrl+x / Ctrl+v cut / paste, Ctrl+a select all;
//! - `vim`: normal / insert / visual modes with the usual motions
//!   (`h j k l w b e 0 ^ $ gg G`), operators (`d c y` + motion, `dd cc yy`,
//!   `D C x X J p P >> <<`) and `u` / `Ctrl+r`.
//!
//! Positions are (line, character index); byte offsets are only computed
//! for the SQL helpers (statement under the cursor, error positions).

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use crate::config::EditorMode;

const MAX_UNDO: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Changed,
    Moved,
    /// Text copied (yank / cut): also goes to the system clipboard.
    Copied(String),
    /// Not an editor key: the application may handle it.
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
}

/// Kind of the last change, to group typing into one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Typing,
    Other,
}

#[derive(Debug, Clone)]
pub struct Editor {
    lines: Vec<String>,
    pub cursor: Pos,
    /// Selection anchor (the selection spans anchor..cursor).
    pub anchor: Option<Pos>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_group: Option<Group>,
    /// Internal register (yank / cut / paste); `linewise` for `yy` / `dd`.
    register: String,
    linewise: bool,
    pub mode: EditorMode,
    pub vim: VimMode,
    /// Pending vim operator or prefix (`d`, `c`, `y`, `g`, `>`, `<`).
    pending: Option<char>,
    pub tab_width: usize,
    /// First visible line and column, kept up to date by the renderer.
    pub scroll: usize,
    pub hscroll: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(EditorMode::Classic, 4)
    }
}

impl Editor {
    pub fn new(mode: EditorMode, tab_width: usize) -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Pos::default(),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_group: None,
            register: String::new(),
            linewise: false,
            mode,
            vim: match mode {
                EditorMode::Vim => VimMode::Normal,
                EditorMode::Classic => VimMode::Insert,
            },
            pending: None,
            tab_width: tab_width.clamp(1, 16),
            scroll: 0,
            hscroll: 0,
        }
    }

    // ------------------------------------------------------------- access

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Replaces the whole text (undoable), cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.checkpoint(Group::Other);
        self.lines = text.split('\n').map(str::to_string).collect();
        self.cursor = self.end_pos();
        self.anchor = None;
    }

    /// Whether keys insert text (classic, or vim insert mode).
    pub fn inserting(&self) -> bool {
        self.mode == EditorMode::Classic || self.vim == VimMode::Insert
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte_in_line(&self, row: usize, col: usize) -> usize {
        self.lines[row]
            .char_indices()
            .nth(col)
            .map_or(self.lines[row].len(), |(i, _)| i)
    }

    /// Byte offset of a position in [`Self::text`].
    pub fn offset(&self, p: Pos) -> usize {
        self.lines[..p.row]
            .iter()
            .map(|l| l.len() + 1)
            .sum::<usize>()
            + self.byte_in_line(p.row, p.col)
    }

    /// Position of a byte offset in [`Self::text`].
    pub fn pos_of(&self, offset: usize) -> Pos {
        let mut rest = offset;
        for (row, line) in self.lines.iter().enumerate() {
            if rest <= line.len() {
                let col = line[..rest.min(line.len())].chars().count();
                return Pos { row, col };
            }
            rest -= line.len() + 1;
        }
        self.end_pos()
    }

    fn end_pos(&self) -> Pos {
        let row = self.lines.len() - 1;
        Pos {
            row,
            col: self.line_len(row),
        }
    }

    /// Byte range of the selection (visual line mode: whole lines).
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        let (mut s, mut e) = if a <= self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        };
        if self.mode == EditorMode::Vim {
            match self.vim {
                VimMode::VisualLine => {
                    s.col = 0;
                    e.col = self.line_len(e.row);
                }
                // Vim's visual selection includes the cursor character.
                VimMode::Visual => e.col = (e.col + 1).min(self.line_len(e.row)),
                _ => {}
            }
        }
        let (s, e) = (self.offset(s), self.offset(e));
        (s < e).then_some(s..e)
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|r| self.text()[r].to_string())
    }

    /// Display column of the cursor in its line.
    pub fn cursor_width(&self) -> usize {
        let p = self.cursor;
        self.lines[p.row][..self.byte_in_line(p.row, p.col)].width()
    }

    // --------------------------------------------------------------- undo

    fn checkpoint(&mut self, group: Group) {
        if group == Group::Typing && self.last_group == Some(Group::Typing) {
            return;
        }
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_group = Some(group);
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        let current = Snapshot {
            lines: std::mem::replace(&mut self.lines, s.lines),
            cursor: self.cursor,
        };
        self.redo.push(current);
        self.cursor = s.cursor;
        self.clamp();
        self.anchor = None;
        self.last_group = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        let current = Snapshot {
            lines: std::mem::replace(&mut self.lines, s.lines),
            cursor: self.cursor,
        };
        self.undo.push(current);
        self.cursor = s.cursor;
        self.clamp();
        self.anchor = None;
        self.last_group = None;
        true
    }

    fn clamp(&mut self) {
        self.cursor.row = self.cursor.row.min(self.lines.len() - 1);
        self.cursor.col = self.cursor.col.min(self.line_len(self.cursor.row));
    }

    // ------------------------------------------------------------ editing

    fn insert_char(&mut self, c: char) {
        let b = self.byte_in_line(self.cursor.row, self.cursor.col);
        self.lines[self.cursor.row].insert(b, c);
        self.cursor.col += 1;
    }

    /// Inserts text at the cursor (replacing the selection), as one undo step.
    pub fn insert_str(&mut self, s: &str) {
        self.checkpoint(Group::Other);
        self.delete_selection();
        for c in s.chars() {
            match c {
                '\n' => self.split_line(false),
                '\r' => {}
                '\t' => {
                    for _ in 0..self.tab_width {
                        self.insert_char(' ');
                    }
                }
                c => self.insert_char(c),
            }
        }
        self.last_group = None;
    }

    /// New line at the cursor; `indent` keeps the current indentation
    /// (plus one level after an opening parenthesis).
    fn split_line(&mut self, indent: bool) {
        let b = self.byte_in_line(self.cursor.row, self.cursor.col);
        let rest = self.lines[self.cursor.row].split_off(b);
        let mut prefix = String::new();
        if indent {
            let line = &self.lines[self.cursor.row];
            prefix = line.chars().take_while(|c| *c == ' ').collect();
            if line.trim_end().ends_with('(') {
                prefix.push_str(&" ".repeat(self.tab_width));
            }
        }
        let rest = if indent {
            rest.trim_start().to_string()
        } else {
            rest
        };
        self.cursor.row += 1;
        self.cursor.col = prefix.chars().count();
        self.lines.insert(self.cursor.row, prefix + &rest);
    }

    fn delete_range(&mut self, r: Range<usize>) -> String {
        let text = self.text();
        let removed = text[r.clone()].to_string();
        let mut new = String::with_capacity(text.len());
        new.push_str(&text[..r.start]);
        new.push_str(&text[r.end..]);
        self.lines = new.split('\n').map(str::to_string).collect();
        self.cursor = self.pos_of(r.start);
        removed
    }

    /// Deletes the selection; returns the removed text.
    fn delete_selection(&mut self) -> Option<String> {
        let r = self.selection()?;
        self.anchor = None;
        Some(self.delete_range(r))
    }

    fn backspace(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }
        let p = self.cursor;
        if p.col > 0 {
            // Remove a whole indentation step in leading spaces.
            let line = &self.lines[p.row];
            let before: String = line.chars().take(p.col).collect();
            let n = if before.chars().all(|c| c == ' ') {
                let rem = p.col % self.tab_width;
                if rem == 0 { self.tab_width } else { rem }
            } else {
                1
            };
            let n = n.min(p.col);
            let start = self.offset(Pos {
                row: p.row,
                col: p.col - n,
            });
            let end = self.offset(p);
            self.delete_range(start..end);
        } else if p.row > 0 {
            let start = self.offset(p) - 1;
            self.delete_range(start..start + 1);
        }
    }

    fn delete_forward(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }
        let o = self.offset(self.cursor);
        let text_len = self.text().len();
        if o < text_len {
            let next = self.text()[o..].chars().next().map_or(1, char::len_utf8);
            self.delete_range(o..o + next);
        }
    }

    fn indent_lines(&mut self, rows: Range<usize>, out: bool) {
        let w = self.tab_width;
        for row in rows {
            if out {
                let n = self.lines[row]
                    .chars()
                    .take_while(|c| *c == ' ')
                    .count()
                    .min(w);
                self.lines[row].drain(..n);
            } else if !self.lines[row].is_empty() {
                self.lines[row].insert_str(0, &" ".repeat(w));
            }
        }
        self.clamp();
    }

    fn selected_rows(&self) -> Range<usize> {
        match self.anchor {
            Some(a) => a.row.min(self.cursor.row)..a.row.max(self.cursor.row) + 1,
            None => self.cursor.row..self.cursor.row + 1,
        }
    }

    // ------------------------------------------------------------ motions

    fn char_at(&self, p: Pos) -> Option<char> {
        self.lines[p.row].chars().nth(p.col)
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Start of the next word (vim `w`), across lines.
    fn next_word(&self, p: Pos) -> Pos {
        let mut p = p;
        let class = |c: Option<char>| match c {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if Self::is_word(c) => 1,
            Some(_) => 2,
        };
        let start = class(self.char_at(p));
        // Skip the current word.
        while start != 0 && p.col < self.line_len(p.row) && class(self.char_at(p)) == start {
            p.col += 1;
        }
        // Skip blanks, possibly to the next lines.
        loop {
            if p.col >= self.line_len(p.row) {
                if p.row + 1 >= self.lines.len() {
                    return Pos {
                        row: p.row,
                        col: self.line_len(p.row),
                    };
                }
                p = Pos {
                    row: p.row + 1,
                    col: 0,
                };
                if self.line_len(p.row) == 0 {
                    return p;
                }
                continue;
            }
            if class(self.char_at(p)) != 0 {
                return p;
            }
            p.col += 1;
        }
    }

    /// Start of the previous word (vim `b`).
    fn prev_word(&self, p: Pos) -> Pos {
        let mut p = p;
        loop {
            if p.col == 0 {
                if p.row == 0 {
                    return p;
                }
                p = Pos {
                    row: p.row - 1,
                    col: self.line_len(p.row - 1),
                };
                if p.col == 0 {
                    return p;
                }
                continue;
            }
            let c = self.char_at(Pos {
                row: p.row,
                col: p.col - 1,
            });
            if c.is_some_and(|c| !c.is_whitespace()) {
                break;
            }
            p.col -= 1;
        }
        let word = self
            .char_at(Pos {
                row: p.row,
                col: p.col - 1,
            })
            .is_some_and(Self::is_word);
        while p.col > 0 {
            let c = self.char_at(Pos {
                row: p.row,
                col: p.col - 1,
            });
            match c {
                Some(c) if !c.is_whitespace() && Self::is_word(c) == word => p.col -= 1,
                _ => break,
            }
        }
        p
    }

    /// End of the current / next word (vim `e`).
    fn word_end(&self, p: Pos) -> Pos {
        let mut p = p;
        let len = self.line_len(p.row);
        if p.col + 1 >= len {
            if p.row + 1 < self.lines.len() {
                p = Pos {
                    row: p.row + 1,
                    col: 0,
                };
            } else {
                return p;
            }
        } else {
            p.col += 1;
        }
        while self.char_at(p).is_some_and(char::is_whitespace) {
            p.col += 1;
        }
        let word = self.char_at(p).is_some_and(Self::is_word);
        while p.col + 1 < self.line_len(p.row) {
            let c = self.char_at(Pos {
                row: p.row,
                col: p.col + 1,
            });
            match c {
                Some(c) if !c.is_whitespace() && Self::is_word(c) == word => p.col += 1,
                _ => break,
            }
        }
        p
    }

    fn first_non_blank(&self, row: usize) -> usize {
        self.lines[row]
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
    }

    fn move_to(&mut self, p: Pos, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else if self.mode == EditorMode::Classic {
            self.anchor = None;
        }
        self.cursor = p;
        self.clamp();
        self.last_group = None;
    }

    fn vertical(&self, delta: isize) -> Pos {
        let row = self
            .cursor
            .row
            .saturating_add_signed(delta)
            .min(self.lines.len() - 1);
        Pos {
            row,
            col: self.cursor.col.min(self.line_len(row)),
        }
    }

    // ------------------------------------------------------- classic keys

    pub fn handle_key(&mut self, key: &KeyEvent) -> EditOutcome {
        match self.mode {
            EditorMode::Classic => self.classic_key(key),
            EditorMode::Vim => match self.vim {
                VimMode::Insert => self.vim_insert_key(key),
                VimMode::Normal => self.vim_normal_key(key),
                VimMode::Visual | VimMode::VisualLine => self.vim_visual_key(key),
            },
        }
    }

    /// Keys shared by classic mode and vim insert mode.
    fn typing_key(&mut self, key: &KeyEvent, select: bool) -> EditOutcome {
        use EditOutcome::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => {
                if self.selection().is_some() {
                    self.checkpoint(Group::Other);
                    self.delete_selection();
                } else {
                    self.checkpoint(Group::Typing);
                }
                self.insert_char(c);
                if !Self::is_word(c) {
                    self.last_group = Some(Group::Other);
                }
                Changed
            }
            KeyCode::Enter if !ctrl && !alt => {
                self.checkpoint(Group::Other);
                self.delete_selection();
                self.split_line(true);
                Changed
            }
            KeyCode::Tab => {
                self.checkpoint(Group::Other);
                if self.anchor.is_some_and(|a| a.row != self.cursor.row) {
                    let rows = self.selected_rows();
                    self.indent_lines(rows, false);
                } else {
                    self.delete_selection();
                    let n = self.tab_width - self.cursor.col % self.tab_width;
                    for _ in 0..n {
                        self.insert_char(' ');
                    }
                }
                Changed
            }
            KeyCode::BackTab => {
                self.checkpoint(Group::Other);
                let rows = self.selected_rows();
                self.indent_lines(rows, true);
                Changed
            }
            KeyCode::Backspace => {
                self.checkpoint(Group::Other);
                self.backspace();
                Changed
            }
            KeyCode::Delete => {
                self.checkpoint(Group::Other);
                self.delete_forward();
                Changed
            }
            KeyCode::Char('w') if ctrl => {
                self.checkpoint(Group::Other);
                let start = self.prev_word(self.cursor);
                let (s, e) = (self.offset(start), self.offset(self.cursor));
                self.delete_range(s..e);
                Changed
            }
            KeyCode::Char('u') if ctrl => {
                self.checkpoint(Group::Other);
                let s = self.offset(Pos {
                    row: self.cursor.row,
                    col: 0,
                });
                let e = self.offset(self.cursor);
                self.delete_range(s..e);
                Changed
            }
            KeyCode::Left if ctrl => {
                let p = self.prev_word(self.cursor);
                self.move_to(p, select);
                Moved
            }
            KeyCode::Right if ctrl => {
                let p = self.next_word(self.cursor);
                self.move_to(p, select);
                Moved
            }
            KeyCode::Left => {
                let p = if self.cursor.col > 0 {
                    Pos {
                        col: self.cursor.col - 1,
                        ..self.cursor
                    }
                } else if self.cursor.row > 0 {
                    Pos {
                        row: self.cursor.row - 1,
                        col: self.line_len(self.cursor.row - 1),
                    }
                } else {
                    self.cursor
                };
                self.move_to(p, select);
                Moved
            }
            KeyCode::Right => {
                let p = if self.cursor.col < self.line_len(self.cursor.row) {
                    Pos {
                        col: self.cursor.col + 1,
                        ..self.cursor
                    }
                } else if self.cursor.row + 1 < self.lines.len() {
                    Pos {
                        row: self.cursor.row + 1,
                        col: 0,
                    }
                } else {
                    self.cursor
                };
                self.move_to(p, select);
                Moved
            }
            KeyCode::Up => {
                let p = self.vertical(-1);
                self.move_to(p, select);
                Moved
            }
            KeyCode::Down => {
                let p = self.vertical(1);
                self.move_to(p, select);
                Moved
            }
            KeyCode::PageUp => {
                let p = self.vertical(-20);
                self.move_to(p, select);
                Moved
            }
            KeyCode::PageDown => {
                let p = self.vertical(20);
                self.move_to(p, select);
                Moved
            }
            KeyCode::Home if ctrl => {
                self.move_to(Pos::default(), select);
                Moved
            }
            KeyCode::End if ctrl => {
                let p = self.end_pos();
                self.move_to(p, select);
                Moved
            }
            KeyCode::Home => {
                // Toggle between the first non-blank and column 0.
                let fnb = self.first_non_blank(self.cursor.row);
                let col = if self.cursor.col == fnb { 0 } else { fnb };
                self.move_to(Pos { col, ..self.cursor }, select);
                Moved
            }
            KeyCode::End => {
                let col = self.line_len(self.cursor.row);
                self.move_to(Pos { col, ..self.cursor }, select);
                Moved
            }
            _ => Ignored,
        }
    }

    fn classic_key(&mut self, key: &KeyEvent) -> EditOutcome {
        use EditOutcome::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('z') if ctrl => {
                self.undo();
                Changed
            }
            KeyCode::Char('y') if ctrl => {
                self.redo();
                Changed
            }
            KeyCode::Char('a') if ctrl => {
                self.anchor = Some(Pos::default());
                self.cursor = self.end_pos();
                Moved
            }
            KeyCode::Char('x') if ctrl => {
                // Cut the selection, or the whole line.
                self.checkpoint(Group::Other);
                let text = match self.delete_selection() {
                    Some(t) => t,
                    None => self.delete_line(),
                };
                self.register = text.clone();
                Copied(text)
            }
            KeyCode::Char('v') if ctrl => {
                let text = self.register.clone();
                self.insert_str(&text);
                Changed
            }
            KeyCode::Char('d') if ctrl => {
                // Duplicate the line.
                self.checkpoint(Group::Other);
                let line = self.lines[self.cursor.row].clone();
                self.lines.insert(self.cursor.row + 1, line);
                self.cursor.row += 1;
                Changed
            }
            KeyCode::Esc if self.anchor.is_some() => {
                self.anchor = None;
                Moved
            }
            _ => self.typing_key(key, shift),
        }
    }

    /// Copies the selection into the register (classic Ctrl+c).
    pub fn copy_selection(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.register = text.clone();
        self.linewise = false;
        Some(text)
    }

    fn delete_line(&mut self) -> String {
        let row = self.cursor.row;
        let removed = if self.lines.len() == 1 {
            std::mem::take(&mut self.lines[0])
        } else {
            self.lines.remove(row)
        };
        self.cursor.row = row.min(self.lines.len() - 1);
        self.cursor.col = self.first_non_blank(self.cursor.row);
        self.linewise = true;
        removed + "\n"
    }

    // ----------------------------------------------------------- vim keys

    fn vim_insert_key(&mut self, key: &KeyEvent) -> EditOutcome {
        if key.code == KeyCode::Esc {
            self.vim = VimMode::Normal;
            self.cursor.col = self.cursor.col.saturating_sub(1);
            self.last_group = None;
            return EditOutcome::Moved;
        }
        self.typing_key(key, false)
    }

    /// Target of a motion key, if `c` is one.
    fn motion(&self, code: KeyCode) -> Option<(Pos, bool)> {
        // (target, linewise)
        let p = self.cursor;
        Some(match code {
            KeyCode::Char('h') | KeyCode::Left => (
                Pos {
                    col: p.col.saturating_sub(1),
                    ..p
                },
                false,
            ),
            KeyCode::Char('l') | KeyCode::Right => (
                Pos {
                    col: (p.col + 1).min(self.line_len(p.row)),
                    ..p
                },
                false,
            ),
            KeyCode::Char('j') | KeyCode::Down => (self.vertical(1), true),
            KeyCode::Char('k') | KeyCode::Up => (self.vertical(-1), true),
            KeyCode::Char('w') => (self.next_word(p), false),
            KeyCode::Char('b') => (self.prev_word(p), false),
            KeyCode::Char('e') => (self.word_end(p), false),
            KeyCode::Char('0') | KeyCode::Home => (Pos { col: 0, ..p }, false),
            KeyCode::Char('^') => (
                Pos {
                    col: self.first_non_blank(p.row),
                    ..p
                },
                false,
            ),
            KeyCode::Char('$') | KeyCode::End => (
                Pos {
                    col: self.line_len(p.row),
                    ..p
                },
                false,
            ),
            KeyCode::Char('G') => (
                Pos {
                    row: self.lines.len() - 1,
                    col: 0,
                },
                true,
            ),
            KeyCode::PageDown => (self.vertical(20), true),
            KeyCode::PageUp => (self.vertical(-20), true),
            _ => return None,
        })
    }

    /// Normal-mode cursor: never past the last character.
    fn normal_clamp(&mut self) {
        self.clamp();
        let len = self.line_len(self.cursor.row);
        if len > 0 && self.cursor.col >= len {
            self.cursor.col = len - 1;
        }
    }

    fn enter_insert(&mut self) -> EditOutcome {
        self.vim = VimMode::Insert;
        self.anchor = None;
        self.last_group = None;
        EditOutcome::Changed
    }

    /// Applies operator `op` from the cursor to `target`.
    fn apply_operator(&mut self, op: char, target: Pos, linewise: bool) -> EditOutcome {
        let (a, b) = if target < self.cursor {
            (target, self.cursor)
        } else {
            (self.cursor, target)
        };
        let range = if linewise {
            let start = self.offset(Pos { row: a.row, col: 0 });
            let end_row_len = self.line_len(b.row);
            let mut end = self.offset(Pos {
                row: b.row,
                col: end_row_len,
            });
            // Include the line break (before the range if it is the last line).
            let total = self.text().len();
            let start = if end < total {
                end += 1;
                start
            } else {
                start.saturating_sub(1)
            };
            start..end
        } else {
            self.offset(a)..self.offset(b)
        };
        let text = self.text()[range.clone()].to_string();
        self.register = text.clone();
        self.linewise = linewise;
        match op {
            'y' => {
                self.cursor = a;
                self.normal_clamp();
                EditOutcome::Copied(text)
            }
            'd' | 'c' => {
                self.checkpoint(Group::Other);
                if linewise && op == 'c' {
                    // `cc`: keep an empty line to type on.
                    let start = self.offset(Pos { row: a.row, col: 0 });
                    let end = self.offset(Pos {
                        row: b.row,
                        col: self.line_len(b.row),
                    });
                    self.delete_range(start..end);
                } else {
                    self.delete_range(range);
                    if linewise {
                        let row = a.row.min(self.lines.len() - 1);
                        self.cursor = Pos {
                            row,
                            col: self.first_non_blank(row),
                        };
                    }
                }
                if op == 'c' {
                    self.enter_insert();
                } else {
                    self.normal_clamp();
                }
                EditOutcome::Copied(text)
            }
            '>' | '<' => {
                self.checkpoint(Group::Other);
                self.indent_lines(a.row..b.row + 1, op == '<');
                EditOutcome::Changed
            }
            _ => EditOutcome::Ignored,
        }
    }

    fn paste(&mut self, after: bool) -> EditOutcome {
        if self.register.is_empty() {
            return EditOutcome::Ignored;
        }
        self.checkpoint(Group::Other);
        let text = self.register.clone();
        if self.linewise {
            let body = text.strip_suffix('\n').unwrap_or(&text);
            let row = if after {
                self.cursor.row + 1
            } else {
                self.cursor.row
            };
            for (i, l) in body.split('\n').enumerate() {
                self.lines.insert(row + i, l.to_string());
            }
            self.cursor = Pos {
                row,
                col: self.first_non_blank(row),
            };
        } else {
            if after && self.line_len(self.cursor.row) > 0 {
                self.cursor.col += 1;
            }
            let save = self.last_group;
            for c in text.chars() {
                if c == '\n' {
                    self.split_line(false);
                } else {
                    self.insert_char(c);
                }
            }
            self.last_group = save;
            self.cursor.col = self.cursor.col.saturating_sub(1);
        }
        self.last_group = None;
        EditOutcome::Changed
    }

    fn vim_normal_key(&mut self, key: &KeyEvent) -> EditOutcome {
        use EditOutcome::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return match key.code {
                KeyCode::Char('r') => {
                    self.redo();
                    Changed
                }
                KeyCode::Char('d') => {
                    self.cursor = self.vertical(10);
                    Moved
                }
                KeyCode::Char('u') => {
                    self.cursor = self.vertical(-10);
                    Moved
                }
                _ => Ignored,
            };
        }
        if let Some(op) = self.pending.take() {
            return match (op, key.code) {
                (_, KeyCode::Esc) => Moved,
                ('g', KeyCode::Char('g')) => {
                    self.cursor = Pos::default();
                    Moved
                }
                ('d' | 'c' | 'y', KeyCode::Char(c)) if c == op => {
                    self.apply_operator(op, self.cursor, true)
                }
                ('>' | '<', KeyCode::Char(c)) if c == op => {
                    self.apply_operator(op, self.cursor, true)
                }
                ('d' | 'c' | 'y' | '>' | '<', KeyCode::Char('g')) => {
                    self.pending = Some(op.to_ascii_uppercase());
                    Moved
                }
                ('D' | 'C' | 'Y', KeyCode::Char('g')) => {
                    let op = op.to_ascii_lowercase();
                    self.apply_operator(op, Pos::default(), true)
                }
                ('d' | 'c' | 'y' | '>' | '<', code) => {
                    // `cw` behaves like `ce` in vim.
                    let code = if op == 'c' && code == KeyCode::Char('w') {
                        KeyCode::Char('e')
                    } else {
                        code
                    };
                    match self.motion(code) {
                        // `e` is inclusive.
                        Some((mut target, linewise)) => {
                            if code == KeyCode::Char('e') {
                                target.col += 1;
                            }
                            self.apply_operator(op, target, linewise)
                        }
                        None => Moved,
                    }
                }
                _ => Moved,
            };
        }
        if let Some((target, _)) = self.motion(key.code) {
            self.cursor = target;
            self.normal_clamp();
            return Moved;
        }
        let p = self.cursor;
        match key.code {
            KeyCode::Char(c @ ('d' | 'c' | 'y' | 'g' | '>' | '<')) => {
                self.pending = Some(c);
                Moved
            }
            KeyCode::Char('i') => self.enter_insert(),
            KeyCode::Char('a') => {
                self.cursor.col = (p.col + 1).min(self.line_len(p.row));
                self.enter_insert()
            }
            KeyCode::Char('I') => {
                self.cursor.col = self.first_non_blank(p.row);
                self.enter_insert()
            }
            KeyCode::Char('A') => {
                self.cursor.col = self.line_len(p.row);
                self.enter_insert()
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                self.checkpoint(Group::Other);
                let indent: String = self.lines[p.row]
                    .chars()
                    .take_while(|c| *c == ' ')
                    .collect();
                let row = if key.code == KeyCode::Char('o') {
                    p.row + 1
                } else {
                    p.row
                };
                self.cursor = Pos {
                    row,
                    col: indent.chars().count(),
                };
                self.lines.insert(row, indent);
                self.enter_insert()
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.line_len(p.row) == 0 {
                    return Moved;
                }
                let target = Pos {
                    col: p.col + 1,
                    ..p
                };
                self.apply_operator('d', target, false)
            }
            KeyCode::Char('X') if p.col > 0 => self.apply_operator(
                'd',
                Pos {
                    col: p.col - 1,
                    ..p
                },
                false,
            ),
            KeyCode::Char('D') => {
                let end = Pos {
                    col: self.line_len(p.row),
                    ..p
                };
                self.apply_operator('d', end, false)
            }
            KeyCode::Char('C') => {
                let end = Pos {
                    col: self.line_len(p.row),
                    ..p
                };
                self.apply_operator('c', end, false)
            }
            KeyCode::Char('J') if p.row + 1 < self.lines.len() => {
                self.checkpoint(Group::Other);
                let next = self.lines.remove(p.row + 1);
                let line = &mut self.lines[p.row];
                let join_col = line.trim_end().chars().count();
                *line = format!("{} {}", line.trim_end(), next.trim_start());
                self.cursor.col = join_col;
                Changed
            }
            KeyCode::Char('p') => self.paste(true),
            KeyCode::Char('P') => self.paste(false),
            KeyCode::Char('u') => {
                self.undo();
                self.normal_clamp();
                Changed
            }
            KeyCode::Char('v') => {
                self.vim = VimMode::Visual;
                self.anchor = Some(p);
                Moved
            }
            KeyCode::Char('V') => {
                self.vim = VimMode::VisualLine;
                self.anchor = Some(p);
                Moved
            }
            KeyCode::Enter => {
                self.cursor = self.vertical(1);
                self.cursor.col = self.first_non_blank(self.cursor.row);
                Moved
            }
            KeyCode::Backspace => {
                if p.col > 0 {
                    self.cursor.col -= 1;
                }
                Moved
            }
            _ => Ignored,
        }
    }

    fn vim_visual_key(&mut self, key: &KeyEvent) -> EditOutcome {
        use EditOutcome::*;
        let leave = |e: &mut Self| {
            e.vim = VimMode::Normal;
            e.anchor = None;
        };
        if self.pending.take() == Some('g') {
            if key.code == KeyCode::Char('g') {
                self.cursor = Pos::default();
            }
            return Moved;
        }
        if let Some((target, _)) = self.motion(key.code) {
            self.cursor = target;
            self.normal_clamp();
            return Moved;
        }
        match key.code {
            KeyCode::Esc => {
                leave(self);
                Moved
            }
            KeyCode::Char('g') => {
                self.pending = Some('g');
                Moved
            }
            KeyCode::Char('v') if self.vim == VimMode::Visual => {
                leave(self);
                Moved
            }
            KeyCode::Char('V') if self.vim == VimMode::VisualLine => {
                leave(self);
                Moved
            }
            KeyCode::Char('v') => {
                self.vim = VimMode::Visual;
                Moved
            }
            KeyCode::Char('V') => {
                self.vim = VimMode::VisualLine;
                Moved
            }
            KeyCode::Char('o') => {
                if let Some(a) = self.anchor {
                    self.anchor = Some(self.cursor);
                    self.cursor = a;
                }
                Moved
            }
            KeyCode::Char(c @ ('d' | 'x' | 'y' | 'c')) => {
                let Some(r) = self.selection() else {
                    leave(self);
                    return Moved;
                };
                let linewise = self.vim == VimMode::VisualLine;
                let start = self.pos_of(r.start);
                let mut text = self.text()[r.clone()].to_string();
                if linewise {
                    text.push('\n');
                }
                self.register = text.clone();
                self.linewise = linewise;
                leave(self);
                if c == 'y' {
                    self.cursor = start;
                    self.normal_clamp();
                    return Copied(text);
                }
                self.checkpoint(Group::Other);
                if linewise {
                    self.cursor = start;
                    let rows = start.row..self.pos_of(r.end).row + 1;
                    let n = rows.len();
                    if c == 'c' {
                        for row in rows.clone().skip(1).rev() {
                            self.lines.remove(row);
                        }
                        self.lines[start.row].clear();
                    } else if n >= self.lines.len() {
                        self.lines = vec![String::new()];
                    } else {
                        self.lines.drain(rows);
                    }
                    self.clamp();
                } else {
                    self.delete_range(r);
                }
                if c == 'c' {
                    self.enter_insert();
                } else {
                    self.normal_clamp();
                }
                Copied(text)
            }
            KeyCode::Char(c @ ('>' | '<')) => {
                self.checkpoint(Group::Other);
                let rows = self.selected_rows();
                self.indent_lines(rows, c == '<');
                leave(self);
                Changed
            }
            _ => Ignored,
        }
    }

    /// Keeps the cursor in a `height` x `width` window (text columns).
    pub fn scroll_into_view(&mut self, height: usize, width: usize) {
        let h = height.max(1);
        if self.cursor.row < self.scroll {
            self.scroll = self.cursor.row;
        } else if self.cursor.row >= self.scroll + h {
            self.scroll = self.cursor.row + 1 - h;
        }
        let x = self.cursor_width();
        let w = width.max(2);
        if x < self.hscroll {
            self.hscroll = x;
        } else if x >= self.hscroll + w {
            self.hscroll = x + 1 - w;
        }
    }

    /// Moves the cursor to a byte offset (error positions).
    pub fn jump_to(&mut self, offset: usize) {
        self.cursor = self.pos_of(offset);
        self.anchor = None;
        if self.mode == EditorMode::Vim && self.vim != VimMode::Insert {
            self.vim = VimMode::Normal;
            self.normal_clamp();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mods(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn type_str(e: &mut Editor, s: &str) {
        for c in s.chars() {
            let k = match c {
                '\n' => key(KeyCode::Enter),
                c => key(KeyCode::Char(c)),
            };
            e.handle_key(&k);
        }
    }

    /// Vim normal-mode keys, as a string (`<esc>` for Esc).
    fn vim(e: &mut Editor, keys: &str) {
        let mut rest = keys;
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix("<esc>") {
                e.handle_key(&key(KeyCode::Esc));
                rest = r;
                continue;
            }
            let c = rest.chars().next().unwrap();
            e.handle_key(&key(KeyCode::Char(c)));
            rest = &rest[c.len_utf8()..];
        }
    }

    #[test]
    fn classic_typing_indent_and_undo() {
        let mut e = Editor::new(EditorMode::Classic, 2);
        type_str(&mut e, "select (\nid");
        assert_eq!(e.text(), "select (\n  id");
        type_str(&mut e, ", name");
        e.handle_key(&mods(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "select (\n  id, ", "one word per undo step");
        e.handle_key(&mods(KeyCode::Char('z'), KeyModifiers::CONTROL));
        e.handle_key(&mods(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "select (\n  id, ");
        // Backspace in the indentation removes a whole level.
        e.set_text("  ");
        e.handle_key(&key(KeyCode::Backspace));
        assert_eq!(e.text(), "");
    }

    #[test]
    fn classic_selection_cut_paste() {
        let mut e = Editor::new(EditorMode::Classic, 4);
        e.set_text("select 1;\nselect 2;");
        e.cursor = Pos { row: 0, col: 0 };
        for _ in 0..6 {
            e.handle_key(&mods(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(e.selected_text().as_deref(), Some("select"));
        let out = e.handle_key(&mods(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(out, EditOutcome::Copied("select".into()));
        assert_eq!(e.text(), " 1;\nselect 2;");
        e.handle_key(&mods(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "select 1;\nselect 2;");
        // Typing replaces the selection.
        e.handle_key(&mods(KeyCode::Char('a'), KeyModifiers::CONTROL));
        type_str(&mut e, "x");
        assert_eq!(e.text(), "x");
    }

    #[test]
    fn offsets_round_trip() {
        let mut e = Editor::default();
        e.set_text("é1\nab\n");
        let p = Pos { row: 1, col: 1 };
        assert_eq!(e.offset(p), 5);
        assert_eq!(e.pos_of(5), p);
        assert_eq!(e.pos_of(100), Pos { row: 2, col: 0 });
    }

    #[test]
    fn vim_motions_and_operators() {
        let mut e = Editor::new(EditorMode::Vim, 4);
        e.set_text("select id, name\nfrom users\nwhere id = 1");
        vim(&mut e, "gg");
        assert_eq!(e.cursor, Pos::default());
        vim(&mut e, "w");
        assert_eq!(e.cursor.col, 7);
        vim(&mut e, "cwuser_id<esc>");
        assert_eq!(e.lines()[0], "select user_id, name");
        vim(&mut e, "j");
        vim(&mut e, "dd");
        assert_eq!(e.text(), "select user_id, name\nwhere id = 1");
        vim(&mut e, "P");
        assert_eq!(e.text(), "select user_id, name\nfrom users\nwhere id = 1");
        vim(&mut e, "u");
        assert_eq!(e.text(), "select user_id, name\nwhere id = 1");
        e.handle_key(&mods(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(e.lines().len(), 3);
        vim(&mut e, "G$x");
        assert_eq!(e.lines()[2], "where id = ");
        vim(&mut e, "0D");
        assert_eq!(e.lines()[2], "");
        vim(&mut e, "ggyyGp");
        assert_eq!(e.lines()[3], "select user_id, name");
        vim(&mut e, "kJ");
        assert_eq!(e.lines()[2], " select user_id, name");
    }

    #[test]
    fn vim_insert_and_visual() {
        let mut e = Editor::new(EditorMode::Vim, 2);
        assert!(!e.inserting());
        vim(&mut e, "iselect 1<esc>");
        assert_eq!(e.text(), "select 1");
        assert_eq!(e.cursor.col, 7);
        vim(&mut e, "0vey");
        assert_eq!(e.register, "select");
        vim(&mut e, "oupdate t<esc>");
        assert_eq!(e.text(), "select 1\nupdate t");
        vim(&mut e, "ggVjd");
        assert_eq!(e.text(), "");
        vim(&mut e, "p");
        assert_eq!(e.text(), "\nselect 1\nupdate t");
        vim(&mut e, "ggdd>>");
        vim(&mut e, "Vj>");
        assert_eq!(e.text(), "    select 1\n  update t");
        vim(&mut e, "ggv$");
        assert_eq!(e.selected_text().as_deref(), Some("    select 1"));
    }
}
