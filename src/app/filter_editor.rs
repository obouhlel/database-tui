//! Structured filter popup (`f`): one row per condition
//! `column  operator  value`, combined with AND or OR.
//!
//! Keys: Tab / Shift+Tab change field, Up / Down change row, Left / Right
//! cycle the column or operator, typing edits the value (or jumps to a
//! column / operator by its first character), Ctrl+n adds a row, Ctrl+d
//! deletes it, Ctrl+o toggles AND / OR, Enter applies, Esc cancels.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::input::LineInput;
use crate::db::data::{Combinator, Condition, Filter, Op};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Column,
    Op,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRow {
    pub column: usize,
    pub op: Op,
    pub value: LineInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Apply,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEditor {
    pub columns: Vec<String>,
    pub rows: Vec<EditRow>,
    pub cursor: usize,
    pub field: Field,
    pub combinator: Combinator,
}

impl FilterEditor {
    /// Opens on the existing conditions, or on a new one for `current`.
    pub fn new(columns: Vec<String>, filter: &Filter, current: Option<&str>) -> Self {
        let mut rows: Vec<EditRow> = filter
            .conditions
            .iter()
            .filter_map(|c| {
                Some(EditRow {
                    column: columns.iter().position(|n| n == &c.column)?,
                    op: c.op,
                    value: LineInput::with_text(c.value.clone()),
                })
            })
            .collect();
        let start_col = current
            .and_then(|c| columns.iter().position(|n| n == c))
            .unwrap_or(0);
        let fresh = rows.is_empty();
        if fresh {
            rows.push(EditRow {
                column: start_col,
                op: Op::Eq,
                value: LineInput::default(),
            });
        }
        Self {
            columns,
            rows,
            cursor: 0,
            // A new condition starts on its value: the column is preselected.
            field: if fresh { Field::Value } else { Field::Column },
            combinator: filter.combinator,
        }
    }

    /// Conditions to apply; rows needing a value but left empty are dropped.
    pub fn conditions(&self) -> Vec<Condition> {
        self.rows
            .iter()
            .filter(|r| !r.op.has_value() || !r.value.is_empty())
            .map(|r| Condition {
                column: self.columns[r.column].clone(),
                op: r.op,
                value: if r.op.has_value() {
                    r.value.text().to_string()
                } else {
                    String::new()
                },
            })
            .collect()
    }

    /// Pasted text goes into the value of the current row (surrounding
    /// blanks and line breaks dropped: a copied cell ends with one).
    pub fn paste(&mut self, text: &str) {
        let Some(row) = self.rows.get_mut(self.cursor) else {
            return;
        };
        if !row.op.has_value() {
            row.op = Op::Eq;
        }
        self.field = Field::Value;
        row.value.insert_str(text.trim());
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Outcome::Apply,
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('n') if ctrl => {
                let column = self.rows.get(self.cursor).map_or(0, |r| r.column);
                self.rows.insert(
                    self.cursor + 1,
                    EditRow {
                        column,
                        op: Op::Eq,
                        value: LineInput::default(),
                    },
                );
                self.cursor += 1;
                self.field = Field::Column;
            }
            KeyCode::Char('d') if ctrl => {
                if self.rows.len() > 1 {
                    self.rows.remove(self.cursor);
                    self.cursor = self.cursor.min(self.rows.len() - 1);
                } else {
                    self.rows[0].value.clear();
                }
            }
            KeyCode::Char('o') if ctrl => {
                self.combinator = match self.combinator {
                    Combinator::And => Combinator::Or,
                    Combinator::Or => Combinator::And,
                };
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(self.rows.len() - 1),
            KeyCode::Tab => self.next_field(true),
            KeyCode::BackTab => self.next_field(false),
            _ => self.edit_field(key),
        }
        Outcome::Continue
    }

    fn next_field(&mut self, forward: bool) {
        let has_value = self.rows[self.cursor].op.has_value();
        self.field = match (self.field, forward) {
            (Field::Column, true) => Field::Op,
            (Field::Op, true) if has_value => Field::Value,
            (Field::Op, true) | (Field::Value, true) => {
                if self.cursor + 1 < self.rows.len() {
                    self.cursor += 1;
                }
                Field::Column
            }
            (Field::Column, false) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    if self.rows[self.cursor].op.has_value() {
                        Field::Value
                    } else {
                        Field::Op
                    }
                } else {
                    Field::Column
                }
            }
            (Field::Op, false) => Field::Column,
            (Field::Value, false) => Field::Op,
        };
    }

    fn edit_field(&mut self, key: &KeyEvent) {
        let ncols = self.columns.len();
        let row = &mut self.rows[self.cursor];
        match self.field {
            Field::Column => match key.code {
                KeyCode::Left => row.column = (row.column + ncols - 1) % ncols,
                KeyCode::Right => row.column = (row.column + 1) % ncols,
                KeyCode::Char(c) => {
                    // Next column starting with this character.
                    let c = c.to_lowercase().next().unwrap_or(c);
                    if let Some(i) = (1..=ncols)
                        .map(|d| (row.column + d) % ncols)
                        .find(|i| self.columns[*i].to_lowercase().starts_with(c))
                    {
                        row.column = i;
                    }
                }
                _ => {}
            },
            Field::Op => {
                let i = Op::ALL.iter().position(|o| *o == row.op).unwrap_or(0);
                let n = Op::ALL.len();
                match key.code {
                    KeyCode::Left => row.op = Op::ALL[(i + n - 1) % n],
                    KeyCode::Right => row.op = Op::ALL[(i + 1) % n],
                    KeyCode::Char(c) => {
                        let c = c.to_ascii_uppercase();
                        if let Some(j) = (1..=n)
                            .map(|d| (i + d) % n)
                            .find(|j| Op::ALL[*j].label().starts_with(c))
                        {
                            row.op = Op::ALL[j];
                        }
                    }
                    _ => {}
                }
            }
            Field::Value => {
                if row.op.has_value() {
                    row.value.handle_key(key);
                }
            }
        }
    }

    pub fn into_filter(self, free_where: Option<String>) -> Filter {
        Filter {
            conditions: self.conditions(),
            combinator: self.combinator,
            free_where,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(e: &mut FilterEditor, s: &str) {
        for c in s.chars() {
            e.handle_key(&k(KeyCode::Char(c)));
        }
    }

    fn cols() -> Vec<String> {
        vec!["id".into(), "status".into(), "total".into()]
    }

    #[test]
    fn new_condition_on_current_column() {
        let mut e = FilterEditor::new(cols(), &Filter::default(), Some("status"));
        assert_eq!(e.field, Field::Value);
        typed(&mut e, "paid");
        assert_eq!(e.handle_key(&k(KeyCode::Enter)), Outcome::Apply);
        assert_eq!(
            e.conditions(),
            vec![Condition {
                column: "status".into(),
                op: Op::Eq,
                value: "paid".into()
            }]
        );
    }

    #[test]
    fn fields_ops_and_rows() {
        let mut e = FilterEditor::new(cols(), &Filter::default(), Some("id"));
        e.handle_key(&k(KeyCode::BackTab)); // -> operator
        typed(&mut e, ">");
        assert_eq!(e.rows[0].op, Op::Gt);
        e.handle_key(&k(KeyCode::Tab));
        typed(&mut e, "100");
        e.handle_key(&ctrl('n'));
        typed(&mut e, "t"); // column starting with t
        e.handle_key(&k(KeyCode::Tab));
        typed(&mut e, "I"); // ILIKE (first op starting with I)
        assert_eq!(e.rows[1].op, Op::ILike);
        typed(&mut e, "i"); // next operator starting with I: IS NULL
        assert_eq!(e.rows[1].op, Op::IsNull);
        e.handle_key(&ctrl('o'));
        let f = e.into_filter(None);
        assert_eq!(f.combinator, Combinator::Or);
        assert_eq!(
            f.conditions,
            vec![
                Condition {
                    column: "id".into(),
                    op: Op::Gt,
                    value: "100".into()
                },
                Condition {
                    column: "total".into(),
                    op: Op::IsNull,
                    value: String::new()
                },
            ]
        );
    }

    #[test]
    fn empty_values_dropped_and_rows_deleted() {
        let existing = Filter {
            conditions: vec![
                Condition {
                    column: "id".into(),
                    op: Op::Eq,
                    value: "1".into(),
                },
                Condition {
                    column: "status".into(),
                    op: Op::Eq,
                    value: "x".into(),
                },
            ],
            combinator: Combinator::And,
            free_where: None,
        };
        let mut e = FilterEditor::new(cols(), &existing, None);
        assert_eq!(e.rows.len(), 2);
        e.handle_key(&k(KeyCode::Down));
        e.handle_key(&ctrl('d'));
        assert_eq!(e.rows.len(), 1);
        e.handle_key(&ctrl('d'));
        assert!(
            e.conditions().is_empty(),
            "last row cleared, empty value dropped"
        );
    }
}
