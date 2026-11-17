//! Following references from the Data tab. Part of the reducer: no I/O.
//!
//! - `gd` on a cell: open the table its foreign key points to, filtered on
//!   the cell's value. A polymorphic relation (`when = "kind = 'x'"`) is kept
//!   only if its condition holds on the current row.
//! - `gr` on a cell: list the tables whose foreign keys point to this column,
//!   and open the chosen one filtered on the value.
//! - `Ctrl+o`: back to the previous table, with its filter restored.
//!
//! Declared and virtual foreign keys (`relations.toml`) are treated alike.
//! The filter is built once the target's columns are known (`sync_data`), so
//! an array column (`uuid[]`) is matched with `= ANY(...)`.

use crossterm::event::{KeyCode, KeyEvent};

use super::data_view::DataColumn;
use super::{App, DetailsState, Effect, Level, Overlay, edit};
use crate::db::catalog::ObjectRef;
use crate::db::data::{Condition, Filter, Op};
use crate::db::schema::{FkOrigin, ForeignKey};
use crate::sql::{quote_ident, quote_literal};

/// Where a jump leads: a table, and the rows to show there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    /// Line shown in the `gr` list.
    pub label: String,
    pub target: ObjectRef,
    pub column: String,
    pub value: String,
    /// Extra SQL condition on the target (a polymorphic relation seen from
    /// the referenced side: `subject_kind = 'inscription'`).
    pub when: Option<String>,
}

impl Jump {
    /// The filter for the target, once its columns are known.
    fn filter(&self, columns: &[DataColumn]) -> Result<Filter, String> {
        let col = columns
            .iter()
            .find(|c| c.name == self.column)
            .ok_or_else(|| {
                format!(
                    "colonne « {} » absente de {}",
                    self.column,
                    self.target.qualified()
                )
            })?;
        let ident = quote_ident(&col.name);
        let mut free = Vec::new();
        let mut conditions = Vec::new();
        if let Some(items) = array_items(&self.value) {
            // Source cell is an array: any of its elements.
            let list: Vec<String> = items.iter().map(|v| quote_literal(v)).collect();
            if list.is_empty() {
                return Err("tableau vide : rien à suivre".into());
            }
            free.push(format!("{ident}::text IN ({})", list.join(", ")));
        } else if col.data_type.ends_with("[]") {
            // Target column is an array: rows that contain the value.
            free.push(format!(
                "{} = ANY({ident}::text[])",
                quote_literal(&self.value)
            ));
        } else {
            conditions.push(Condition {
                column: col.name.clone(),
                op: Op::Eq,
                value: self.value.clone(),
            });
        }
        if let Some(w) = &self.when {
            free.push(w.clone());
        }
        Ok(Filter {
            conditions,
            free_where: (!free.is_empty()).then(|| free.join(" AND ")),
            ..Filter::default()
        })
    }
}

/// Elements of a PostgreSQL array literal (`{a,b}`), `None` for a scalar.
/// Ids never need quoting, so quoted elements are only unwrapped.
fn array_items(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    Some(
        inner
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty() && s != "NULL")
            .collect(),
    )
}

/// Evaluates a `column = 'value'` condition against a row; `None` when the
/// condition has another shape or the column is unknown (then it is kept).
fn condition_holds(when: &str, lookup: impl Fn(&str) -> Option<Option<String>>) -> Option<bool> {
    let (col, val) = when.split_once('=')?;
    let col = col.trim().trim_matches('"');
    let val = val.trim().strip_prefix('\'')?.strip_suffix('\'')?;
    if col.is_empty() || col.contains(char::is_whitespace) || val.contains('\'') {
        return None;
    }
    Some(lookup(col)?.as_deref() == Some(val))
}

fn describe(fk: &ForeignKey, other: &ObjectRef, base: &ObjectRef, column: &str) -> String {
    let table = if other.database == base.database {
        other.qualified()
    } else {
        format!("{}:{}", other.database, other.qualified())
    };
    let mut s = format!("{table}.{column}");
    if let FkOrigin::Virtual { kind, when, .. } = &fk.origin {
        s.push_str(&format!("  ~ {kind}"));
        if let Some(w) = when {
            s.push_str(&format!(" (si {w})"));
        }
    }
    s
}

