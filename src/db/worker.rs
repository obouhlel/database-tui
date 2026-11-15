//! The DB worker: a tokio task that owns the backend. The UI sends
//! [`DbCommand`]s and receives [`DbEvent`]s; it never awaits a query itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::catalog::{Catalog, ObjectRef};
use super::data::{DataRequest, RowCount, TextRows};
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
    /// Cancel the query running on the session connection.
    Cancel,
    Disconnect,
}

#[derive(Debug)]
pub enum DbEvent {
    Connected {
        info: ServerInfo,
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
    CatalogFailed(String),
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
    ConnectionLost(String),
}

/// Spawns the worker on the current tokio runtime.
pub fn spawn(events: UnboundedSender<DbEvent>) -> UnboundedSender<DbCommand> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(run(rx, events));
    tx
}

async fn run(mut commands: UnboundedReceiver<DbCommand>, events: UnboundedSender<DbEvent>) {
    let mut backend: Option<Arc<dyn Backend>> = None;
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
                backend = None;
                let current = generation.fetch_add(1, Ordering::SeqCst) + 1;
                let started = Instant::now();
                let mut request = *request;
                if resolve_password && request.password.is_none() {
                    match password::resolve(&request.conn).await {
                        Ok(found) => {
                            if let Some((pw, source)) = found {
                                tracing::info!(connection = %request.conn.name, ?source, "password resolved");
                                request.password = Some(pw);
                            }
                        }
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
                match super::connect(&request, on_lost).await {
                    Ok(b) => {
                        let info = b.info().clone();
                        backend = Some(b);
                        send(
                            &events,
                            DbEvent::Connected {
                                info,
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
            DbCommand::LoadCatalog { include_system } => {
                let Some(b) = backend.clone() else {
                    send(&events, DbEvent::CatalogFailed("not connected".into()));
                    continue;
                };
                let events = events.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let ev = match b.load_catalog(include_system).await {
                        Ok(catalog) => DbEvent::CatalogLoaded {
                            catalog,
                            elapsed: started.elapsed(),
                        },
                        Err(e) => DbEvent::CatalogFailed(format!("{e:#}")),
                    };
                    send(&events, ev);
                });
            }
            DbCommand::LoadDetails(object) => {
                let Some(b) = backend.clone() else {
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
                    let ev = match b.object_details(&object).await {
                        Ok(details) => DbEvent::DetailsLoaded {
                            details: Box::new(details),
                            elapsed: started.elapsed(),
                        },
                        Err(e) => DbEvent::DetailsFailed {
                            object,
                            error: format!("{e:#}"),
                        },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::FetchRows { id, request } => {
                let Some(b) = backend.clone() else {
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
                    let ev = match b.fetch_rows(&request).await {
                        Ok(rows) => DbEvent::RowsLoaded {
                            id,
                            offset: request.offset,
                            rows,
                            elapsed: started.elapsed(),
                        },
                        Err(e) => DbEvent::RowsFailed {
                            id,
                            error: format!("{e:#}"),
                        },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::CountRows { id, request, bound } => {
                let Some(b) = backend.clone() else { continue };
                let events = events.clone();
                tokio::spawn(async move {
                    let ev = match b.count_rows(&request, bound).await {
                        Ok(count) => DbEvent::RowsCounted { id, count },
                        Err(e) => DbEvent::CountFailed {
                            id,
                            error: format!("{e:#}"),
                        },
                    };
                    send(&events, ev);
                });
            }
            DbCommand::Cancel => {
                if let Some(b) = backend.clone() {
                    tokio::spawn(async move {
                        if let Err(e) = b.cancel().await {
                            tracing::warn!("cancel failed: {e:#}");
                        }
                    });
                }
            }
            DbCommand::Disconnect => {
                generation.fetch_add(1, Ordering::SeqCst);
                backend = None;
            }
        }
    }
    tracing::debug!("db worker stopped");
}

fn send(events: &UnboundedSender<DbEvent>, ev: DbEvent) {
    // The receiver only disappears when the app is shutting down.
    let _ = events.send(ev);
}
