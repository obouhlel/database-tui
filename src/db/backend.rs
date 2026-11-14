//! The engine abstraction. PostgreSQL is the only implementation for now;
//! another engine (MySQL, SQLite, ...) would add a module implementing
//! [`Backend`] and a branch in [`super::connect`].

use async_trait::async_trait;

use super::catalog::Catalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Postgres,
}

/// What the UI needs to know about an open connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub kind: BackendKind,
    /// e.g. `17.2`
    pub version: String,
    pub user: String,
    pub database: String,
    /// `true` if the TLS handshake succeeded.
    pub tls: bool,
}

/// An open session on a database server.
///
/// Implementations own their connections and must be cheap to share
/// (`Arc<dyn Backend>`): every method takes `&self` so that the worker can run
/// catalog queries while a user query is in flight. Dropping the last handle
/// closes the connections.
#[async_trait]
pub trait Backend: Send + Sync {
    fn info(&self) -> &ServerInfo;

    /// Loads schemas and their objects. System schemas are included only if
    /// `include_system` is set.
    async fn load_catalog(&self, include_system: bool) -> anyhow::Result<Catalog>;
}
