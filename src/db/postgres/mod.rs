//! PostgreSQL backend built on `tokio-postgres`.
//!
//! Two connections per session:
//! - `session`: user queries, data browsing, writes and manual transactions;
//! - `catalog`: metadata (tree, schema tab, completion) so that it is never
//!   blocked behind a long user query. It is always read-only.

mod catalog;
pub mod changes;
pub mod ddl;
mod details;
pub mod query;
mod tls;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

use super::backend::{Backend, BackendKind, ServerInfo};
use super::catalog::{Catalog, DatabaseInfo, ObjectRef};
use super::changes::{ApplyError, ChangeCounts, ChangeSet};
use super::data::{DataRequest, RowCount, TextRows};
use super::schema::ObjectDetails;
use super::{ConnectError, ConnectRequest};
use crate::config::SslMode;

/// Called (from a background task) when a connection drops.
pub type OnConnectionLost = Arc<dyn Fn(String) + Send + Sync>;

pub struct PgBackend {
    session: Client,
    catalog: Client,
    info: ServerInfo,
    /// Cancels the query running on the session connection.
    cancel: tokio_postgres::CancelToken,
    tls: tokio_postgres_rustls::MakeRustlsConnect,
    /// Held while a transaction runs on the session connection, so that no
    /// other statement (a page of rows) lands inside it.
    session_lock: tokio::sync::Mutex<()>,
}

impl PgBackend {
    pub async fn connect(
        req: &ConnectRequest,
        on_lost: OnConnectionLost,
    ) -> Result<Self, ConnectError> {
        let (session, catalog) = tokio::try_join!(
            open(req, "session", on_lost.clone()),
            open(req, "catalog", on_lost)
        )?;

        let mut setup = Vec::new();
        if req.conn.read_only {
            setup.push("SET default_transaction_read_only = on".to_string());
        }
        if req.statement_timeout_secs > 0 {
            setup.push(format!(
                "SET statement_timeout = {}",
                req.statement_timeout_secs.saturating_mul(1000)
            ));
        }
        if !setup.is_empty() {
            session
                .batch_execute(&setup.join("; "))
                .await
                .map_err(|e| ConnectError::Other(e.into()))?;
        }
        catalog
            .batch_execute("SET default_transaction_read_only = on")
            .await
            .map_err(|e| ConnectError::Other(e.into()))?;

        let row = session
            .query_one(
                "SELECT current_setting('server_version'), current_user::text, current_database()::text, \
                 coalesce((SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()), false), \
                 current_setting('server_version_num')::int4",
                &[],
            )
            .await
            .map_err(|e| ConnectError::Other(e.into()))?;
        let info = ServerInfo {
            kind: BackendKind::Postgres,
            version: row.get(0),
            user: row.get(1),
            database: row.get(2),
            tls: row.get(3),
            version_num: row.get(4),
        };
        tracing::info!(
            connection = %req.conn.name,
            version = %info.version,
            tls = info.tls,
            read_only = req.conn.read_only,
            "connected"
        );
        let cancel = session.cancel_token();
        let tls = tls::make_connector(&req.conn).map_err(ConnectError::Other)?;
        Ok(Self {
            session,
            catalog,
            info,
            cancel,
            tls,
            session_lock: tokio::sync::Mutex::new(()),
        })
    }
}

async fn open(
    req: &ConnectRequest,
    role: &'static str,
    on_lost: OnConnectionLost,
) -> Result<Client, ConnectError> {
    let conn = &req.conn;
    let mut cfg = tokio_postgres::Config::new();
    cfg.host(&conn.host)
        .port(conn.port)
        .user(conn.effective_user())
        .dbname(conn.effective_database())
        .application_name(format!("pgtui ({role})"))
        .connect_timeout(Duration::from_secs(10))
        .keepalives(true)
        .ssl_mode(match conn.sslmode {
            SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => {
                tokio_postgres::config::SslMode::Require
            }
        });
    if let Some(p) = &req.password {
        cfg.password(p.as_bytes());
    }
    let tls = tls::make_connector(conn).map_err(ConnectError::Other)?;
    let (client, connection) = cfg.connect(tls).await.map_err(classify)?;
    tokio::spawn(async move {
        match connection.await {
            Ok(()) => tracing::debug!(role, "connection closed"),
            Err(e) => {
                tracing::error!(role, "connection lost: {e}");
                on_lost(e.to_string());
            }
        }
    });
    Ok(client)
}

fn classify(e: tokio_postgres::Error) -> ConnectError {
    // The interesting part ("password missing") is often in the source chain.
    let mut full = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        full.push_str(": ");
        full.push_str(&s.to_string());
        source = s.source();
    }
    let needs_password =
        e.code() == Some(&SqlState::INVALID_PASSWORD) || full.contains("password missing");
    if needs_password {
        let msg = e.as_db_error().map_or_else(
            || "le serveur demande un mot de passe".to_string(),
            |d| d.message().to_string(),
        );
        ConnectError::PasswordRequired(msg)
    } else {
        ConnectError::Other(anyhow::anyhow!(full))
    }
}

#[async_trait]
impl Backend for PgBackend {
    fn info(&self) -> &ServerInfo {
        &self.info
    }

