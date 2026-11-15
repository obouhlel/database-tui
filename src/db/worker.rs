//! The DB worker: a tokio task that owns the backends. The UI sends
//! [`DbCommand`]s and receives [`DbEvent`]s; it never awaits a query itself.
//!
//! Several connections (servers) can be open at once, each identified by its
//! name. For each one, the database chosen at connection time is opened
//! first, the others lazily (when the tree expands them or an object of
//! theirs is opened), with the same credentials and through the same tunnel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, OnceCell};

use super::catalog::{Catalog, DatabaseInfo, ObjectRef};
use super::changes::{ApplyError, ChangeCounts, ChangeSet};
use super::data::{DataRequest, RowCount, TextRows};
use super::discover::{self, DiscoveredServer};
use super::schema::ObjectDetails;
use super::{Backend, ConnectError, ConnectRequest, ServerInfo};
use crate::config::password;

#[derive(Debug)]
pub enum DbCommand {
    /// Opens the connection `request.conn.name`, replacing it if already
    /// open. With `resolve_password`, the configured password sources are
    /// tried when `request.password` is empty.
    Connect {
        request: Box<ConnectRequest>,
        resolve_password: bool,
    },
    LoadCatalog {
        connection: String,
        database: String,
        include_system: bool,
    },
    LoadDetails(ObjectRef),
    /// Fetch a window of rows; `id` is echoed back to discard stale results.
    FetchRows {
        id: u64,
        request: Box<DataRequest>,
    },
    CountRows {
        id: u64,
        request: Box<DataRequest>,
        bound: u64,
    },
    /// Apply staged changes in one transaction.
    ApplyChanges {
        id: u64,
        set: Box<ChangeSet>,
    },
    /// Run a user-edited script (the generated SQL) in one transaction.
    RunScript {
        id: u64,
        object: ObjectRef,
        sql: String,
    },
    /// Cancel the query running on the session connection of a database.
    Cancel {
        connection: String,
        database: String,
    },
    /// Probe the local machine for PostgreSQL servers.
    Discover {
        ports: Vec<u16>,
    },
    Disconnect {
        connection: String,
    },
}

#[derive(Debug)]
pub enum DbEvent {
    Connected {
        connection: String,
        info: ServerInfo,
        /// Databases of the server (the connected one included).
        databases: Vec<DatabaseInfo>,
        elapsed: Duration,
    },
    ConnectFailed {
        connection: String,
        error: String,
        password_required: bool,
    },
    /// `catalog.connection` names the connection.
    CatalogLoaded {
        catalog: Catalog,
        elapsed: Duration,
    },
    CatalogFailed {
        connection: String,
        database: String,
        error: String,
    },
    DetailsLoaded {
        details: Box<ObjectDetails>,
        elapsed: Duration,
    },
    DetailsFailed {
        object: ObjectRef,
        error: String,
    },
    RowsLoaded {
        id: u64,
        offset: u64,
        rows: TextRows,
        elapsed: Duration,
    },
    RowsFailed {
        id: u64,
        error: String,
    },
    RowsCounted {
        id: u64,
        count: RowCount,
    },
    CountFailed {
        id: u64,
        error: String,
    },
    /// `counts` is `None` for a script.
    ChangesApplied {
        id: u64,
        counts: Option<ChangeCounts>,
        elapsed: Duration,
    },
    ChangesFailed {
        id: u64,
        error: ApplyError,
    },
    Discovered(Vec<DiscoveredServer>),
    /// A step of a long operation (opening a tunnel, connecting).
    Progress {
        connection: String,
        message: String,
    },
    ConnectionLost {
        connection: String,
        message: String,
    },
}

/// Spawns the worker on the current tokio runtime.
pub fn spawn(events: UnboundedSender<DbEvent>) -> UnboundedSender<DbCommand> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(run(rx, events));
    tx
}

/// A backend opened at most once, even when several requests race for it.
type LazyBackend = Arc<OnceCell<Arc<dyn Backend>>>;

