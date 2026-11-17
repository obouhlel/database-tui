//! Data tab editing: cell editor, new / deleted rows, undo / redo, the
//! pending changes view (diff and generated SQL), and applying everything in
//! one transaction. Part of the reducer: no I/O, only [`Effect`]s.
//!
//! Staging is per table: leaving a table (opening another object,
//! disconnecting, quitting) with pending changes asks to apply or discard
//! them first.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::grid::{Cell, CellStyle, GridData, GridState};
use super::input::{InputOutcome, LineInput};
use super::staging::Origin;
use super::textarea::{AreaOutcome, TextArea};
use super::validate::validate;
use super::value::ValueKind;
use super::{App, Effect, Level, Overlay};
use crate::db::catalog::ObjectRef;
use crate::db::changes::{ApplyError, ChangeCounts, NewValue};
use crate::db::worker::{DbCommand, DbEvent};
use crate::keymap::Action;

fn is_uuid(data_type: &str) -> bool {
    data_type.trim().eq_ignore_ascii_case("uuid")
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Values longer than this open the multi-line editor.
const LONG_VALUE: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// A loaded row (its values when the editor opened).
    Existing(Vec<Option<String>>),
    /// A staged new row, by index.
    Insert(usize),
}

#[derive(Debug, Clone)]
pub enum EditorInput {
    Line(LineInput),
    Multi(TextArea),
}

