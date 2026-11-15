//! Connection lifecycle in the tree: expanding a connection connects it,
//! expanding a database loads its catalog; several connections can be open
//! at once. Also disconnection and deletion of connections.

use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};

use super::input::LineInput;
use super::server::{ConnState, Server};
use super::tree::{DatabaseEntry, NodeKey};
use super::{App, DataView, DetailsState, Effect, LastOp, Level, Overlay, SchemaView};
use crate::db::ConnectRequest;
use crate::db::catalog::{Catalog, DatabaseInfo, ObjectRef};
use crate::db::worker::{DbCommand, DbEvent};

impl App {
    pub fn server(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name() == name)
    }

    pub(super) fn server_mut(&mut self, name: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.name() == name)
    }

    /// Connection of the current object, else of the tree selection.
    pub fn current_server(&self) -> Option<&Server> {
        let name = self
            .current
            .as_ref()
            .map(|o| o.connection.as_str())
            .or_else(|| self.tree.selected_connection())?;
        self.server(name)
    }

    /// Database of the current object, else of the tree selection.
    pub fn current_database(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|o| o.database.as_str())
            .or_else(|| self.tree.selected_database())
            .or_else(|| {
                self.current_server()?
                    .info
                    .as_ref()
                    .map(|i| i.database.as_str())
            })
    }

    /// Catalog of a database, if loaded.
    pub fn catalog(&self, connection: &str, database: &str) -> Option<&Catalog> {
        self.server(connection)?
            .database(database)?
            .catalog
            .as_ref()
    }

    pub(super) fn refresh_tree(&mut self) {
        self.tree.rebuild(&self.servers);
    }

    /// Opens the connection's node in the tree (it connects when expanded).
    pub(super) fn open_connection(&mut self, name: &str) {
        let key = NodeKey::Connection(name.to_string());
        if let Some(s) = self.server_mut(name)
            && matches!(s.state, ConnState::Failed(_) | ConnState::Lost(_))
        {
            s.reset();
        }
        self.tree.expand_node(key.clone());
        self.refresh_tree();
        self.tree.select(&key);
    }

    /// Connects every expanded connection that is not connected yet.
    pub(super) fn sync_connections(&mut self) -> Vec<Effect> {
        let names: Vec<String> = self
            .servers
            .iter()
            .filter(|s| s.state == ConnState::Disconnected)
            .filter(|s| {
                self.tree
                    .is_expanded(&NodeKey::Connection(s.name().to_string()))
            })
            .map(|s| s.name().to_string())
            .collect();
        names
            .into_iter()
            .flat_map(|n| self.connect_server(&n, None, true))
            .collect()
    }

    /// `Connect` for a server; `password` overrides its configured sources.
    pub(super) fn connect_server(
        &mut self,
        name: &str,
        password: Option<String>,
        resolve: bool,
    ) -> Vec<Effect> {
        let general_timeout = self.config.general.clone();
        let Some(s) = self.server_mut(name) else {
            return Vec::new();
        };
        s.reset();
        s.state = ConnState::Connecting(Instant::now());
        let request = ConnectRequest {
            statement_timeout_secs: s.conn.statement_timeout(&general_timeout),
            password: password.or_else(|| s.password.clone()),
            conn: s.conn.clone(),
        };
        self.refresh_tree();
        self.dirty = true;
        vec![
            DbCommand::Connect {
                request: Box::new(request),
                resolve_password: resolve,
            }
            .into(),
        ]
    }

    /// Loads the catalog of every expanded database that has none yet.
    pub(super) fn sync_catalogs(&mut self) -> Vec<Effect> {
        let include_system = self.config.general.show_system_schemas;
        let mut effects = Vec::new();
        for s in self.servers.iter_mut().filter(|s| s.is_connected()) {
            for db in &mut s.databases {
                let key = NodeKey::Database(s.conn.name.clone(), db.name().to_string());
                if self.tree.is_expanded(&key)
                    && db.catalog.is_none()
                    && !db.loading
                    && db.error.is_none()
                    && db.info.can_connect
                {
                    db.loading = true;
                    effects.push(
                        DbCommand::LoadCatalog {
                            connection: s.conn.name.clone(),
                            database: db.name().to_string(),
                            include_system,
                        }
                        .into(),
                    );
                }
            }
        }
        if !effects.is_empty() {
            self.refresh_tree();
            self.dirty = true;
        }
        effects
    }

    /// Reloads every loaded catalog (and retries failed ones).
    pub(super) fn reload_catalog(&mut self) -> Vec<Effect> {
        let include_system = self.config.general.show_system_schemas;
        let mut effects = Vec::new();
        for s in self.servers.iter_mut().filter(|s| s.is_connected()) {
            for db in &mut s.databases {
                if (db.catalog.is_some() || db.error.is_some()) && !db.loading {
                    db.loading = true;
                    db.error = None;
                    effects.push(
                        DbCommand::LoadCatalog {
                            connection: s.conn.name.clone(),
                            database: db.name().to_string(),
                            include_system,
                        }
                        .into(),
                    );
                }
            }
        }
        self.refresh_tree();
        effects
    }

    /// Closes a connection and collapses its node.
    pub(super) fn disconnect(&mut self, name: &str) -> Vec<Effect> {
        if self.guard(super::edit::After::Disconnect(name.to_string())) {
            return Vec::new();
        }
        let Some(s) = self.server_mut(name) else {
            return Vec::new();
        };
        let was_open = s.state != ConnState::Disconnected;
        s.reset();
        self.tree.forget_connection(name);
        self.tree
            .collapse_node(&NodeKey::Connection(name.to_string()));
        if self.current.as_ref().is_some_and(|o| o.connection == name) {
            self.current = None;
            self.schema = SchemaView::default();
            self.data = DataView::default();
        }
        self.back_stack.retain(|o| o.connection != name);
        if self.login.as_ref().is_some_and(|l| l.connection == name) {
            self.login = None;
        }
        self.refresh_tree();
        if was_open {
            self.notify(Level::Info, format!("{name} déconnecté"));
        }
        vec![
            DbCommand::Disconnect {
                connection: name.to_string(),
            }
            .into(),
        ]
    }

    /// `dd` on a connection: asks for confirmation.
    pub(super) fn ask_delete(&mut self, name: &str) {
        if self.guard(super::edit::After::Disconnect(name.to_string())) {
            return;
        }
        self.overlay = Some(Overlay::ConfirmDelete(name.to_string()));
    }

    pub(super) fn on_confirm_delete_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::ConfirmDelete(name)) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('o' | 'O' | 'y' | 'Y') => {
                let saved = self.server(&name).is_some_and(|s| s.saved);
                let mut effects = self.disconnect(&name);
                self.servers.retain(|s| s.name() != name);
                self.refresh_tree();
                if saved {
                    effects.push(Effect::DeleteConnection(name));
                } else {
                    self.notify(Level::Info, format!("« {name} » retiré de la liste"));
                }
                effects
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Vec::new(),
            _ => {
                self.overlay = Some(Overlay::ConfirmDelete(name));
                Vec::new()
            }
        }
    }

    /// Database events concerning connections and catalogs.
    pub(super) fn on_server_event(&mut self, ev: DbEvent) -> Vec<Effect> {
        match ev {
            DbEvent::Progress {
                connection,
                message,
            } => {
                if let Some(s) = self.server_mut(&connection) {
                    s.progress = Some(message);
                }
                self.refresh_tree();
            }
            DbEvent::Connected {
                connection,
                info,
                databases,
                elapsed,
            } => {
                self.last_op = Some(LastOp {
                    label: "connexion".into(),
                    elapsed,
                });
                let primary = info.database.clone();
                let Some(s) = self.server_mut(&connection) else {
                    return Vec::new();
                };
                if !matches!(s.state, ConnState::Connecting(_)) {
                    return Vec::new();
                }
                // The connected database first, then the others by name.
                let mut infos = databases;
                if !infos.iter().any(|d| d.name == primary) {
                    infos.push(DatabaseInfo {
                        name: primary.clone(),
                        comment: None,
                        size_bytes: None,
                        can_connect: true,
                    });
                }
                infos.sort_by_key(|d| (d.name != primary, d.name.clone()));
                s.databases = infos.into_iter().map(DatabaseEntry::new).collect();
                s.state = ConnState::Connected;
                s.progress = None;
                let msg = format!(
                    "Connecté à {connection} (PostgreSQL {}{}) — {} base(s)",
                    info.version,
                    if info.tls { ", TLS" } else { "" },
                    s.databases.len()
                );
                s.info = Some(info);
                let conn = s.conn.clone();
                self.tree
                    .expand_node(NodeKey::Database(connection.clone(), primary));
                self.refresh_tree();
                self.notify(Level::Success, msg);
                self.login_succeeded(&conn);
            }
            DbEvent::ConnectFailed {
                connection,
                error,
                password_required,
            } => {
                let Some(s) = self.server_mut(&connection) else {
                    return Vec::new();
                };
                s.state = ConnState::Failed(error.clone());
                s.progress = None;
                let conn = s.conn.clone();
                self.refresh_tree();
                if self.login_failed(&connection, error.clone()) {
                    return Vec::new();
                }
                if password_required {
                    self.overlay = Some(Overlay::Password {
                        conn: Box::new(conn),
                        input: LineInput::default(),
                        error: Some(error),
                    });
                } else {
                    tracing::warn!(%connection, "connection failed: {error}");
                    self.overlay = Some(Overlay::Error {
                        title: format!("Connexion à {connection} impossible"),
                        message: error,
                    });
                }
            }
            DbEvent::CatalogLoaded { catalog, elapsed } => {
                let count = catalog.object_count();
                self.last_op = Some(LastOp {
                    label: "catalogue".into(),
                    elapsed,
                });
                let (conn_name, db_name) = (catalog.connection.clone(), catalog.database.clone());
                // Drop the current object if it no longer exists.
                if let Some(cur) = self
                    .current
                    .as_ref()
                    .filter(|c| c.connection == conn_name && c.database == db_name)
                {
                    let exists = catalog.schemas.iter().any(|sc| {
                        sc.name == cur.schema
                            && sc
                                .objects
                                .iter()
                                .any(|o| ObjectRef::new(&conn_name, &db_name, &sc.name, o) == *cur)
                    });
                    if !exists {
                        self.current = None;
                        self.schema = SchemaView::default();
                        self.data = DataView::default();
                    }
                }
                let Some(s) = self.server_mut(&conn_name) else {
                    return Vec::new();
                };
                s.details.retain(|o, _| o.database != db_name);
                if let Some(db) = s.databases.iter_mut().find(|d| d.name() == db_name) {
                    db.catalog = Some(catalog);
                    db.loading = false;
                    db.error = None;
                }
                self.refresh_tree();
                tracing::info!(connection = %conn_name, database = %db_name, objects = count, ?elapsed, "catalog loaded");
            }
            DbEvent::CatalogFailed {
                connection,
                database,
                error,
            } => {
                if let Some(db) = self
                    .server_mut(&connection)
                    .and_then(|s| s.databases.iter_mut().find(|d| d.name() == database))
                {
                    db.loading = false;
                    db.error = Some(error.clone());
                }
                self.refresh_tree();
                self.notify(Level::Error, format!("{connection} / {database} : {error}"));
            }
            DbEvent::DetailsLoaded { details, elapsed } => {
                self.last_op = Some(LastOp {
                    label: "détails".into(),
                    elapsed,
                });
                if let Some(s) = self.server_mut(&details.object.connection) {
                    s.details.insert(
                        details.object.clone(),
                        DetailsState::Loaded(Arc::new(*details)),
                    );
                }
            }
            DbEvent::DetailsFailed { object, error } => {
                if let Some(s) = self.server_mut(&object.connection) {
                    s.details.insert(object, DetailsState::Failed(error));
                }
            }
            DbEvent::ConnectionLost {
                connection,
                message,
            } => {
                if let Some(s) = self.server_mut(&connection) {
                    s.state = ConnState::Lost(message.clone());
                }
                self.refresh_tree();
                self.notify(
                    Level::Error,
                    format!(
                        "{connection} : connexion perdue ({message}) — Entrée pour reconnecter"
                    ),
                );
            }
            _ => {}
        }
        Vec::new()
    }
}
