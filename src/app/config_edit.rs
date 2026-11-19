//! Adding and changing connections by editing `config.toml` in the user's
//! editor (`a`, `e`, `:add`, `:config`), then testing them (`T`, `:test`).
//!
//! The reducer only asks for the editor ([`App::edit_request`]); the event
//! loop suspends the terminal, runs it and answers with
//! [`Msg::ConfigEdited`](super::Msg::ConfigEdited).

use crossterm::event::{KeyCode, KeyEvent};

use super::server::Server;
use super::{App, Effect, Level, Overlay};
use crate::config::ConnectionConfig;
use crate::db::probe::{self, ProbeOk};
use crate::db::worker::{DbCommand, DbEvent};

/// What the event loop opens in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// Append a template block for a new connection.
    pub add: bool,
    /// Place the cursor on this connection's block.
    pub connection: Option<String>,
    /// Or on this line (a syntax error).
    pub line: Option<usize>,
}

/// Outcome of an editor session.
#[derive(Debug, Clone)]
pub enum ConfigEdit {
    /// The file parses: its connections.
    Saved(Vec<ConnectionConfig>),
    /// The file no longer parses (it is left as written, the application
    /// keeps the previous connections).
    Invalid {
        message: String,
        line: Option<usize>,
    },
    /// The template was left untouched and removed again.
    NothingAdded,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRow {
    pub id: u64,
    pub name: String,
    pub target: String,
    /// `None` while the test runs.
    pub result: Option<Result<ProbeOk, String>>,
}

/// The connection test popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeView {
    pub title: String,
    pub rows: Vec<ProbeRow>,
    pub cursor: usize,
}

impl ProbeView {
    pub fn done(&self) -> bool {
        self.rows.iter().all(|r| r.result.is_some())
    }

    pub fn failed(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.result, Some(Err(_))))
            .count()
    }
}

impl App {
    /// `a` / `:add`: the editor on a new connection block.
    pub(super) fn ask_add_connection(&mut self) {
        self.request_edit(EditRequest {
            add: true,
            connection: None,
            line: None,
        });
    }

    /// `e` / `:config`: the editor on the selected connection (if any).
    pub(super) fn ask_edit_config(&mut self, connection: Option<String>) {
        let connection = connection.filter(|n| self.server(n).is_some_and(|s| s.saved));
        self.request_edit(EditRequest {
            add: false,
            connection,
            line: None,
        });
    }

    fn request_edit(&mut self, req: EditRequest) {
        if self.config_path.is_none() {
            self.notify(Level::Error, "Fichier de configuration inconnu");
            return;
        }
        self.overlay = None;
        self.edit_request = Some(req);
    }

    /// The event loop is back from the editor.
    pub(super) fn on_config_edited(&mut self, edit: ConfigEdit) -> Vec<Effect> {
        match edit {
            ConfigEdit::Saved(conns) => self.apply_connections(conns),
            ConfigEdit::Invalid { message, line } => {
                self.overlay = Some(Overlay::ConfigInvalid { message, line });
                Vec::new()
            }
            ConfigEdit::NothingAdded => {
                self.notify(Level::Info, "Aucune connexion ajoutée");
                Vec::new()
            }
            ConfigEdit::Failed(e) => {
                self.notify(Level::Error, e);
                Vec::new()
            }
        }
    }

    /// Replaces the configured connections; the added and changed ones are
    /// tested. A changed connection that was open is closed first.
    fn apply_connections(&mut self, conns: Vec<ConnectionConfig>) -> Vec<Effect> {
        let mut effects = Vec::new();
        let old: Vec<ConnectionConfig> = self
            .servers
            .iter()
            .filter(|s| s.saved)
            .map(|s| s.conn.clone())
            .collect();
        let removed: Vec<String> = old
            .iter()
            .filter(|o| !conns.iter().any(|c| c.name == o.name))
            .map(|o| o.name.clone())
            .collect();
        let changed: Vec<String> = conns
            .iter()
            .filter(|c| old.iter().any(|o| o.name == c.name && *o != **c))
            .map(|c| c.name.clone())
            .collect();
        let added: Vec<String> = conns
            .iter()
            .filter(|c| !old.iter().any(|o| o.name == c.name))
            .map(|c| c.name.clone())
            .collect();
        for name in removed.iter().chain(&changed) {
            let open = self
                .server(name)
                .is_some_and(|s| s.state != super::server::ConnState::Disconnected);
            if open {
                effects.extend(self.disconnect(name));
            }
        }
        // Command-line connections first, then the file's order.
        let mut servers: Vec<Server> = std::mem::take(&mut self.servers);
        let mut next: Vec<Server> = Vec::new();
        next.extend(servers.iter().filter(|s| !s.saved).cloned());
        for c in &conns {
            match servers
                .iter()
                .position(|s| s.saved && s.conn.name == c.name)
            {
                Some(i) if !changed.contains(&c.name) => next.push(servers.swap_remove(i)),
                _ => next.push(Server::new(c.clone(), None, true)),
            }
        }
        self.servers = next;
        self.config.connections = conns;
        self.picker_cursor = self.picker_cursor.min(self.picker_len().saturating_sub(1));
        self.refresh_tree();

        let to_test: Vec<String> = added.iter().chain(&changed).cloned().collect();
        if to_test.is_empty() {
            let msg = if removed.is_empty() {
                "Connexions inchangées".to_string()
            } else {
                format!("{} connexion(s) retirée(s)", removed.len())
            };
            self.notify(Level::Info, msg);
            return effects;
        }
        let mut title = Vec::new();
        if !added.is_empty() {
            title.push(format!("{} ajoutée(s)", added.len()));
        }
        if !changed.is_empty() {
            title.push(format!("{} modifiée(s)", changed.len()));
        }
        if !removed.is_empty() {
            title.push(format!("{} retirée(s)", removed.len()));
        }
        effects.extend(self.start_probes(&to_test, format!("Connexions {}", title.join(", "))));
        effects
    }

