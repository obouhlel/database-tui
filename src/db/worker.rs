//! The DB worker: a tokio task that owns the backends. The UI sends
//! [`DbCommand`]s and receives [`DbEvent`]s; it never awaits a query itself.
//!
//! A session targets a server: the database chosen at connection time is
//! opened first, the others lazily (when the tree expands them or an object
//! of theirs is opened), with the same credentials.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, OnceCell};

use super::catalog::{Catalog, DatabaseInfo, ObjectRef};
use super::data::{DataRequest, RowCount, TextRows};
use super::discover::{self, DiscoveredServer};
use super::schema::ObjectDetails;
use super::{Backend, ConnectError, ConnectRequest, ServerInfo};
use crate::config::password;

#[derive(Debug)]
pub enum DbCommand {
    /// Opens a session, replacing the current one. With `resolve_password`,
    /// the configured password sources are tried when `request.password` is empty.
    Connect {
        request: Box<ConnectRequest>,
        resolve_password: bool,
    },
    LoadCatalog {
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
    /// Cancel the query running on the session connection of `database`.
    Cancel {
        database: String,
    },
    /// Probe the local machine for PostgreSQL servers.
    Discover {
        ports: Vec<u16>,
    },
    Disconnect,
}

#[derive(Debug)]
pub enum DbEvent {
    Connected {
        info: ServerInfo,
        /// Databases of the server (the connected one included).
        databases: Vec<DatabaseInfo>,
        elapsed: Duration,
    },
    ConnectFailed {
        error: String,
        password_required: bool,
    },
    CatalogLoaded {
        catalog: Catalog,
        elapsed: Duration,
    },
    CatalogFailed {
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
    Discovered(Vec<DiscoveredServer>),
    ConnectionLost(String),
}

/// Spawns the worker on the current tokio runtime.
pub fn spawn(events: UnboundedSender<DbEvent>) -> UnboundedSender<DbCommand> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(run(rx, events));
    tx
}

/// A backend opened at most once, even when several requests race for it.
type LazyBackend = Arc<OnceCell<Arc<dyn Backend>>>;

/// One backend per database of the connected server, opened on demand.
struct Pool {
    template: ConnectRequest,
    on_lost: super::postgres::OnConnectionLost,
    backends: Mutex<HashMap<String, LazyBackend>>,
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
            tracing::info!(database, "opening database");
            super::connect(&req, self.on_lost.clone()).await
        })
        .await
        .cloned()
    }
}

async fn run(mut commands: UnboundedReceiver<DbCommand>, events: UnboundedSender<DbEvent>) {
    let mut pool: Option<Arc<Pool>> = None;
    // Bumped on every (dis)connection so that late notifications from an old
    // session are ignored.
    let generation = Arc::new(AtomicU64::new(0));

    while let Some(cmd) = commands.recv().await {
        tracing::debug!(?cmd, "db command");
        match cmd {
            DbCommand::Connect {
                request,
                resolve_password,
            } => {
                pool = None;
                let current = generation.fetch_add(1, Ordering::SeqCst) + 1;
                let started = Instant::now();
                let mut request = *request;
                if resolve_password && request.password.is_none() {
                    match password::resolve(&request.conn).await {
                        Ok(Some((pw, source))) => {
                            tracing::info!(connection = %request.conn.name, ?source, "password resolved");
                            request.password = Some(pw);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            send(
                                &events,
                                DbEvent::ConnectFailed {
                                    error: format!("{e:#}"),
                                    password_required: false,
                                },
                            );
                            continue;
                        }
                    }
                }
                let on_lost = {
                    let events = events.clone();
                    let generation = generation.clone();
                    Arc::new(move |msg: String| {
                        if generation.load(Ordering::SeqCst) == current {
                            send(&events, DbEvent::ConnectionLost(msg));
                        }
                    })
                };
                let primary = request.conn.effective_database();
                let p = Arc::new(Pool {
                    template: request.clone(),
                    on_lost,
                    backends: Mutex::default(),
                });
                match p.get(&primary).await {
                    Ok(b) => {
                        let info = b.info().clone();
                        let databases = match b.list_databases().await {
                            Ok(d) => d,
                            Err(e) => {
                                tracing::warn!("cannot list databases: {e:#}");
                                Vec::new()
                            }
                        };
                        pool = Some(p);
                        send(
                            &events,
                            DbEvent::Connected {
                                info,
                                databases,
                                elapsed: started.elapsed(),
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!(connection = %request.conn.name, "connect failed: {e}");
                        send(
                            &events,
                            DbEvent::ConnectFailed {
                                password_required: matches!(e, ConnectError::PasswordRequired(_)),
                                error: e.to_string(),
                            },
                        );
                    }
                }
            }
            DbCommand::LoadCatalog {
                database,
                include_system,
            } => {
                let Some(p) = pool.clone() else {
                    send(
                        &events,
                        DbEvent::CatalogFailed {
                            database,
                            error: "not connected".into(),
                        },
                    );
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let ev = match p.get(&database).await {
                        Ok(b) => match b.load_catalog(include_system).await {
                            Ok(catalog) => DbEvent::CatalogLoaded {
                                catalog,
                                elapsed: started.elapsed(),
                            },
                            Err(e) => DbEvent::CatalogFailed {
                                database,
                                error: format!("{e:#}"),
                            },
                        },
                        Err(e) => DbEvent::CatalogFailed {
                            database,
                            error: e.to_string(),
                        },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::LoadDetails(object) => {
                let Some(p) = pool.clone() else {
                    send(
                        &events,
                        DbEvent::DetailsFailed {
                            object,
                            error: "not connected".into(),
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
                let Some(p) = pool.clone() else {
                    send(
                        &events,
                        DbEvent::RowsFailed {
                            id,
                            error: "not connected".into(),
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
                let Some(p) = pool.clone() else { continue };
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
            DbCommand::Cancel { database } => {
                if let Some(p) = pool.clone() {
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
            DbCommand::Disconnect => {
                generation.fetch_add(1, Ordering::SeqCst);
                pool = None;
            }
        }
    }
    tracing::debug!("db worker stopped");
}

fn send(events: &UnboundedSender<DbEvent>, ev: DbEvent) {
    // The receiver only disappears when the app is shutting down.
    let _ = events.send(ev);
}