impl App {
    /// Column name and value of the cell under the cursor.
    fn current_cell(&self) -> Result<(String, String), String> {
        let col = self
            .data
            .current_column()
            .ok_or("Aucune colonne sous le curseur")?
            .name
            .clone();
        match self.data.value(self.data.grid.row, self.data.grid.col) {
            None => Err("Ligne pas encore chargée".into()),
            Some(None) => Err(format!("« {col} » est NULL : rien à suivre")),
            Some(Some(v)) => Ok((col, v.to_string())),
        }
    }

    fn loaded_details(&self) -> Option<std::sync::Arc<crate::db::schema::ObjectDetails>> {
        match self.current_details()? {
            DetailsState::Loaded(d) => Some(d.clone()),
            _ => None,
        }
    }

    /// `gd`: the row(s) this cell points to.
    pub(super) fn follow_reference(&mut self) {
        let (column, value) = match self.current_cell() {
            Ok(c) => c,
            Err(e) => return self.notify(Level::Info, e),
        };
        let Some(d) = self.loaded_details() else {
            return;
        };
        let row = self.data.row_at(self.data.grid.row).cloned();
        let lookup = |name: &str| {
            let i = self.data.columns.iter().position(|c| c.name == name)?;
            Some(row.as_ref()?.get(i).cloned().flatten())
        };
        let jumps: Vec<Jump> = d
            .foreign_keys
            .iter()
            .filter(|fk| fk.from_columns == [column.as_str()] && fk.to_columns.len() == 1)
            .filter(|fk| match &fk.origin {
                FkOrigin::Virtual { when: Some(w), .. } => {
                    condition_holds(w, lookup) != Some(false)
                }
                _ => true,
            })
            .map(|fk| Jump {
                label: describe(fk, &fk.to, &d.object, &fk.to_columns[0]),
                target: fk.to.clone(),
                column: fk.to_columns[0].clone(),
                value: value.clone(),
                when: None,
            })
            .collect();
        match jumps.len() {
            0 => self.notify(
                Level::Info,
                format!("« {column} » ne référence aucune table"),
            ),
            1 => self.jump(jumps.into_iter().next().expect("one jump")),
            _ => {
                self.overlay = Some(Overlay::JumpList {
                    title: format!(" {column} = {value} → "),
                    jumps,
                    cursor: 0,
                })
            }
        }
    }

    /// `gr`: the tables that point to this cell's value.
    pub(super) fn show_references(&mut self) {
        let (column, value) = match self.current_cell() {
            Ok(c) => c,
            Err(e) => return self.notify(Level::Info, e),
        };
        let Some(d) = self.loaded_details() else {
            return;
        };
        let jumps: Vec<Jump> = d
            .referenced_by
            .iter()
            .filter(|fk| fk.to_columns == [column.as_str()] && fk.from_columns.len() == 1)
            .map(|fk| Jump {
                label: describe(fk, &fk.from, &d.object, &fk.from_columns[0]),
                target: fk.from.clone(),
                column: fk.from_columns[0].clone(),
                value: value.clone(),
                when: match &fk.origin {
                    FkOrigin::Virtual { when, .. } => when.clone(),
                    FkOrigin::Declared => None,
                },
            })
            .collect();
        if jumps.is_empty() {
            self.notify(
                Level::Info,
                format!("Aucune table ne référence « {column} »"),
            );
        } else {
            self.overlay = Some(Overlay::JumpList {
                title: format!(" ← {column} = {value} "),
                jumps,
                cursor: 0,
            });
        }
    }

    /// Opens the jump's table in the Data tab; the filter is set when its
    /// details arrive (see [`App::take_jump_filter`]).
    pub(super) fn jump(&mut self, jump: Jump) {
        if !self.has_database(&jump.target) {
            return self.notify(
                Level::Warning,
                format!(
                    "Base « {} » absente de la connexion {}",
                    jump.target.database, jump.target.connection
                ),
            );
        }
        let target = jump.target.clone();
        self.pending_jump = Some(jump);
        if self.guard(edit::After::Follow(target.clone())) {
            // Pending changes first; the jump resumes after apply/discard.
            return;
        }
        if let Some(cur) = self.current.take() {
            self.saved_filters
                .insert(cur.clone(), self.data.filter.clone());
            self.back_stack.push(cur);
        }
        self.go_to(target);
    }

