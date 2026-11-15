//! Servers discovered on the local machine: listing in the picker, login
//! prompt (user, password, database), and the offer to save the server in
//! `config.toml` (without the password) after a successful login.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::input::{InputOutcome, LineInput};
use super::{App, Effect, Level, Overlay};
use crate::config::{ConnectionConfig, SslMode};
use crate::db::discover::DiscoveredServer;
use crate::db::worker::DbCommand;

/// Login form fields, in order.
pub const LOGIN_FIELDS: [&str; 3] = ["Utilisateur", "Mot de passe", "Base"];

#[derive(Debug, Clone)]
pub struct LoginForm {
    pub server: DiscoveredServer,
    /// Name of the connection created for this login (set on submit).
    pub connection: String,
    pub fields: [LineInput; 3],
    pub field: usize,
    pub error: Option<String>,
}

impl LoginForm {
    pub fn new(server: DiscoveredServer) -> Self {
        let user = std::env::var("PGUSER").unwrap_or_else(|_| "postgres".into());
        Self {
            server,
            connection: String::new(),
            fields: [
                LineInput::with_text(user),
                LineInput::default(),
                LineInput::with_text("postgres"),
            ],
            // Most servers need a password: start there.
            field: 1,
            error: None,
        }
    }

    pub fn user(&self) -> &str {
        self.fields[0].text().trim()
    }

    pub fn password(&self) -> &str {
        self.fields[1].text()
    }

    pub fn database(&self) -> &str {
        self.fields[2].text().trim()
    }
}

/// What the picker shows at an index.
pub enum PickerItem<'a> {
    Configured(&'a ConnectionConfig),
    Discovered(&'a DiscoveredServer),
}

impl App {
    pub fn picker_len(&self) -> usize {
        self.servers.len() + self.discovered.len()
    }

    pub fn picker_item(&self, i: usize) -> Option<PickerItem<'_>> {
        match self.servers.get(i) {
            Some(s) => Some(PickerItem::Configured(&s.conn)),
            None => self
                .discovered
                .get(i - self.servers.len())
                .map(PickerItem::Discovered),
        }
    }

    /// Starts a scan of the local machine (picker, `R`).
    pub(super) fn discover(&mut self) -> Vec<Effect> {
        if !self.config.general.discover || self.discovering {
            return Vec::new();
        }
        self.discovering = true;
        vec![
            DbCommand::Discover {
                ports: self.config.general.discover_ports.clone(),
            }
            .into(),
        ]
    }

    pub(super) fn on_discovered(&mut self, servers: Vec<DiscoveredServer>) {
        self.discovering = false;
        self.discovered = servers;
        self.picker_cursor = self.picker_cursor.min(self.picker_len().saturating_sub(1));
        if self.discovered.is_empty() {
            let ports: Vec<String> = self
                .config
                .general
                .discover_ports
                .iter()
                .map(u16::to_string)
                .collect();
            self.notify(
                Level::Info,
                format!(
                    "Aucun serveur PostgreSQL local sur le port {}",
                    ports.join(", ")
                ),
            );
        }
    }

    pub(super) fn open_login(&mut self, server: DiscoveredServer) {
        self.overlay = Some(Overlay::Login(Box::new(LoginForm::new(server))));
    }

    pub(super) fn on_login_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Login(mut form)) = self.overlay.take() else {
            return Vec::new();
        };
        let back = key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::BackTab;
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                let n = LOGIN_FIELDS.len();
                form.field = if back || key.code == KeyCode::Up {
                    (form.field + n - 1) % n
                } else {
                    (form.field + 1) % n
                };
            }
            KeyCode::Enter => {
                if form.user().is_empty() {
                    form.error = Some("L'utilisateur est obligatoire".into());
                    form.field = 0;
                } else {
                    return self.submit_login(*form);
                }
            }
            _ => {
                if form.fields[form.field].handle_key(&key) == InputOutcome::Changed {
                    form.error = None;
                }
            }
        }
        self.overlay = Some(Overlay::Login(form));
        Vec::new()
    }

    fn submit_login(&mut self, form: LoginForm) -> Vec<Effect> {
        let conn = ConnectionConfig {
            host: form.server.host.clone(),
            port: form.server.port,
            user: Some(form.user().to_string()),
            database: Some(if form.database().is_empty() {
                "postgres".into()
            } else {
                form.database().into()
            }),
            sslmode: SslMode::Prefer,
            ..ConnectionConfig::named(self.unique_name(&form.server, form.user()))
        };
        // An empty password lets trust / peer authentication work.
        let password = (!form.password().is_empty()).then(|| form.password().to_string());
        let name = conn.name.clone();
        let mut form = form;
        form.connection = name.clone();
        self.login = Some(form);
        // The new connection appears in the tree, connecting.
        self.servers
            .push(super::server::Server::new(conn, None, false));
        self.screen = super::Screen::Main;
        self.focus = super::Focus::Tree;
        self.tree
            .expand_node(super::tree::NodeKey::Connection(name.clone()));
        let effects = self.connect_server(&name, password, false);
        self.tree.select(&super::tree::NodeKey::Connection(name));
        effects
    }

    /// `localhost`, `localhost-5433`, `socket`, ... unique among connections.
    fn unique_name(&self, server: &DiscoveredServer, user: &str) -> String {
        let mut base = if server.is_socket() {
            "socket".to_string()
        } else {
            "localhost".to_string()
        };
        if server.port != 5432 {
            base.push_str(&format!("-{}", server.port));
        }
        if user != "postgres" {
            base.push_str(&format!("-{user}"));
        }
        let taken = |n: &str| self.servers.iter().any(|s| s.conn.name == n);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|n| !taken(n))
            .unwrap_or(base)
    }

    /// The login failed: show the form again with the error.
    pub(super) fn login_failed(&mut self, connection: &str, error: String) -> bool {
        if self
            .login
            .as_ref()
            .is_none_or(|l| l.connection != connection)
        {
            return false;
        }
        let Some(mut form) = self.login.take() else {
            return false;
        };
        // The connection only existed for this attempt.
        self.servers
            .retain(|s| s.conn.name != connection || s.saved);
        self.refresh_tree();
        form.error = Some(error);
        form.fields[1].clear();
        form.field = 1;
        self.overlay = Some(Overlay::Login(Box::new(form)));
        true
    }

    /// The login succeeded: offer to save the server (without password).
    pub(super) fn login_succeeded(&mut self, conn: &crate::config::ConnectionConfig) {
        if self
            .login
            .as_ref()
            .is_none_or(|l| l.connection != conn.name)
        {
            return;
        }
        self.login = None;
        let known = self.servers.iter().any(|s| {
            let c = &s.conn;
            s.saved
                && c.host == conn.host
                && c.port == conn.port
                && c.user == conn.user
                && c.database == conn.database
        });
        if !known && self.config_path.is_some() {
            self.overlay = Some(Overlay::SaveServer(Box::new(conn.clone())));
        }
    }

    pub(super) fn on_save_server_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::SaveServer(conn)) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('o' | 'O' | 'y' | 'Y') | KeyCode::Enter => {
                if let Some(s) = self.server_mut(&conn.name) {
                    s.saved = true;
                }
                vec![Effect::SaveConnection(conn)]
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Vec::new(),
            _ => {
                self.overlay = Some(Overlay::SaveServer(conn));
                Vec::new()
            }
        }
    }
}