impl EditorInput {
    pub fn text(&self) -> String {
        match self {
            EditorInput::Line(l) => l.text().to_string(),
            EditorInput::Multi(t) => t.text(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CellEditor {
    pub target: EditTarget,
    /// Column index (in `DataView::columns`) and display column.
    pub col: usize,
    pub dcol: usize,
    pub column: String,
    pub data_type: String,
    pub input: EditorInput,
    pub error: Option<String>,
    /// Original text of a JSON value, to ignore a pure reformatting.
    json_original: Option<String>,
    /// Value when the editor opened and its text: validating the text
    /// unchanged keeps a NULL / DEFAULT value as it is.
    initial: (NewValue, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesMode {
    Diff,
    Sql,
}

/// `D`: everything that will change.
#[derive(Debug, Clone)]
pub struct ChangesView {
    pub mode: ChangesMode,
    pub grid: GridData,
    pub state: GridState,
    pub sql: String,
    pub scroll: u16,
    /// The generated SQL being edited by hand.
    pub editing: Option<TextArea>,
    pub counts: ChangeCounts,
}

/// What to do once the pending changes are applied or discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Open(ObjectRef),
    /// Follow a link of the Schema tab.
    Follow(ObjectRef),
    /// Back to the previous object.
    Back,
    Quit,
    Disconnect(String),
}

impl After {
    pub fn describe(&self) -> String {
        match self {
            After::Open(o) | After::Follow(o) => format!("ouvrir {}", o.qualified()),
            After::Back => "revenir à l'objet précédent".into(),
            After::Quit => "quitter".into(),
            After::Disconnect(n) => format!("fermer {n}"),
        }
    }
}

impl App {
    /// Pending changes of the open table, if any.
    pub fn pending_changes(&self) -> Option<ChangeCounts> {
        if self.tab == super::Tab::Sql && !self.results_mode {
            return self
                .sql
                .active_view()
                .filter(|v| !v.staging.is_empty())
                .map(|v| v.staging.counts());
        }
        (!self.data.staging.is_empty()).then(|| self.data.staging.counts())
    }

    /// Before leaving the table: `true` if the action must wait (the user is
    /// asked to apply or discard pending changes).
    pub(super) fn guard(&mut self, after: After) -> bool {
        if self.results_mode || self.data.staging.is_empty() {
            return self.guard_transaction(&after);
        }
        let same_table = match &after {
            After::Open(o) | After::Follow(o) => self.data.is_for(o),
            After::Back => false,
            After::Disconnect(n) => self.data.object.as_ref().is_none_or(|o| &o.connection != n),
            After::Quit => false,
        };
        if same_table {
            return self.guard_transaction(&after);
        }
        self.overlay = Some(Overlay::PendingChanges(after));
        true
    }

    pub(super) fn perform_after(&mut self, after: After) -> Vec<Effect> {
        match after {
            After::Open(o) => self.open_object(o),
            After::Follow(o) => {
                if let Some(cur) = self.current.take() {
                    self.back_stack.push(cur);
                }
                self.go_to(o)
            }
            After::Back => match self.back_stack.pop() {
                Some(prev) => self.go_to(prev),
                None => self.notify(Level::Info, "Pas d'objet précédent"),
            },
            After::Quit => {
                if !self.guard(After::Quit) {
                    self.should_quit = true;
                }
            }
            After::Disconnect(n) => return self.disconnect(&n),
        }
        Vec::new()
    }

    fn editable(&mut self) -> bool {
        if let Some(reason) = self.data.read_only_reason.clone() {
            self.notify(Level::Warning, format!("Édition impossible — {reason}"));
            return false;
        }
        if self.data.applying.is_some() {
            self.notify(Level::Info, "Application des changements en cours…");
            return false;
        }
        true
    }

    pub(super) fn dispatch_edit(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        match action {
            EditCell => {
                if self.editable() {
                    self.open_editor(self.data.grid.row, self.data.grid.col);
                }
            }
            InsertRow => {
                if self.editable() {
                    self.data.staging.insert_row();
                    self.data.visual = None;
                    self.data.grid.top();
                    // Start on the first column the server does not fill.
                    let d = &self.data;
                    let first = d
                        .order
                        .iter()
                        .position(|i| !d.columns[*i].has_default)
                        .unwrap_or(0);
                    self.data.grid.col = first;
                    self.open_editor(0, first);
                }
            }
            DuplicateRow => {
                if self.editable() {
                    self.duplicate_rows();
                }
            }
            DeleteRow => {
                if self.editable() {
                    self.delete_rows();
                }
            }
            Undo => {
                if self.data.staging.undo() {
                    self.after_staging_change();
                    self.notify(Level::Info, "Changement annulé");
                } else {
                    self.notify(Level::Info, "Rien à annuler");
                }
            }
            Redo => {
                if self.data.staging.redo() {
                    self.after_staging_change();
                    self.notify(Level::Info, "Changement rétabli");
                } else {
                    self.notify(Level::Info, "Rien à rétablir");
                }
            }
            ShowChanges => match self.changes_view() {
                Some(v) => self.overlay = Some(Overlay::Changes(Box::new(v))),
                None => self.notify(Level::Info, "Aucun changement en attente"),
            },
            ApplyChanges => self.ask_apply(None),
            DiscardChanges => {
                if self.data.staging.is_empty() {
                    self.notify(Level::Info, "Aucun changement en attente");
                } else {
                    self.overlay = Some(Overlay::ConfirmDiscard);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn after_staging_change(&mut self) {
        let rows = self.data.row_count();
        self.data.grid.scroll_into_view(rows);
    }

    /// `p`: copies the current row (or the selected rows) into new rows.
    /// Columns only the server may fill, and key columns with a default,
    /// are left to the server; a `uuid` key without default gets a fresh
    /// UUID. A key that is still a copy is opened in the editor.
    fn duplicate_rows(&mut self) {
        let (range, _) = self.data.selection(true);
        self.data.visual = None;
        let d = &self.data;
        let mut rows = Vec::new();
        for r in range {
            let values: Vec<NewValue> = if r < d.ins() {
                d.staging.state.inserts[r].clone()
            } else if let Some(row) = d.row_at(r) {
                let staged = d
                    .staging
                    .key_of(row)
                    .and_then(|k| d.staging.state.updates.get(&k));
                (0..d.columns.len())
                    .map(|c| match staged.and_then(|e| e.cells.get(&c)) {
                        Some(v) => v.clone(),
                        None => NewValue::from_text(row.get(c).and_then(|v| v.as_deref())),
                    })
                    .collect()
            } else {
                continue;
            };
            let values = values
                .into_iter()
                .zip(&d.columns)
                .map(|(v, c)| {
                    if c.server_only || (c.key && c.has_default) {
                        NewValue::Default
                    } else if c.key && is_uuid(&c.data_type) {
                        NewValue::Text(new_uuid())
                    } else {
                        v
                    }
                })
                .collect();
            rows.push(values);
        }
        let n = self.data.staging.insert_rows(rows);
        if n == 0 {
            self.notify(Level::Warning, "Ligne non chargée");
            return;
        }
        self.data.grid.top();
        // A key column still holding the copied value must be changed.
        let d = &self.data;
        let copied_key = d.order.iter().position(|&c| {
            let col = &d.columns[c];
            col.key && !col.has_default && !col.server_only && !is_uuid(&col.data_type)
        });
        match copied_key {
            Some(dcol) if n == 1 => {
                self.data.grid.col = dcol;
                self.notify(Level::Info, "Ligne dupliquée — modifiez la clé");
                self.open_editor(0, dcol);
            }
            _ => self.notify(Level::Info, format!("{n} ligne(s) dupliquée(s)")),
        }
    }

    /// `dd`: marks the current row (or the selected rows) for deletion, or
    /// unmarks them; removes a new row.
    fn delete_rows(&mut self) {
        let (range, _) = self.data.selection(true);
        self.data.visual = None;
        if range.len() == 1 && range.start < self.data.ins() {
            self.data.staging.remove_insert(range.start);
            self.after_staging_change();
            self.notify(Level::Info, "Nouvelle ligne retirée");
            return;
        }
        let rows: Vec<Vec<Option<String>>> = range
            .clone()
            .filter_map(|r| self.data.row_at(r).cloned())
            .collect();
        if rows.is_empty() {
            self.notify(Level::Warning, "Ligne non chargée");
            return;
        }
        let (mut marked, mut unmarked) = (0, 0);
        for r in &rows {
            match self.data.staging.toggle_delete(r) {
                Some(true) => marked += 1,
                Some(false) => unmarked += 1,
                None => {}
            }
        }
        if range.len() == 1 {
            let n = self.data.row_count();
            self.data.grid.move_by(1, n);
        }
        let msg = match (marked, unmarked) {
            (0, 0) => "Ligne sans clé : suppression impossible".to_string(),
            (m, 0) => format!("{m} ligne(s) marquée(s) pour suppression"),
            (0, u) => format!("{u} ligne(s) conservée(s)"),
            (m, u) => format!("{m} ligne(s) marquée(s), {u} conservée(s)"),
        };
        self.notify(Level::Info, msg);
    }

    /// Opens the editor on grid row `row`, display column `dcol`.
    fn open_editor(&mut self, row: usize, dcol: usize) {
        let d = &self.data;
        let Some(&col) = d.order.get(dcol) else {
            return;
        };
        let column = &d.columns[col];
        let (target, current, value) = if row < d.ins() {
            let v = d.staging.state.inserts[row][col].clone();
            let text = match &v {
                NewValue::Text(t) => t.clone(),
                _ => String::new(),
            };
            (EditTarget::Insert(row), text, v)
        } else {
            let Some(r) = d.row_at(row) else {
                self.notify(Level::Info, "Ligne non chargée");
                return;
            };
            if d.staging.cell(r, col) == Some(super::staging::CellChange::Deleted) {
                self.notify(
                    Level::Info,
                    "Ligne marquée pour suppression (dd pour la conserver)",
                );
                return;
            }
            let staged = d
                .staging
                .state
                .updates
                .get(&d.staging.key_of(r).unwrap_or_default());
            let value = match staged.and_then(|e| e.cells.get(&col)) {
                Some(v) => v.clone(),
                None => NewValue::from_text(r[col].as_deref()),
            };
            let text = match &value {
                NewValue::Text(t) => t.clone(),
                _ => String::new(),
            };
            (EditTarget::Existing(r.clone()), text, value)
        };
        let initial = (value, current.clone());
        let json = column.kind == ValueKind::Json;
        let (text, json_original) = match serde_json::from_str::<serde_json::Value>(&current) {
            Ok(v) if json && !current.is_empty() => (
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| current.clone()),
                Some(current.clone()),
            ),
            _ => (current, None),
        };
        let multi = text.contains('\n') || text.chars().count() > LONG_VALUE;
        let input = if multi {
            EditorInput::Multi(TextArea::new(&text))
        } else {
            EditorInput::Line(LineInput::with_text(text))
        };
        self.overlay = Some(Overlay::CellEditor(Box::new(CellEditor {
            target,
            col,
            dcol,
            column: column.name.clone(),
            data_type: column.data_type.clone(),
            input,
            error: None,
            json_original,
            initial,
        })));
    }

    pub(super) fn on_cell_editor_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::CellEditor(mut ed)) = self.overlay.take() else {
            return Vec::new();
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let multi = matches!(ed.input, EditorInput::Multi(_));
        // Moves to another column after validating.
        let mut next: Option<isize> = None;
        let value = match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Char('n') if ctrl => Some(NewValue::Null),
            KeyCode::Char('d') if ctrl => Some(NewValue::Default),
            KeyCode::Char('s') if ctrl => Some(NewValue::Text(ed.input.text())),
            KeyCode::Char('g') if ctrl => {
                // A fresh UUID: replaces a uuid value, else inserted.
                let id = new_uuid();
                match &mut ed.input {
                    EditorInput::Line(l) if is_uuid(&ed.data_type) => *l = LineInput::with_text(id),
                    EditorInput::Line(l) => l.insert_str(&id),
                    EditorInput::Multi(t) if is_uuid(&ed.data_type) => *t = TextArea::new(&id),
                    EditorInput::Multi(t) => t.insert_str(&id),
                }
                ed.error = None;
                None
            }
            KeyCode::Enter if alt && !multi => {
                // Switch to the multi-line editor.
                let text = ed.input.text();
                let mut area = TextArea::new(&text);
                area.row = 0;
                area.col = text.chars().count();
                ed.input = EditorInput::Multi(area);
                None
            }
            KeyCode::Enter if !multi || alt || ctrl => Some(NewValue::Text(ed.input.text())),
            KeyCode::Tab if !multi => {
                next = Some(1);
                Some(NewValue::Text(ed.input.text()))
            }
            KeyCode::BackTab if !multi => {
                next = Some(-1);
                Some(NewValue::Text(ed.input.text()))
            }
            _ => {
                let changed = match &mut ed.input {
                    EditorInput::Line(l) => l.handle_key(&key) == InputOutcome::Changed,
                    EditorInput::Multi(t) => t.handle_key(&key) == AreaOutcome::Changed,
                };
                if changed {
                    ed.error = None;
                }
                None
            }
        };
        let Some(mut value) = value else {
            self.overlay = Some(Overlay::CellEditor(ed));
            return Vec::new();
        };
        if let NewValue::Text(t) = &value
            && *t == ed.initial.1
            && ed.initial.0 != NewValue::Text(t.clone())
        {
            // Untouched NULL / DEFAULT.
            value = ed.initial.0.clone();
        }
        if let NewValue::Text(t) = &value {
            if let Err(e) = validate(&ed.data_type, t) {
                ed.error = Some(e);
                self.overlay = Some(Overlay::CellEditor(ed));
                return Vec::new();
            }
            // A JSON value that was only reformatted keeps its original
            // text; jsonb (which keeps no layout) is staged on one line.
            let parsed = serde_json::from_str::<serde_json::Value>(t).ok();
            if let Some(orig) = &ed.json_original
                && parsed.is_some()
                && parsed == serde_json::from_str::<serde_json::Value>(orig).ok()
            {
                value = NewValue::Text(orig.clone());
            } else if ed.data_type == "jsonb"
                && let Some(v) = parsed
            {
                value = NewValue::Text(v.to_string());
            }
        }
        match &ed.target {
            EditTarget::Existing(row) => {
                if !self.data.staging.set_cell(row, ed.col, value) {
                    self.notify(Level::Warning, "Ligne sans clé : modification impossible");
                    return Vec::new();
                }
            }
            EditTarget::Insert(i) => self.data.staging.set_insert_cell(*i, ed.col, value),
        }
        if let Some(delta) = next {
            let n = self.data.order.len();
            let dcol = ed.dcol.saturating_add_signed(delta);
            if dcol < n && dcol != ed.dcol {
                self.data.grid.col = dcol;
                self.open_editor(self.data.grid.row, dcol);
            }
        }
        Vec::new()
    }

    fn changes_view(&self) -> Option<ChangesView> {
        let d = &self.data;
        let obj = d.object.clone()?;
        if d.staging.is_empty() {
            return None;
        }
        let state = &d.staging.state;
        let key_text = |key: &Vec<String>| -> String {
            d.key_columns
                .iter()
                .zip(key)
                .map(|(c, v)| format!("{c} = {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let show = |v: Option<&str>| v.map_or_else(|| "NULL".to_string(), str::to_string);
        let mut rows = Vec::new();
        for (key, edit) in &state.updates {
            if state.deletes.contains_key(key) {
                continue;
            }
            for (col, v) in &edit.cells {
                let style = if edit.conflict {
                    CellStyle::Conflict
                } else {
                    CellStyle::Modified
                };
                rows.push(vec![
                    Cell::styled("UPDATE", CellStyle::Modified),
                    Cell::new(key_text(key)),
                    Cell::new(d.columns[*col].name.clone()),
                    Cell::styled(
                        show(edit.original.get(*col).and_then(|o| o.as_deref())),
                        CellStyle::Dim,
                    ),
                    Cell::styled(v.display(), style),
                ]);
            }
        }
        let n = state.inserts.len();
        for (i, values) in state.inserts.iter().enumerate().rev() {
            let label = format!("nouvelle {}", n - i);
            let set: Vec<(usize, &NewValue)> = values
                .iter()
                .enumerate()
                .filter(|(_, v)| **v != NewValue::Default)
                .collect();
            if set.is_empty() {
                rows.push(vec![
                    Cell::styled("INSERT", CellStyle::Inserted),
                    Cell::new(label.clone()),
                    Cell::styled("(toutes)", CellStyle::Dim),
                    Cell::new(""),
                    Cell::styled("DEFAULT", CellStyle::Inserted),
                ]);
            }
            for (col, v) in set {
                rows.push(vec![
                    Cell::styled("INSERT", CellStyle::Inserted),
                    Cell::new(label.clone()),
                    Cell::new(d.columns[col].name.clone()),
                    Cell::new(""),
                    Cell::styled(v.display(), CellStyle::Inserted),
                ]);
            }
        }
        for key in state.deletes.keys() {
            rows.push(vec![
                Cell::styled("DELETE", CellStyle::Deleted),
                Cell::new(key_text(key)),
                Cell::styled("(ligne entière)", CellStyle::Dim),
                Cell::new(""),
                Cell::new(""),
            ]);
        }
        let headers = ["Opération", "Ligne", "Colonne", "Avant", "Après"]
            .map(String::from)
            .to_vec();
        let (set, _) = d.staging.change_set(obj, d.column_specs());
        Some(ChangesView {
            mode: ChangesMode::Diff,
            grid: GridData::new(headers, rows, 1),
            state: GridState::default(),
            sql: crate::db::postgres::changes::preview(&set),
            scroll: 0,
            editing: None,
            counts: d.staging.counts(),
        })
    }

    pub(super) fn on_changes_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Changes(mut v)) = self.overlay.take() else {
            return Vec::new();
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(area) = &mut v.editing {
            match key.code {
                KeyCode::Esc => v.editing = None,
                KeyCode::Char('s') if ctrl => {
                    let script = area.text();
                    self.overlay = Some(Overlay::Changes(v));
                    self.ask_apply(Some(script));
                    return Vec::new();
                }
                _ => {
                    area.handle_key(&key);
                }
            }
            self.overlay = Some(Overlay::Changes(v));
            return Vec::new();
        }
        let rows = v.grid.rows.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('D') => return Vec::new(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('s') if !ctrl => {
                v.mode = match v.mode {
                    ChangesMode::Diff => ChangesMode::Sql,
                    ChangesMode::Sql => ChangesMode::Diff,
                };
            }
            KeyCode::Char('s') if ctrl => {
                self.overlay = Some(Overlay::Changes(v));
                self.ask_apply(None);
                return Vec::new();
            }
            KeyCode::Char('e') if v.mode == ChangesMode::Sql => {
                v.editing = Some(TextArea::new(&v.sql));
            }
            KeyCode::Char('j') | KeyCode::Down => match v.mode {
                ChangesMode::Diff => v.state.move_by(1, rows),
                ChangesMode::Sql => v.scroll = v.scroll.saturating_add(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match v.mode {
                ChangesMode::Diff => v.state.move_by(-1, rows),
                ChangesMode::Sql => v.scroll = v.scroll.saturating_sub(1),
            },
            KeyCode::Char('g') | KeyCode::Home => {
                v.state.top();
                v.scroll = 0;
            }
            KeyCode::Char('G') | KeyCode::End => v.state.bottom(rows),
            KeyCode::PageDown => v.state.move_by(v.state.page(), rows),
            KeyCode::PageUp => v.state.move_by(-v.state.page(), rows),
            _ => {}
        }
        self.overlay = Some(Overlay::Changes(v));
        Vec::new()
    }

    /// `Ctrl+s`: asks for confirmation (the connection name on production).
    fn ask_apply(&mut self, script: Option<String>) {
        if self.data.staging.is_empty() {
            self.notify(Level::Info, "Aucun changement en attente");
            return;
        }
        if self.data.applying.is_some() {
            self.notify(Level::Info, "Application déjà en cours…");
            return;
        }
        let in_txn = self.sql.txn != super::sql_tab::TxnState::Idle
            && self.data.object.as_ref().is_some_and(|o| {
                self.sql.target.as_ref() == Some(&(o.connection.clone(), o.database.clone()))
            });
        if in_txn {
            self.notify(
                Level::Warning,
                "Transaction SQL ouverte sur cette base : terminez-la (COMMIT ou ROLLBACK) avant d'appliquer",
            );
            return;
        }
        let production = self
            .data
            .object
            .as_ref()
            .and_then(|o| self.server(&o.connection))
            .is_some_and(|s| s.conn.is_production());
        // Keep the diff view underneath to come back to it on cancel.
        let previous = self.overlay.take().map(Box::new);
        self.overlay = Some(Overlay::ConfirmApply {
            script,
            production,
            input: LineInput::default(),
            counts: self.data.staging.counts(),
            previous,
        });
    }

    pub(super) fn on_confirm_apply_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::ConfirmApply {
            script,
            production,
            mut input,
            counts,
            previous,
        }) = self.overlay.take()
        else {
            return Vec::new();
        };
        let cancel = |app: &mut App| {
            app.overlay = previous.clone().map(|b| *b);
            app.after_apply = None;
            Vec::new()
        };
        let confirmed = if production {
            match input.handle_key(&key) {
                InputOutcome::Cancel => return cancel(self),
                InputOutcome::Submit => {
                    let expected = self.data.object.as_ref().map(|o| o.connection.clone());
                    if Some(input.text().trim()) == expected.as_deref() {
                        true
                    } else {
                        self.notify(
                            Level::Warning,
                            "Nom de connexion incorrect : rien n'a été appliqué",
                        );
                        return cancel(self);
                    }
                }
                _ => false,
            }
        } else {
            match key.code {
                KeyCode::Char('o' | 'O' | 'y' | 'Y') | KeyCode::Enter => true,
                KeyCode::Char('n' | 'N') | KeyCode::Esc => return cancel(self),
                _ => false,
            }
        };
        if !confirmed {
            self.overlay = Some(Overlay::ConfirmApply {
                script,
                production,
                input,
                counts,
                previous,
            });
            return Vec::new();
        }
        self.apply(script)
    }

    fn apply(&mut self, script: Option<String>) -> Vec<Effect> {
        let Some(obj) = self.data.object.clone() else {
            return Vec::new();
        };
        self.request_seq += 1;
        let id = self.request_seq;
        self.data.applying = Some(id);
        self.apply_in_results = self.results_mode.then_some(self.sql.active);
        let (set, origins) = self
            .data
            .staging
            .change_set(obj.clone(), self.data.column_specs());
        self.apply_origins = origins;
        self.status = None;
        let cmd = match script {
            Some(sql) => DbCommand::RunScript {
                id,
                object: obj,
                sql,
            },
            None => DbCommand::ApplyChanges {
                id,
                set: Box::new(set),
            },
        };
        vec![cmd.into()]
    }

    pub(super) fn on_confirm_discard_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.overlay = None;
        if let KeyCode::Char('o' | 'O' | 'y' | 'Y') = key.code {
            let n = self.data.staging.counts().total();
            self.data.staging.discard();
            self.after_staging_change();
            self.notify(
                Level::Info,
                format!("{n} changement(s) abandonné(s) — u pour revenir en arrière"),
            );
        }
        Vec::new()
    }

    pub(super) fn on_pending_changes_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::PendingChanges(after)) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('a' | 's') => {
                self.after_apply = Some(after);
                self.ask_apply(None);
            }
            KeyCode::Char('x') => {
                self.data.staging.clear();
                return self.perform_after(after);
            }
            KeyCode::Char('D') => {
                if let Some(v) = self.changes_view() {
                    self.overlay = Some(Overlay::Changes(Box::new(v)));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => self.overlay = Some(Overlay::PendingChanges(after)),
        }
        Vec::new()
    }

    pub(super) fn on_apply_event(&mut self, ev: DbEvent) -> Vec<Effect> {
        let Some(i) = self.apply_in_results.take() else {
            return self.apply_event(ev);
        };
        // Changes of a SQL result: handled on that result, then the
        // statement runs again to show the new values.
        let prev = self.sql.active;
        self.sql.active = i;
        self.rerun_results = false;
        let effects = self
            .with_results(|app| app.apply_event(ev))
            .unwrap_or_default();
        let statement = self.sql.results.get(i).map(|r| r.statement.clone());
        if self.rerun_results
            && let (Some(stmt), Some(target)) = (statement, self.sql.target.clone())
        {
            self.rerun_results = false;
            let offset = self.sql.editor.text().find(&stmt).unwrap_or(0);
            let mut effects = effects;
            effects.extend(self.rerun(stmt, offset, target));
            return effects;
        }
        self.sql.active = prev.min(self.sql.results.len().saturating_sub(1));
        effects
    }

    fn apply_event(&mut self, ev: DbEvent) -> Vec<Effect> {
        match ev {
            DbEvent::ChangesApplied {
                id,
                counts,
                elapsed,
            } if self.data.applying == Some(id) => {
                self.data.applying = None;
                let counts = counts.unwrap_or_else(|| self.data.staging.counts());
                self.data.staging.clear();
                self.apply_origins.clear();
                if matches!(
                    self.overlay,
                    Some(Overlay::Changes(_) | Overlay::PendingChanges(_))
                ) {
                    self.overlay = None;
                }
                self.last_op = Some(super::LastOp {
                    label: "commit".into(),
                    elapsed,
                });
                self.requery_data();
                self.notify(
                    Level::Success,
                    format!("Appliqué en une transaction : {}", counts.describe()),
                );
                if let Some(after) = self.after_apply.take() {
                    return self.perform_after(after);
                }
            }
            DbEvent::ChangesFailed { id, error } if self.data.applying == Some(id) => {
                self.data.applying = None;
                self.after_apply = None;
                let message = match &error {
                    ApplyError::Conflict { index } => {
                        let origin = self.apply_origins.get(*index).cloned();
                        if let Some(o) = &origin {
                            self.data.staging.mark_conflict(o);
                        }
                        format!(
                            "Conflit sur la ligne {} : elle a été modifiée ou supprimée par ailleurs \
                             depuis sa lecture.\n\n\
                             Rien n'a été écrit (ROLLBACK). Les valeurs actuelles sont rechargées : \
                             la cellule en conflit est soulignée. Rééditez-la (e) pour confirmer votre \
                             valeur, ou annulez le changement (u).",
                            origin.map_or_else(|| "?".into(), |o| self.origin_row(&o))
                        )
                    }
                    ApplyError::Failed {
                        index: Some(i),
                        message,
                    } => format!(
                        "{} a échoué :\n{message}\n\nRien n'a été écrit (ROLLBACK) ; les changements \
                         restent en attente pour correction.",
                        self.apply_origins.get(*i).map_or_else(
                            || format!("Le changement {}", i + 1),
                            |o| {
                                let mut d = self.describe_origin(o);
                                if let Some(first) = d.get_mut(..1) {
                                    first.make_ascii_uppercase();
                                }
                                d
                            }
                        )
                    ),
                    ApplyError::Failed {
                        index: None,
                        message,
                    } => format!(
                        "{message}\n\nRien n'a été écrit (ROLLBACK) ; les changements restent en attente."
                    ),
                };
                self.requery_data();
                self.notify(
                    Level::Error,
                    "Changements non appliqués : rien n'a été écrit",
                );
                self.overlay = Some(Overlay::Error {
                    title: "Changements non appliqués".into(),
                    message,
                });
            }
            _ => {}
        }
        Vec::new()
    }

    /// `id = 2`, or `nouvelle 1`.
    fn origin_row(&self, o: &Origin) -> String {
        match o {
            Origin::Update(k) | Origin::Delete(k) => self
                .data
                .key_columns
                .iter()
                .zip(k)
                .map(|(c, v)| format!("{c} = {v}"))
                .collect::<Vec<_>>()
                .join(", "),
            Origin::Insert(i) => format!("nouvelle {}", self.data.ins().saturating_sub(*i)),
        }
    }

    fn describe_origin(&self, o: &Origin) -> String {
        let key = |k: &Vec<String>| {
            self.data
                .key_columns
                .iter()
                .zip(k)
                .map(|(c, v)| format!("{c} = {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match o {
            Origin::Update(k) => format!("la mise à jour de la ligne {}", key(k)),
            Origin::Delete(k) => format!("la suppression de la ligne {}", key(k)),
            Origin::Insert(i) => format!(
                "l'insertion de la nouvelle ligne {}",
                self.data.ins().saturating_sub(*i)
            ),
        }
    }

    /// Current database value of the cell under the cursor when its staged
    /// change is in conflict.
    pub fn cursor_conflict(&self) -> Option<Option<&str>> {
        let d = &self.data;
        let col = *d.order.get(d.grid.col)?;
        let row = d.row_at(d.grid.row)?;
        matches!(
            d.staging.cell(row, col),
            Some(super::staging::CellChange::Conflict(_))
        )
        .then(|| row.get(col).and_then(|v| v.as_deref()))
    }

    /// Original value of the cell under the cursor when it is edited.
    pub fn cursor_original(&self) -> Option<Option<&str>> {
        let d = &self.data;
        let col = *d.order.get(d.grid.col)?;
        d.staging.original(d.row_at(d.grid.row)?, col)
    }
}