    async fn load_catalog(&self, include_system: bool) -> anyhow::Result<Catalog> {
        catalog::load(&self.catalog, &self.info.database, include_system).await
    }

    async fn list_databases(&self) -> anyhow::Result<Vec<DatabaseInfo>> {
        let rows = self
            .catalog
            .query(
                "SELECT d.datname::text, shobj_description(d.oid, 'pg_database'), \
                        has_database_privilege(d.datname, 'CONNECT'), \
                        CASE WHEN has_database_privilege(d.datname, 'CONNECT') \
                             THEN pg_database_size(d.datname) END \
                 FROM pg_database d \
                 WHERE d.datallowconn AND NOT d.datistemplate \
                 ORDER BY d.datname",
                &[],
            )
            .await
            .map_err(pg_error)?;
        Ok(rows
            .iter()
            .map(|r| DatabaseInfo {
                name: r.get(0),
                comment: r.get(1),
                can_connect: r.get(2),
                size_bytes: r.get(3),
            })
            .collect())
    }

    async fn object_details(&self, object: &ObjectRef) -> anyhow::Result<ObjectDetails> {
        details::load(&self.catalog, object).await
    }

    async fn fetch_rows(&self, req: &DataRequest) -> anyhow::Result<TextRows> {
        let sql = query::select(req);
        let _guard = self.session_lock.lock().await;
        let rows = self
            .session
            .query(&sql.text, &params(&sql.params))
            .await
            .map_err(pg_error)?;
        let mut columns: Vec<String> = req.columns.iter().map(|c| c.name.clone()).collect();
        if req.with_row_version {
            columns.push("xmin".into());
        }
        let rows = rows
            .iter()
            .map(|r| {
                (0..r.len())
                    .map(|i| r.get::<_, Option<String>>(i))
                    .collect()
            })
            .collect();
        Ok(TextRows { columns, rows })
    }

    async fn count_rows(&self, req: &DataRequest, bound: u64) -> anyhow::Result<RowCount> {
        let sql = query::count(req, bound);
        let n: i64 = self
            .catalog
            .query_one(&sql.text, &params(&sql.params))
            .await
            .map_err(pg_error)?
            .get(0);
        let n = n.max(0) as u64;
        Ok(if n > bound {
            RowCount::AtLeast(bound)
        } else {
            RowCount::Exact(n)
        })
    }

    async fn apply_changes(&self, set: &ChangeSet) -> Result<ChangeCounts, ApplyError> {
        let _guard = self.session_lock.lock().await;
        let failed = |index, e: tokio_postgres::Error| ApplyError::Failed {
            index,
            message: format!("{:#}", pg_error(e)),
        };
        self.session
            .batch_execute("BEGIN")
            .await
            .map_err(|e| failed(None, e))?;
        let mut outcome = Ok(());
        for (index, (st, change)) in changes::statements(set)
            .iter()
            .zip(&set.changes)
            .enumerate()
        {
            let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = st
                .params
                .iter()
                .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            match self.session.execute(&st.text, &params).await {
                Ok(0) if change.checks_one_row() => {
                    outcome = Err(ApplyError::Conflict { index });
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    outcome = Err(failed(Some(index), e));
                    break;
                }
            }
        }
        match outcome {
            Ok(()) => {
                if let Err(e) = self.session.batch_execute("COMMIT").await {
                    // A deferred constraint can fail at commit time.
                    let _ = self.session.batch_execute("ROLLBACK").await;
                    return Err(failed(None, e));
                }
                tracing::info!(table = %set.object.qualified(), changes = set.changes.len(), "changes applied");
                Ok(set.counts())
            }
            Err(e) => {
                if let Err(r) = self.session.batch_execute("ROLLBACK").await {
                    tracing::error!("rollback failed: {r}");
                }
                tracing::warn!(table = %set.object.qualified(), "changes rolled back: {e}");
                Err(e)
            }
        }
    }

    async fn run_script(&self, sql: &str) -> anyhow::Result<()> {
        let _guard = self.session_lock.lock().await;
        self.session
            .batch_execute("BEGIN")
            .await
            .map_err(pg_error)?;
        match self.session.batch_execute(sql).await {
            Ok(()) => {
                if let Err(e) = self.session.batch_execute("COMMIT").await {
                    let _ = self.session.batch_execute("ROLLBACK").await;
                    return Err(pg_error(e));
                }
                Ok(())
            }
            Err(e) => {
                let _ = self.session.batch_execute("ROLLBACK").await;
                Err(pg_error(e))
            }
        }
    }

    async fn cancel(&self) -> anyhow::Result<()> {
        self.cancel.cancel_query(self.tls.clone()).await?;
        Ok(())
    }
}

fn params(values: &[String]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    values
        .iter()
        .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect()
}

/// Server errors with their detail and hint, e.g. a syntax error in a free
/// `WHERE` clause.
fn pg_error(e: tokio_postgres::Error) -> anyhow::Error {
    match e.as_db_error() {
        Some(db) => {
            let mut msg = db.message().to_string();
            if let Some(d) = db.detail() {
                msg.push_str(&format!(" ({d})"));
            }
            if let Some(h) = db.hint() {
                msg.push_str(&format!(" — {h}"));
            }
            anyhow::anyhow!(msg)
        }
        None => e.into(),
    }
}
