//! Changes staged in the Data tab, for one table at a time.
//!
//! Existing rows are identified by the values of their key columns (primary
//! key or NOT NULL unique key), so staged edits survive reloads, sorting and
//! scrolling. Nothing is written until the change set is applied.
//!
//! Undo / redo keep snapshots of the whole state: it is small (only the
//! edited rows), and every operation becomes trivially reversible.

use std::collections::BTreeMap;

use crate::db::catalog::ObjectRef;
use crate::db::changes::{Change, ChangeCounts, ChangeSet, NewValue};
use crate::db::data::ColumnSpec;

/// Key column values of a row.
pub type RowKey = Vec<String>;

const MAX_UNDO: usize = 200;

/// An existing row with staged edits (or marked for deletion).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowEdit {
    /// Values as read (one per column), the base of conflict detection.
    pub original: Vec<Option<String>>,
    /// Row version (`xmin`) as read, if the query returned it.
    pub version: Option<String>,
    /// New values by column index.
    pub cells: BTreeMap<usize, NewValue>,
    /// The last apply found the row changed since it was read.
    pub conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StagedState {
    pub updates: BTreeMap<RowKey, RowEdit>,
    pub deletes: BTreeMap<RowKey, RowEdit>,
    /// New rows, one value per column.
    pub inserts: Vec<Vec<NewValue>>,
}

/// What a change of the change set refers to (to report errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Update(RowKey),
    Insert(usize),
    Delete(RowKey),
}

