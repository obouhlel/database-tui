//! Completion popup of the SQL editor (`Ctrl+Space`, `Ctrl+n`): candidates
//! from [`crate::sql::complete`], fed by the tree's catalog (schemas,
//! relations, functions) and by the columns of the target database, loaded
//! once per database.

use std::collections::HashMap;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Effect, Level};
use crate::db::catalog::{ObjectGroup, ObjectKind};
use crate::db::worker::DbCommand;
use crate::sql::complete::{self, Candidate, CompletionCatalog, Relation};

#[derive(Debug, Clone)]
pub struct CompletionPopup {
    /// Bytes replaced by the chosen candidate.
    pub range: Range<usize>,
    pub items: Vec<Candidate>,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default)]
pub enum ColumnsState {
    #[default]
    Loading,
    Loaded {
        columns: Vec<complete::Column>,
        search_path: Vec<String>,
    },
}

/// Columns per (connection, database).
pub type ColumnsCache = HashMap<(String, String), ColumnsState>;

fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table | ObjectKind::PartitionedTable => "table",
        ObjectKind::ForeignTable => "table externe",
        ObjectKind::View => "vue",
        ObjectKind::MaterializedView => "vue matérialisée",
        ObjectKind::Sequence => "séquence",
        ObjectKind::Function => "fonction",
        ObjectKind::Procedure => "procédure",
    }
}

impl App {
    /// What the completion knows about the target database.
    fn completion_catalog(&self, connection: &str, database: &str) -> CompletionCatalog {
        let mut cat = CompletionCatalog {
            search_path: vec!["public".into()],
            ..Default::default()
        };
        if let Some(c) = self.catalog(connection, database) {
            for s in &c.schemas {
                cat.schemas.push(s.name.clone());
                for o in &s.objects {
                    match o.kind.group() {
                        ObjectGroup::Functions => {
                            cat.functions.push((s.name.clone(), o.name.clone()))
                        }
                        ObjectGroup::Sequences => {}
                        _ => cat.relations.push(Relation {
                            schema: s.name.clone(),
                            name: o.name.clone(),
                            kind: kind_label(o.kind),
                        }),
                    }
                }
            }
        }
        if let Some(ColumnsState::Loaded {
            columns,
            search_path,
        }) = self
            .sql
            .columns
            .get(&(connection.to_string(), database.to_string()))
        {
            cat.columns = columns.clone();
            if !search_path.is_empty() {
                cat.search_path = search_path.clone();
            }
        }
        cat
    }

    /// Opens (or refreshes) the popup at the cursor. Returns the commands
    /// to load the columns the first time.
    pub(super) fn complete(&mut self, explicit: bool) -> Vec<Effect> {
        let Some((connection, database)) = self.sql_target() else {
            if explicit {
                self.notify(Level::Info, "Complétion : aucune connexion ouverte");
            }
            return Vec::new();
        };
        let mut effects = Vec::new();
        let key = (connection.clone(), database.clone());
        if let std::collections::hash_map::Entry::Vacant(e) = self.sql.columns.entry(key) {
            e.insert(ColumnsState::Loading);
            effects.push(
                DbCommand::LoadCompletion {
                    connection: connection.clone(),
                    database: database.clone(),
                    include_system: self.config.general.show_system_schemas,
                }
                .into(),
            );
        }
        let cat = self.completion_catalog(&connection, &database);
        let ed = &self.sql.editor;
        let text = ed.text();
        let cursor = ed.offset(ed.cursor);
        let (range, items) = complete::complete(&text, cursor, &cat);
        self.sql.popup = if items.is_empty() {
            if explicit {
                self.notify(Level::Info, "Aucune proposition");
            }
            None
        } else {
            Some(CompletionPopup {
                range,
                items,
                cursor: 0,
            })
        };
        effects
    }

    pub(super) fn on_completion_loaded(
        &mut self,
        connection: String,
        database: String,
        columns: Vec<complete::Column>,
        search_path: Vec<String>,
    ) {
        self.sql.columns.insert(
            (connection, database),
            ColumnsState::Loaded {
                columns,
                search_path,
            },
        );
        if self.sql.popup.is_some() {
            let _ = self.complete(false);
        }
    }

    /// Keys while the popup is open. `None`: not handled (the editor gets
    /// the key, and the popup follows the word being typed).
    pub(super) fn on_completion_key(&mut self, key: &KeyEvent) -> Option<Vec<Effect>> {
        let popup = self.sql.popup.as_mut()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let n = popup.items.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('n') if key.code == KeyCode::Down || ctrl => {
                popup.cursor = (popup.cursor + 1) % n;
            }
            KeyCode::Up | KeyCode::Char('p') if key.code == KeyCode::Up || ctrl => {
                popup.cursor = (popup.cursor + n - 1) % n;
            }
            KeyCode::Tab | KeyCode::Enter => {
                let popup = self.sql.popup.take()?;
                let c = &popup.items[popup.cursor];
                let ed = &mut self.sql.editor;
                ed.anchor = Some(ed.pos_of(popup.range.start));
                ed.cursor = ed.pos_of(popup.range.end);
                if popup.range.is_empty() {
                    ed.anchor = None;
                }
                ed.insert_str(&c.insert);
            }
            KeyCode::Esc => self.sql.popup = None,
            _ => return None,
        }
        Some(Vec::new())
    }

    /// After a key went to the editor with the popup open: refresh it while
    /// a word is typed, close it otherwise.
    pub(super) fn follow_completion(&mut self, key: &KeyEvent) -> Vec<Effect> {
        let word = matches!(key.code, KeyCode::Char(c) if c.is_alphanumeric() || c == '_' || c == '.')
            || key.code == KeyCode::Backspace;
        if word && !key.modifiers.contains(KeyModifiers::CONTROL) {
            self.complete(false)
        } else {
            self.sql.popup = None;
            Vec::new()
        }
    }
}
