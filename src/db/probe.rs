//! Connection test: what the application does to connect (password
//! sources, tunnel, TLS), then everything is closed again. Used by
//! `pgtui --check-connections` and by the connection test popup.

use std::time::{Duration, Instant};

use super::{ConnectError, ConnectRequest};
use crate::config::{ConnectionConfig, password};

/// Whole test, tunnel included.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOk {
    /// e.g. `16.4`
    pub version: String,
    pub user: String,
    pub database: String,
    pub tls: bool,
    pub elapsed: Duration,
}

/// Connects with `conn` (resolving its password unless `password` is
/// given), reads the server version, then disconnects.
pub async fn probe(conn: &ConnectionConfig, password: Option<String>) -> Result<ProbeOk, String> {
    match tokio::time::timeout(PROBE_TIMEOUT, run(conn, password)).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "pas de réponse après {} s",
            PROBE_TIMEOUT.as_secs()
        )),
    }
}

async fn run(conn: &ConnectionConfig, password: Option<String>) -> Result<ProbeOk, String> {
    let started = Instant::now();
    let password = match password {
        Some(p) => Some(p),
        None => password::resolve(conn)
            .await
            .map_err(|e| format!("mot de passe : {e:#}"))?
            .map(|(pw, _)| pw),
    };
    let mut target = conn.clone();
    // Kept alive until the end of the test, killed when dropped.
    let _tunnel = match &conn.tunnel_cmd {
        Some(cmd) => {
            let timeout = Duration::from_secs(conn.tunnel_timeout_secs.unwrap_or(15));
            let t = super::tunnel::open(cmd, timeout)
                .await
                .map_err(|e| format!("tunnel : {e:#}"))?;
            target.host = "127.0.0.1".into();
            target.port = t.port;
            Some(t)
        }
        None => None,
    };
    let request = ConnectRequest {
        conn: target,
        password,
        statement_timeout_secs: 0,
    };
    let backend = super::connect(&request, std::sync::Arc::new(|_| {}))
        .await
        .map_err(|e| match e {
            ConnectError::PasswordRequired(m) => format!("mot de passe requis ou refusé : {m}"),
            ConnectError::Other(e) => format!("{e:#}"),
        })?;
    let info = backend.info().clone();
    Ok(ProbeOk {
        // `16.4 (Ubuntu 16.4-1)`: the number is enough here.
        version: info
            .version
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
        user: info.user,
        database: info.database,
        tls: info.tls,
        elapsed: started.elapsed(),
    })
}

/// `user@host:port/database`, the way the connection is reached.
pub fn describe(conn: &ConnectionConfig) -> String {
    let via = if conn.tunnel_cmd.is_some() {
        " (tunnel)"
    } else {
        ""
    };
    format!(
        "{}@{}:{}/{}{via}",
        conn.effective_user(),
        conn.host,
        conn.port,
        conn.effective_database()
    )
}