/// One backend per database of a server, opened on demand.
struct Pool {
    template: ConnectRequest,
    on_lost: super::postgres::OnConnectionLost,
    backends: Mutex<HashMap<String, LazyBackend>>,
    /// Shared by every database of the server; killed when the pool is dropped.
    tunnel: Option<std::sync::Mutex<super::tunnel::Tunnel>>,
}

impl Pool {
    async fn get(&self, database: &str) -> Result<Arc<dyn Backend>, ConnectError> {
        let cell = self
            .backends
            .lock()
            .await
            .entry(database.to_string())
            .or_default()
            .clone();
        cell.get_or_try_init(|| async {
            let mut req = self.template.clone();
            req.conn.database = Some(database.to_string());
            tracing::info!(connection = %req.conn.name, database, "opening database");
            super::connect(&req, self.on_lost.clone()).await
        })
        .await
        .cloned()
    }
}

/// Open connections. The generation is bumped on every (dis)connection so
/// that late results and notifications of a replaced session are ignored.
#[derive(Default)]
struct Pools {
    generation: HashMap<String, u64>,
    open: HashMap<String, Arc<Pool>>,
}

type SharedPools = Arc<std::sync::Mutex<Pools>>;

impl Pools {
    fn bump(&mut self, connection: &str) -> u64 {
        self.open.remove(connection);
        let g = self.generation.entry(connection.to_string()).or_default();
        *g += 1;
        *g
    }

    fn is_current(&self, connection: &str, generation: u64) -> bool {
        self.generation.get(connection) == Some(&generation)
    }
}

fn pool(pools: &SharedPools, connection: &str) -> Option<Arc<Pool>> {
    pools.lock().ok()?.open.get(connection).cloned()
}