    /// `T` / `:test`: tests every configured connection.
    pub(super) fn test_all_connections(&mut self) -> Vec<Effect> {
        let names: Vec<String> = self.servers.iter().map(|s| s.name().to_string()).collect();
        if names.is_empty() {
            self.notify(Level::Info, "Aucune connexion à tester");
            return Vec::new();
        }
        self.start_probes(&names, "Test des connexions".into())
    }

    fn start_probes(&mut self, names: &[String], title: String) -> Vec<Effect> {
        let mut rows = Vec::new();
        let mut effects = Vec::new();
        for name in names {
            let Some(s) = self.server(name) else { continue };
            let mut conn = s.conn.clone();
            // A password given on the command line (`--url`) is tested too.
            if conn.password.is_none() {
                conn.password = s.password.clone();
            }
            let id = self.next_request_id();
            rows.push(ProbeRow {
                id,
                name: name.clone(),
                target: probe::describe(&conn),
                result: None,
            });
            effects.push(
                DbCommand::Probe {
                    id,
                    conn: Box::new(conn),
                }
                .into(),
            );
        }
        self.overlay = Some(Overlay::Probes(Box::new(ProbeView {
            title,
            rows,
            cursor: 0,
        })));
        effects
    }

    pub(super) fn on_probe_event(&mut self, ev: DbEvent) {
        let DbEvent::Probed { id, result } = ev else {
            return;
        };
        if let Some(Overlay::Probes(v)) = &mut self.overlay
            && let Some(row) = v.rows.iter_mut().find(|r| r.id == id)
        {
            row.result = Some(result);
            if v.done() {
                let failed = v.failed();
                let n = v.rows.len();
                if failed == 0 {
                    self.notify(Level::Success, format!("{n}/{n} connexion(s) OK"));
                } else {
                    self.notify(
                        Level::Error,
                        format!("{failed} connexion(s) en échec sur {n}"),
                    );
                }
            }
        }
    }

    pub(super) fn on_probes_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Probes(mut v)) = self.overlay.take() else {
            return Vec::new();
        };
        let selected = v.rows.get(v.cursor).map(|r| r.name.clone());
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Enter => {
                // Open the connection in the tree.
                if let Some(name) = selected {
                    self.screen = super::Screen::Main;
                    self.focus = super::Focus::Tree;
                    self.open_connection(&name);
                }
                return Vec::new();
            }
            KeyCode::Char('e') => {
                self.ask_edit_config(selected);
                return Vec::new();
            }
            KeyCode::Char('a') => {
                self.ask_add_connection();
                return Vec::new();
            }
            KeyCode::Char('r') | KeyCode::Char('T') => {
                let names: Vec<String> = v.rows.iter().map(|r| r.name.clone()).collect();
                return self.start_probes(&names, v.title.clone());
            }
            KeyCode::Char('j') | KeyCode::Down => {
                v.cursor = (v.cursor + 1).min(v.rows.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => v.cursor = v.cursor.saturating_sub(1),
            _ => {}
        }
        self.overlay = Some(Overlay::Probes(v));
        Vec::new()
    }

    pub(super) fn on_config_invalid_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::ConfigInvalid { message, line }) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('e') | KeyCode::Enter => self.request_edit(EditRequest {
                add: false,
                connection: None,
                line,
            }),
            KeyCode::Esc | KeyCode::Char('q') => self.notify(
                Level::Warning,
                "config.toml invalide : les connexions actuelles sont conservées",
            ),
            _ => self.overlay = Some(Overlay::ConfigInvalid { message, line }),
        }
        Vec::new()
    }
}
