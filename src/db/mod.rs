//! Database access. Only the worker task touches connections; the UI talks to
//! it through [`worker::DbCommand`] / [`worker::DbEvent`].

pub mod backend;
pub mod catalog;
pub mod postgres;
pub mod worker;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};

use crate::config::{ConnectionConfig, SslMode};
pub use backend::{Backend, BackendKind, ServerInfo};

/// Everything needed to open a connection.
#[derive(Clone)]
pub struct ConnectRequest {
    pub conn: ConnectionConfig,
    /// Resolved or typed password; `None` lets the server decide (trust, peer, .pgpass already tried).
    pub password: Option<String>,
    pub statement_timeout_secs: u64,
}

impl std::fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectRequest")
            .field("conn", &self.conn.name)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The server wants a (different) password: the UI should prompt.
    #[error("{0}")]
    PasswordRequired(String),
    #[error("{0:#}")]
    Other(#[from] anyhow::Error),
}

/// Opens a backend for the request (PostgreSQL only for now).
pub async fn connect(
    req: &ConnectRequest,
    on_lost: postgres::OnConnectionLost,
) -> Result<Arc<dyn Backend>, ConnectError> {
    let backend = postgres::PgBackend::connect(req, on_lost).await?;
    Ok(Arc::new(backend))
}

/// Turns a `postgres://` URL (or libpq `key=value` string) into a connection
/// named `name`, plus the password it contains, if any.
///
/// `sslmode=verify-ca|verify-full` and `sslrootcert` are understood even
/// though `tokio-postgres` itself does not parse them.
pub fn connection_from_url(name: &str, url: &str) -> Result<(ConnectionConfig, Option<String>)> {
    let (rest, sslmode, sslrootcert) = extract_tls_params(url)?;
    let cfg = tokio_postgres::Config::from_str(&rest).context("invalid connection URL")?;

    let mut conn = ConnectionConfig::named(name);
    if let Some(host) = cfg.get_hosts().first() {
        conn.host = match host {
            tokio_postgres::config::Host::Tcp(h) => h.clone(),
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(p) => p.display().to_string(),
        };
    }
    if let Some(port) = cfg.get_ports().first() {
        conn.port = *port;
    }
    conn.user = cfg.get_user().map(str::to_string);
    conn.database = cfg.get_dbname().map(str::to_string);
    if let Some(mode) = sslmode {
        conn.sslmode = mode;
    }
    conn.sslrootcert = sslrootcert;
    let password = cfg
        .get_password()
        .map(|p| String::from_utf8(p.to_vec()).map_err(|_| anyhow!("password in URL is not UTF-8")))
        .transpose()?;
    Ok((conn, password))
}

/// Removes `sslmode` / `sslrootcert` from a URL or `key=value` string.
fn extract_tls_params(url: &str) -> Result<(String, Option<SslMode>, Option<PathBuf>)> {
    let mut sslmode = None;
    let mut rootcert = None;
    let mut take = |key: &str, value: &str| -> Result<bool> {
        match key {
            "sslmode" => {
                sslmode = Some(SslMode::from_str(value).map_err(|e| anyhow!(e))?);
                Ok(true)
            }
            "sslrootcert" => {
                rootcert = Some(PathBuf::from(value));
                Ok(true)
            }
            _ => Ok(false),
        }
    };

    let rest = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        match url.split_once('?') {
            None => url.to_string(),
            Some((base, query)) => {
                let mut kept = Vec::new();
                for pair in query.split('&').filter(|p| !p.is_empty()) {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    if !take(k, v)? {
                        kept.push(pair);
                    }
                }
                if kept.is_empty() {
                    base.to_string()
                } else {
                    format!("{base}?{}", kept.join("&"))
                }
            }
        }
    } else {
        let mut kept = Vec::new();
        for token in url.split_whitespace() {
            let (k, v) = token.split_once('=').unwrap_or((token, ""));
            if !take(k, v)? {
                kept.push(token);
            }
        }
        kept.join(" ")
    };
    Ok((rest, sslmode, rootcert))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_full() {
        let (c, pw) = connection_from_url(
            "url",
            "postgres://bob:s%3Acret@db.example.com:6432/shop?sslmode=verify-full&application_name=x",
        )
        .unwrap();
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 6432);
        assert_eq!(c.user.as_deref(), Some("bob"));
        assert_eq!(c.database.as_deref(), Some("shop"));
        assert_eq!(c.sslmode, SslMode::VerifyFull);
        assert_eq!(pw.as_deref(), Some("s:cret"));
    }

    #[test]
    fn url_minimal_defaults() {
        let (c, pw) = connection_from_url("url", "postgresql://localhost").unwrap();
        assert_eq!(c.host, "localhost");
        assert_eq!(c.port, 5432);
        assert_eq!(c.sslmode, SslMode::Prefer);
        assert!(pw.is_none());
    }

    #[test]
    fn key_value_form() {
        let (c, _) = connection_from_url(
            "url",
            "host=/var/run/postgresql user=me dbname=x sslmode=verify-ca sslrootcert=/tmp/ca.pem",
        )
        .unwrap();
        assert_eq!(c.host, "/var/run/postgresql");
        assert_eq!(c.sslmode, SslMode::VerifyCa);
        assert_eq!(c.sslrootcert, Some(PathBuf::from("/tmp/ca.pem")));
    }

    #[test]
    fn invalid_sslmode() {
        assert!(connection_from_url("url", "postgres://h/db?sslmode=bogus").is_err());
    }
}
