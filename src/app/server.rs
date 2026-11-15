//! A connection of the tree (configured, from the command line, or added
//! from a discovered server) and the state of its session.

use std::collections::HashMap;
use std::time::Instant;

use super::DetailsState;
use super::tree::DatabaseEntry;
use crate::config::ConnectionConfig;
use crate::db::ServerInfo;
use crate::db::catalog::ObjectRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting(Instant),
    Connected,
    /// Last attempt failed (expanding again retries).
    Failed(String),
    /// The session dropped (server restarted, tunnel closed, ...).
    Lost(String),
}

#[derive(Debug, Clone)]
pub struct Server {
    pub conn: ConnectionConfig,
    /// Password given on the command line (`--url`), if any.
    pub password: Option<String>,
    /// Comes from `config.toml` (can be deleted from it).
    pub saved: bool,
    pub state: ConnState,
    pub info: Option<ServerInfo>,
    /// Every database of the server, with its catalog once loaded.
    pub databases: Vec<DatabaseEntry>,
    /// Schema tab cache, cleared when a catalog is reloaded.
    pub details: HashMap<ObjectRef, DetailsState>,
    /// Progress shown while connecting (tunnel, ...).
    pub progress: Option<String>,
}

impl Server {
    pub fn new(conn: ConnectionConfig, password: Option<String>, saved: bool) -> Self {
        Self {
            conn,
            password,
            saved,
            state: ConnState::Disconnected,
            info: None,
            databases: Vec::new(),
            details: HashMap::new(),
            progress: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.conn.name
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnState::Connected
    }

    /// Back to "not connected", keeping the configuration.
    pub fn reset(&mut self) {
        self.state = ConnState::Disconnected;
        self.info = None;
        self.databases.clear();
        self.details.clear();
        self.progress = None;
    }

    pub fn database(&self, name: &str) -> Option<&DatabaseEntry> {
        self.databases.iter().find(|d| d.name() == name)
    }
}