/// Staged state of a cell, for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellChange<'a> {
    Modified(&'a NewValue),
    Conflict(&'a NewValue),
    Deleted,
}

#[derive(Debug, Clone, Default)]
pub struct Staging {
    pub state: StagedState,
    undo: Vec<StagedState>,
    redo: Vec<StagedState>,
    /// Indices of the key columns.
    key_columns: Vec<usize>,
    ncols: usize,
}

impl Staging {
    pub fn new(key_columns: Vec<usize>, ncols: usize) -> Self {
        Self {
            key_columns,
            ncols,
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.state.updates.is_empty()
            && self.state.deletes.is_empty()
            && self.state.inserts.is_empty()
    }

    pub fn counts(&self) -> ChangeCounts {
        let s = &self.state;
        ChangeCounts {
            updated: s
                .updates
                .keys()
                .filter(|k| !s.deletes.contains_key(*k))
                .count() as u64,
            inserted: s.inserts.len() as u64,
            deleted: s.deletes.len() as u64,
        }
    }

    pub fn inserts(&self) -> usize {
        self.state.inserts.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Key of a loaded row (`None` if a key value is NULL: not editable).
    pub fn key_of(&self, row: &[Option<String>]) -> Option<RowKey> {
        self.key_columns
            .iter()
            .map(|i| row.get(*i).cloned().flatten())
            .collect()
    }

    fn snapshot(&mut self) {
        self.undo.push(self.state.clone());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn base(&self, row: &[Option<String>]) -> RowEdit {
        RowEdit {
            original: row.iter().take(self.ncols).cloned().collect(),
            version: row.get(self.ncols).cloned().flatten(),
            ..Default::default()
        }
    }

    /// Stages `value` for column `col` of a loaded row. Setting a cell back
    /// to its original value removes the edit. Returns `false` if the row
    /// has no usable key.
    pub fn set_cell(&mut self, row: &[Option<String>], col: usize, value: NewValue) -> bool {
        let Some(key) = self.key_of(row) else {
            return false;
        };
        self.snapshot();
        let base = self.base(row);
        let edit = self
            .state
            .updates
            .entry(key.clone())
            .or_insert_with(|| base.clone());
        if edit.conflict {
            // The user saw the current values: they become the new base.
            edit.original = base.original;
            edit.version = base.version;
            edit.conflict = false;
        }
        let unchanged =
            NewValue::from_text(edit.original.get(col).and_then(|v| v.as_deref())) == value;
        if unchanged {
            edit.cells.remove(&col);
        } else {
            edit.cells.insert(col, value);
        }
        if edit.cells.is_empty() {
            self.state.updates.remove(&key);
        }
        true
    }

    pub fn set_insert_cell(&mut self, index: usize, col: usize, value: NewValue) {
        if index < self.state.inserts.len() && col < self.ncols {
            self.snapshot();
            self.state.inserts[index][col] = value;
        }
    }

    /// Adds an empty row (every column `DEFAULT`) and returns its index.
    pub fn insert_row(&mut self) -> usize {
        self.snapshot();
        self.state
            .inserts
            .insert(0, vec![NewValue::Default; self.ncols]);
        0
    }

    pub fn remove_insert(&mut self, index: usize) {
        if index < self.state.inserts.len() {
            self.snapshot();
            self.state.inserts.remove(index);
        }
    }

    /// Marks a loaded row for deletion, or unmarks it. Returns the new
    /// state (`Some(true)`: marked), `None` without a usable key.
    pub fn toggle_delete(&mut self, row: &[Option<String>]) -> Option<bool> {
        let key = self.key_of(row)?;
        self.snapshot();
        if self.state.deletes.remove(&key).is_some() {
            return Some(false);
        }
        let base = self.base(row);
        self.state.deletes.insert(key, base);
        Some(true)
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.state, prev));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut self.state, next));
                true
            }
            None => false,
        }
    }

    /// Drops every staged change (undoable).
    pub fn discard(&mut self) {
        if !self.is_empty() {
            self.snapshot();
            self.state = StagedState::default();
        }
    }

    /// After a successful apply: nothing left, no history.
    pub fn clear(&mut self) {
        *self = Self::new(std::mem::take(&mut self.key_columns), self.ncols);
    }

    pub fn mark_conflict(&mut self, origin: &Origin) {
        match origin {
            Origin::Update(k) => {
                if let Some(e) = self.state.updates.get_mut(k) {
                    e.conflict = true;
                }
            }
            Origin::Delete(k) => {
                if let Some(e) = self.state.deletes.get_mut(k) {
                    e.conflict = true;
                }
            }
            Origin::Insert(_) => {}
        }
    }

    /// Staged state of column `col` of a loaded row.
    pub fn cell(&self, row: &[Option<String>], col: usize) -> Option<CellChange<'_>> {
        if self.is_empty() {
            return None;
        }
        let key = self.key_of(row)?;
        if self.state.deletes.contains_key(&key) {
            return Some(CellChange::Deleted);
        }
        let edit = self.state.updates.get(&key)?;
        edit.cells.get(&col).map(|v| {
            if edit.conflict {
                CellChange::Conflict(v)
            } else {
                CellChange::Modified(v)
            }
        })
    }

    /// Original value of an edited cell (shown under the grid).
    pub fn original(&self, row: &[Option<String>], col: usize) -> Option<Option<&str>> {
        let edit = self.state.updates.get(&self.key_of(row)?)?;
        edit.cells
            .contains_key(&col)
            .then(|| edit.original.get(col).and_then(|v| v.as_deref()))
    }

    /// The change set to apply: deletes, then updates, then inserts. Also
    /// returns what each change refers to.
    pub fn change_set(
        &self,
        object: ObjectRef,
        columns: Vec<ColumnSpec>,
    ) -> (ChangeSet, Vec<Origin>) {
        let name = |i: usize| columns[i].name.clone();
        let key_pairs = |key: &RowKey| -> Vec<(String, String)> {
            self.key_columns
                .iter()
                .zip(key)
                .map(|(i, v)| (name(*i), v.clone()))
                .collect()
        };
        let mut changes = Vec::new();
        let mut origins = Vec::new();
        for (key, edit) in &self.state.deletes {
            changes.push(Change::Delete {
                key: key_pairs(key),
                expect_version: edit.version.clone(),
            });
            origins.push(Origin::Delete(key.clone()));
        }
        for (key, edit) in &self.state.updates {
            if self.state.deletes.contains_key(key) {
                continue;
            }
            changes.push(Change::Update {
                key: key_pairs(key),
                set: edit
                    .cells
                    .iter()
                    .map(|(i, v)| (name(*i), v.clone()))
                    .collect(),
                expect: edit
                    .cells
                    .keys()
                    .map(|i| (name(*i), edit.original.get(*i).cloned().flatten()))
                    .collect(),
            });
            origins.push(Origin::Update(key.clone()));
        }
        // Inserts are shown newest first; insert them in creation order.
        for (i, row) in self.state.inserts.iter().enumerate().rev() {
            changes.push(Change::Insert {
                values: row
                    .iter()
                    .enumerate()
                    .map(|(c, v)| (name(c), v.clone()))
                    .collect(),
            });
            origins.push(Origin::Insert(i));
        }
        (
            ChangeSet {
                object,
                columns,
                changes,
            },
            origins,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::catalog::ObjectKind;
    use pretty_assertions::assert_eq;

    fn row(id: &str, name: Option<&str>) -> Vec<Option<String>> {
        // id, name, then the row version.
        vec![
            Some(id.into()),
            name.map(str::to_string),
            Some("900".into()),
        ]
    }

    fn staging() -> Staging {
        Staging::new(vec![0], 2)
    }

    fn columns() -> Vec<ColumnSpec> {
        ["id", "name"]
            .iter()
            .map(|n| ColumnSpec {
                name: n.to_string(),
                data_type: "text".into(),
            })
            .collect()
    }

    fn obj() -> ObjectRef {
        ObjectRef {
            connection: "c".into(),
            database: "d".into(),
            schema: "public".into(),
            name: "t".into(),
            kind: ObjectKind::Table,
            signature: None,
        }
    }

    #[test]
    fn edit_then_revert_to_original() {
        let mut s = staging();
        let r = row("1", Some("a"));
        assert!(s.set_cell(&r, 1, NewValue::Text("b".into())));
        assert_eq!(
            s.cell(&r, 1),
            Some(CellChange::Modified(&NewValue::Text("b".into())))
        );
        assert_eq!(s.original(&r, 1), Some(Some("a")));
        assert_eq!(s.counts().updated, 1);
        s.set_cell(&r, 1, NewValue::Text("a".into()));
        assert!(s.is_empty(), "back to the original value");
        // A row without key cannot be edited.
        assert!(!s.set_cell(&[None, None], 1, NewValue::Null));
    }

    #[test]
    fn undo_redo() {
        let mut s = staging();
        let r = row("1", Some("a"));
        s.set_cell(&r, 1, NewValue::Null);
        s.toggle_delete(&row("2", None));
        s.insert_row();
        assert_eq!(
            s.counts(),
            ChangeCounts {
                updated: 1,
                inserted: 1,
                deleted: 1
            }
        );
        assert!(s.undo());
        assert_eq!(s.counts().inserted, 0);
        assert!(s.undo());
        assert!(s.undo());
        assert!(s.is_empty() && !s.undo());
        assert!(s.redo());
        assert_eq!(s.counts().updated, 1);
        // A new change clears the redo history.
        s.discard();
        assert!(!s.redo());
        assert!(s.undo());
        assert_eq!(s.counts().updated, 1);
    }

    #[test]
    fn delete_wins_over_update() {
        let mut s = staging();
        let r = row("1", Some("a"));
        s.set_cell(&r, 1, NewValue::Text("b".into()));
        assert_eq!(s.toggle_delete(&r), Some(true));
        assert_eq!(s.cell(&r, 1), Some(CellChange::Deleted));
        assert_eq!(
            s.counts(),
            ChangeCounts {
                updated: 0,
                inserted: 0,
                deleted: 1
            }
        );
        let (set, origins) = s.change_set(obj(), columns());
        assert_eq!(set.changes.len(), 1);
        assert_eq!(origins, vec![Origin::Delete(vec!["1".into()])]);
        assert_eq!(s.toggle_delete(&r), Some(false));
        assert_eq!(s.counts().updated, 1, "the edit comes back");
    }

    #[test]
    fn change_set_order_and_content() {
        let mut s = staging();
        s.set_cell(&row("1", Some("a")), 1, NewValue::Text("b".into()));
        s.toggle_delete(&row("2", Some("x")));
        s.insert_row();
        s.set_insert_cell(0, 1, NewValue::Text("first".into()));
        s.insert_row();
        let (set, origins) = s.change_set(obj(), columns());
        assert_eq!(
            set.changes,
            vec![
                Change::Delete {
                    key: vec![("id".into(), "2".into())],
                    expect_version: Some("900".into())
                },
                Change::Update {
                    key: vec![("id".into(), "1".into())],
                    set: vec![("name".into(), NewValue::Text("b".into()))],
                    expect: vec![("name".into(), Some("a".into()))],
                },
                Change::Insert {
                    values: vec![
                        ("id".into(), NewValue::Default),
                        ("name".into(), NewValue::Text("first".into()))
                    ]
                },
                Change::Insert {
                    values: vec![
                        ("id".into(), NewValue::Default),
                        ("name".into(), NewValue::Default)
                    ]
                },
            ]
        );
        assert_eq!(origins[2], Origin::Insert(1));
    }

    #[test]
    fn conflict_then_reedit_takes_the_new_base() {
        let mut s = staging();
        s.set_cell(&row("1", Some("a")), 1, NewValue::Text("b".into()));
        s.mark_conflict(&Origin::Update(vec!["1".into()]));
        let fresh = row("1", Some("changed elsewhere"));
        assert!(matches!(s.cell(&fresh, 1), Some(CellChange::Conflict(_))));
        s.set_cell(&fresh, 1, NewValue::Text("b".into()));
        let edit = &s.state.updates[&vec!["1".to_string()]];
        assert!(!edit.conflict);
        assert_eq!(edit.original[1].as_deref(), Some("changed elsewhere"));
    }
}