    /// Filter for a Data view just created for `obj`: the jump that led
    /// there, else the one it had when left by a jump.
    pub(super) fn take_jump_filter(&mut self, obj: &ObjectRef) -> Option<Filter> {
        if let Some(jump) = self.pending_jump.take()
            && &jump.target == obj
        {
            return match jump.filter(&self.data.columns) {
                Ok(f) => Some(f),
                Err(e) => {
                    self.notify(Level::Warning, e);
                    None
                }
            };
        }
        self.saved_filters.remove(obj)
    }

    pub(super) fn on_jump_list_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::JumpList {
            title,
            jumps,
            mut cursor,
        }) = self.overlay.take()
        else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(jumps.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Enter => {
                let jump = jumps[cursor].clone();
                self.jump(jump);
                return Vec::new();
            }
            _ => {}
        }
        self.overlay = Some(Overlay::JumpList {
            title,
            jumps,
            cursor,
        });
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::value::ValueKind;
    use crate::db::catalog::ObjectKind;

    fn column(name: &str, ty: &str) -> DataColumn {
        DataColumn {
            name: name.into(),
            data_type: ty.into(),
            kind: ValueKind::Text,
            key: false,
            has_default: false,
            server_only: false,
        }
    }

    fn jump(column: &str, value: &str, when: Option<&str>) -> Jump {
        Jump {
            label: String::new(),
            target: ObjectRef {
                connection: "c".into(),
                database: "d".into(),
                schema: "public".into(),
                name: "t".into(),
                kind: ObjectKind::Table,
                signature: None,
            },
            column: column.into(),
            value: value.into(),
            when: when.map(str::to_string),
        }
    }

    #[test]
    fn scalar_value_on_scalar_column_is_a_parameter() {
        let f = jump("id", "abc", None)
            .filter(&[column("id", "uuid")])
            .unwrap();
        assert_eq!(f.conditions.len(), 1);
        assert_eq!(f.conditions[0].value, "abc");
        assert_eq!(f.free_where, None);
    }

    #[test]
    fn arrays_on_either_side() {
        let f = jump("ids", "abc", None)
            .filter(&[column("ids", "uuid[]")])
            .unwrap();
        assert_eq!(f.free_where.as_deref(), Some("'abc' = ANY(ids::text[])"));
        let f = jump("id", "{a,b}", None)
            .filter(&[column("id", "uuid")])
            .unwrap();
        assert_eq!(f.free_where.as_deref(), Some("id::text IN ('a', 'b')"));
        assert!(
            jump("id", "{}", None)
                .filter(&[column("id", "uuid")])
                .is_err()
        );
    }

    #[test]
    fn condition_of_a_polymorphic_relation_is_added() {
        let f = jump("subject_id", "x", Some("subject_kind = 'inscription'"))
            .filter(&[column("subject_id", "uuid")])
            .unwrap();
        assert_eq!(f.conditions.len(), 1);
        assert_eq!(
            f.free_where.as_deref(),
            Some("subject_kind = 'inscription'")
        );
    }

    #[test]
    fn unknown_column_is_an_error() {
        assert!(
            jump("nope", "x", None)
                .filter(&[column("id", "uuid")])
                .is_err()
        );
    }

    #[test]
    fn conditions_are_checked_against_the_row() {
        let row = |c: &str| match c {
            "client_type" => Some(Some("entreprise".to_string())),
            "vide" => Some(None),
            _ => None,
        };
        assert_eq!(
            condition_holds("client_type = 'entreprise'", row),
            Some(true)
        );
        assert_eq!(
            condition_holds("client_type = 'candidat'", row),
            Some(false)
        );
        assert_eq!(condition_holds("vide = 'x'", row), Some(false));
        // Unknown column or other shapes: undecided, the relation is kept.
        assert_eq!(condition_holds("autre = 'x'", row), None);
        assert_eq!(condition_holds("client_type IN ('a')", row), None);
    }

    #[test]
    fn array_literals() {
        assert_eq!(
            array_items("{a,\"b\",NULL}"),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(array_items("abc"), None);
    }
}