async fn run(mut commands: UnboundedReceiver<DbCommand>, events: UnboundedSender<DbEvent>) {
    let pools: SharedPools = Arc::default();

    while let Some(cmd) = commands.recv().await {
        tracing::debug!(?cmd, "db command");
        match cmd {
            DbCommand::Connect {
                request,
                resolve_password,
            } => {
                let connection = request.conn.name.clone();
                let Ok(generation) = pools.lock().map(|mut p| p.bump(&connection)) else {
                    continue;
                };
                tokio::spawn(connect(
                    *request,
                    resolve_password,
                    generation,
                    pools.clone(),
                    events.clone(),
                ));
            }
            DbCommand::LoadCatalog {
                connection,
                database,
                include_system,
            } => {
                let Some(p) = pool(&pools, &connection) else {
                    send(
                        &events,
                        DbEvent::CatalogFailed {
                            connection,
                            database,
                            error: "non connecté".into(),
                        },
                    );
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let ev = match p.get(&database).await {
                        Ok(b) => match b.load_catalog(include_system).await {
                            Ok(mut catalog) => {
                                catalog.connection = connection;
                                DbEvent::CatalogLoaded {
                                    catalog,
                                    elapsed: started.elapsed(),
                                }
                            }
                            Err(e) => DbEvent::CatalogFailed {
                                connection,
                                database,
                                error: format!("{e:#}"),
                            },
                        },
                        Err(e) => DbEvent::CatalogFailed {
                            connection,
                            database,
                            error: e.to_string(),
                        },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::LoadDetails(object) => {
                let Some(p) = pool(&pools, &object.connection) else {
                    send(
                        &events,
                        DbEvent::DetailsFailed {
                            object,
                            error: "non connecté".into(),
                        },
                    );
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let result = match p.get(&object.database).await {
                        Ok(b) => b
                            .object_details(&object)
                            .await
                            .map_err(|e| format!("{e:#}")),
                        Err(e) => Err(e.to_string()),
                    };
                    let ev = match result {
                        Ok(details) => DbEvent::DetailsLoaded {
                            details: Box::new(details),
                            elapsed: started.elapsed(),
                        },
                        Err(error) => DbEvent::DetailsFailed { object, error },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::FetchRows { id, request } => {
                let Some(p) = pool(&pools, &request.object.connection) else {
                    send(
                        &events,
                        DbEvent::RowsFailed {
                            id,
                            error: "non connecté".into(),
                        },
                    );
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let result = match p.get(&request.object.database).await {
                        Ok(b) => b.fetch_rows(&request).await.map_err(|e| format!("{e:#}")),
                        Err(e) => Err(e.to_string()),
                    };
                    let ev = match result {
                        Ok(rows) => DbEvent::RowsLoaded {
                            id,
                            offset: request.offset,
                            rows,
                            elapsed: started.elapsed(),
                        },
                        Err(error) => DbEvent::RowsFailed { id, error },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::CountRows { id, request, bound } => {
                let Some(p) = pool(&pools, &request.object.connection) else {
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let result = match p.get(&request.object.database).await {
                        Ok(b) => b
                            .count_rows(&request, bound)
                            .await
                            .map_err(|e| format!("{e:#}")),
                        Err(e) => Err(e.to_string()),
                    };
                    let ev = match result {
                        Ok(count) => DbEvent::RowsCounted { id, count },
                        Err(error) => DbEvent::CountFailed { id, error },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::ApplyChanges { id, set } => {
                let p = pool(&pools, &set.object.connection);
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let result = match p {
                        None => Err(not_connected()),
                        Some(p) => match p.get(&set.object.database).await {
                            Ok(b) => b.apply_changes(&set).await,
                            Err(e) => Err(ApplyError::Failed {
                                index: None,
                                message: e.to_string(),
                            }),
                        },
                    };
                    send(
                        &events,
                        match result {
                            Ok(counts) => DbEvent::ChangesApplied {
                                id,
                                counts: Some(counts),
                                elapsed: started.elapsed(),
                            },
                            Err(error) => DbEvent::ChangesFailed { id, error },
                        },
                    );
                });
            }
            DbCommand::RunScript { id, object, sql } => {
                let p = pool(&pools, &object.connection);
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let result = match p {
                        None => Err(not_connected()),
                        Some(p) => match p.get(&object.database).await {
                            Ok(b) => b.run_script(&sql).await.map_err(|e| ApplyError::Failed {
                                index: None,
                                message: format!("{e:#}"),
                            }),
                            Err(e) => Err(ApplyError::Failed {
                                index: None,
                                message: e.to_string(),
                            }),
                        },
                    };
                    send(
                        &events,
                        match result {
                            Ok(()) => DbEvent::ChangesApplied {
                                id,
                                counts: None,
                                elapsed: started.elapsed(),
                            },
                            Err(error) => DbEvent::ChangesFailed { id, error },
                        },
                    );
                });
            }
            DbCommand::Cancel {
                connection,
                database,
            } => {
                if let Some(p) = pool(&pools, &connection) {
                    tokio::spawn(async move {
                        let result = match p.get(&database).await {
                            Ok(b) => b.cancel().await,
                            Err(e) => Err(e.into()),
                        };
                        if let Err(e) = result {
                            tracing::warn!("cancel failed: {e:#}");
                        }
                    });
                }
            }
            DbCommand::Discover { ports } => {
                let events = events.clone();
                tokio::spawn(async move {
                    let found = discover::scan(&ports).await;
                    tracing::info!(servers = found.len(), "discovery done");
                    send(&events, DbEvent::Discovered(found));
                });
            }
            DbCommand::Disconnect { connection } => {
                if let Ok(mut p) = pools.lock() {
                    p.bump(&connection);
                }
                tracing::info!(%connection, "disconnected");
            }
        }
    }
    tracing::debug!("db worker stopped");
}

/// Opens a connection: password, tunnel, first database, list of databases.
async fn connect(
    mut request: ConnectRequest,
    resolve_password: bool,
    generation: u64,
    pools: SharedPools,
    events: UnboundedSender<DbEvent>,
) {
    let connection = request.conn.name.clone();
    let started = Instant::now();
    let fail = |error: String, password_required: bool| DbEvent::ConnectFailed {
        connection: connection.clone(),
        error,
        password_required,
    };
    if resolve_password && request.password.is_none() {
        match password::resolve(&request.conn).await {
            Ok(Some((pw, source))) => {
                tracing::info!(%connection, ?source, "password resolved");
                request.password = Some(pw);
            }
            Ok(None) => {}
            Err(e) => {
                send(&events, fail(format!("{e:#}"), false));
                return;
            }
        }
    }
    let on_lost: super::postgres::OnConnectionLost = {
        let events = events.clone();
        let pools = pools.clone();
        let connection = connection.clone();
        Arc::new(move |message: String| {
            let current = pools
                .lock()
                .is_ok_and(|p| p.is_current(&connection, generation));
            if current {
                send(
                    &events,
                    DbEvent::ConnectionLost {
                        connection: connection.clone(),
                        message,
                    },
                );
            }
        })
    };
    let progress = |message: &str| DbEvent::Progress {
        connection: connection.clone(),
        message: message.into(),
    };

    let primary = request.conn.effective_database();
    let mut template = request.clone();
    let mut tunnel = None;
    if let Some(cmd) = request.conn.tunnel_cmd.clone() {
        send(&events, progress("Ouverture du tunnel…"));
        let timeout = Duration::from_secs(request.conn.tunnel_timeout_secs.unwrap_or(15));
        match super::tunnel::open(&cmd, timeout).await {
            Ok(t) => {
                template.conn.host = "127.0.0.1".into();
                template.conn.port = t.port;
                tunnel = Some(t);
                send(&events, progress("Tunnel ouvert, connexion…"));
            }
            Err(e) => {
                send(&events, fail(format!("{e:#}"), false));
                return;
            }
        }
    }
    let has_tunnel = tunnel.is_some();
    let p = Arc::new(Pool {
        template,
        on_lost: on_lost.clone(),
        backends: Mutex::default(),
        tunnel: tunnel.map(std::sync::Mutex::new),
    });
    match p.get(&primary).await {
        Ok(b) => {
            let info = b.info().clone();
            let databases = b.list_databases().await.unwrap_or_else(|e| {
                tracing::warn!(%connection, "cannot list databases: {e:#}");
                Vec::new()
            });
            // A newer Connect or a Disconnect may have happened meanwhile.
            let Ok(mut guard) = pools.lock() else { return };
            if !guard.is_current(&connection, generation) {
                return;
            }
            guard.open.insert(connection.clone(), p.clone());
            drop(guard);
            if has_tunnel {
                watch_tunnel(Arc::downgrade(&p), on_lost);
            }
            send(
                &events,
                DbEvent::Connected {
                    connection,
                    info,
                    databases,
                    elapsed: started.elapsed(),
                },
            );
        }
        Err(e) => {
            tracing::warn!(%connection, "connect failed: {e}");
            send(
                &events,
                fail(
                    e.to_string(),
                    matches!(e, ConnectError::PasswordRequired(_)),
                ),
            );
        }
    }
}

/// Reports the death of the tunnel process (ssh killed, network down).
fn watch_tunnel(pool: std::sync::Weak<Pool>, on_lost: super::postgres::OnConnectionLost) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(p) = pool.upgrade() else { return };
            let reason = p.tunnel.as_ref().and_then(|t| t.lock().ok()?.exited());
            if let Some(reason) = reason {
                tracing::warn!("{reason}");
                on_lost(reason);
                return;
            }
        }
    });
}

fn not_connected() -> ApplyError {
    ApplyError::Failed {
        index: None,
        message: "non connecté".into(),
    }
}

fn send(events: &UnboundedSender<DbEvent>, ev: DbEvent) {
    // The receiver only disappears when the app is shutting down.
    let _ = events.send(ev);
}
