//! `K` on a foreign key cell: a popup with the referenced row.
//! The worker resolves the foreign key from the table's details, so it also
//! works on SQL results that come from a table.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Effect, Level, Overlay};
use crate::db::worker::{DbCommand, DbEvent, Reference};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferencePeek {
    pub id: u64,
    pub column: String,
    pub value: String,
    /// `None` while loading.
    pub result: Option<Result<Box<Reference>, String>>,
    pub scroll: u16,
}

impl App {
    pub(super) fn peek_reference(&mut self) -> Vec<Effect> {
        let d = &self.data;
        let Some(object) = d.object.clone() else {
            self.notify(Level::Info, "Résultat non lié à une table");
            return Vec::new();
        };
        let Some(&col) = d.order.get(d.grid.col) else {
            return Vec::new();
        };
        let column = d.columns[col].name.clone();
        let row: Vec<(String, Option<String>)> = if d.grid.row < d.ins() {
            let values = &d.staging.state.inserts[d.grid.row];
            d.columns
                .iter()
                .zip(values)
                .map(|(c, v)| {
                    let text = match v {
                        crate::db::changes::NewValue::Text(t) => Some(t.clone()),
                        _ => None,
                    };
                    (c.name.clone(), text)
                })
                .collect()
        } else {
            let Some(r) = d.row_at(d.grid.row) else {
                self.notify(Level::Info, "Ligne non chargée");
                return Vec::new();
            };
            // Staged values win: the popup shows what the row will point to.
            let staged = d
                .staging
                .key_of(r)
                .and_then(|k| d.staging.state.updates.get(&k));
            d.columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let v = match staged.and_then(|e| e.cells.get(&i)) {
                        Some(crate::db::changes::NewValue::Text(t)) => Some(t.clone()),
                        Some(_) => None,
                        None => r.get(i).cloned().flatten(),
                    };
                    (c.name.clone(), v)
                })
                .collect()
        };
        let Some(value) = row[col].1.clone() else {
            self.notify(Level::Info, format!("{column} est NULL"));
            return Vec::new();
        };
        let id = self.next_request_id();
        self.overlay = Some(Overlay::Reference(Box::new(ReferencePeek {
            id,
            column: column.clone(),
            value,
            result: None,
            scroll: 0,
        })));
        vec![
            DbCommand::FetchReference {
                id,
                object,
                column,
                row,
            }
            .into(),
        ]
    }

    pub(super) fn on_reference_event(&mut self, ev: DbEvent) {
        let DbEvent::ReferenceLoaded { id, result } = ev else {
            return;
        };
        if let Some(Overlay::Reference(p)) = &mut self.overlay
            && p.id == id
        {
            p.result = Some(result);
        }
    }

    pub(super) fn on_reference_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Reference(mut p)) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('K') => {
                return Vec::new();
            }
            KeyCode::Char('j') | KeyCode::Down => p.scroll = p.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => p.scroll = p.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => p.scroll = p.scroll.saturating_add(10),
            KeyCode::PageUp => p.scroll = p.scroll.saturating_sub(10),
            KeyCode::Char('g') => p.scroll = 0,
            KeyCode::Char('y') => {
                if let Some(Ok(r)) = &p.result
                    && let Some(row) = &r.row
                {
                    let map: serde_json::Map<String, serde_json::Value> = r
                        .columns
                        .iter()
                        .zip(row)
                        .map(|((n, _), v)| {
                            (
                                n.clone(),
                                v.clone()
                                    .map_or(serde_json::Value::Null, serde_json::Value::String),
                            )
                        })
                        .collect();
                    let text = serde_json::to_string_pretty(&map).unwrap_or_default();
                    self.overlay = Some(Overlay::Reference(p));
                    self.notify(Level::Success, "Ligne référencée copiée en JSON");
                    return vec![Effect::Copy(text)];
                }
            }
            _ => {}
        }
        self.overlay = Some(Overlay::Reference(p));
        Vec::new()
    }
}
