//! PostgreSQL backend built on `tokio-postgres`.
//!
//! Two connections per session:
//! - `session`: user queries, data browsing, writes and manual transactions;
//! - `catalog`: metadata (tree, schema tab, completion) so that it is never
//!   blocked behind a long user query. It is always read-only.

mod catalog;
pub mod ddl;
mod details;
mod tls;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

use super::backend::{Backend, BackendKind, ServerInfo};
use super::catalog::{Catalog, ObjectRef};
use super::schema::ObjectDetails;
use super::{ConnectError, ConnectRequest};
use crate::config::SslMode;

/// Called (from a background task) when a connection drops.
pub type OnConnectionLost = Arc<dyn Fn(String) + Send + Sync>;

pub struct PgBackend {
    #[allow(dead_code)] // used from milestone 3 on (data / SQL queries)
    session: Client,
    catalog: Client,
    info: ServerInfo,
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
        Ok(Self {
            session,
            catalog,
            info,
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
        catalog::load(&self.catalog, include_system).await
    }

    async fn object_details(&self, object: &ObjectRef) -> anyhow::Result<ObjectDetails> {
        details::load(&self.catalog, object).await
    }
}
