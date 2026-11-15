//! The engine abstraction. PostgreSQL is the only implementation for now;
//! another engine (MySQL, SQLite, ...) would add a module implementing
//! [`Backend`] and a branch in [`super::connect`].

use async_trait::async_trait;

use super::catalog::{Catalog, DatabaseInfo, ObjectRef};
use super::changes::{ApplyError, ChangeCounts, ChangeSet};
use super::data::{DataRequest, RowCount, TextRows};
use super::exec::{ExecError, StatementResult};
use super::schema::ObjectDetails;

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
    /// e.g. `170002`
    pub version_num: i32,
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

    /// All databases of the server (templates excluded).
    async fn list_databases(&self) -> anyhow::Result<Vec<DatabaseInfo>>;

    /// Columns, indexes, constraints, DDL, ... of one object.
    async fn object_details(&self, object: &ObjectRef) -> anyhow::Result<ObjectDetails>;

    /// One window of rows, every value as text (session connection).
    async fn fetch_rows(&self, req: &DataRequest) -> anyhow::Result<TextRows>;

    /// Number of rows matching the request's filter, counting at most
    /// `bound + 1` rows (catalog connection, never blocks the session).
    async fn count_rows(&self, req: &DataRequest, bound: u64) -> anyhow::Result<RowCount>;

    /// Applies staged changes in one transaction on the session connection.
    /// Everything is rolled back on the first error or conflict (an update
    /// or delete that matches no row).
    async fn apply_changes(&self, set: &ChangeSet) -> Result<ChangeCounts, ApplyError>;

    /// Runs a script written or edited by the user in one transaction
    /// (rolled back on error).
    async fn run_script(&self, sql: &str) -> anyhow::Result<()>;

    /// Runs one statement typed by the user on the session connection,
    /// keeping at most `max_rows` rows. Transaction control (`BEGIN`,
    /// `COMMIT`, ...) is the user's: no implicit transaction is added.
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<StatementResult, ExecError>;

    /// Asks the server to cancel the query running on the session connection.
    async fn cancel(&self) -> anyhow::Result<()>;
}
