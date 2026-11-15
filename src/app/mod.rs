//! Application state and reducer.
//!
//! [`App::update`] is the only place where state changes: it takes a [`Msg`]
//! (key, DB event, tick) and returns the [`DbCommand`]s to send to the worker.
//! It performs no I/O, which keeps it unit-testable.

pub mod input;
pub mod tree;

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

use crate::config::{Config, ConnectionConfig};
use crate::db::catalog::{Catalog, ObjectRef};
use crate::db::worker::{DbCommand, DbEvent};
use crate::db::{ConnectRequest, ServerInfo};
use crate::keymap::{Action, Context, KeyChord, KeyResolver, Keymap, Resolved};
use crate::ui::theme::Theme;
use input::{InputOutcome, LineInput};
use tree::{RowKind, TreeState};

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Tick,
    Db(DbEvent),
    /// SIGTERM / SIGHUP: leave immediately.
    Terminate,
}

impl Msg {
    pub fn from_terminal(ev: Event) -> Option<Self> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => Some(Msg::Key(k)),
            Event::Paste(s) => Some(Msg::Paste(s)),
            Event::Resize(..) => Some(Msg::Resize),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Picker,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Schema,
    Data,
    Sql,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Schema, Tab::Data, Tab::Sql];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Schema => "Schema",
            Tab::Data => "Data",
            Tab::Sql => "SQL",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn cycle(self, delta: isize) -> Tab {
        let n = Tab::ALL.len() as isize;
        Tab::ALL[(self.index() as isize + delta).rem_euclid(n) as usize]
    }
}

/// Mode shown in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Command,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Filter => "FILTER",
            Mode::Command => "COMMAND",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub level: Level,
    pub at: Instant,
}

/// Modal layer drawn on top of the screen; it receives keys first.
#[derive(Debug, Clone)]
pub enum Overlay {
    Help {
        scroll: u16,
    },
    Command(LineInput),
    Password {
        conn: Box<ConnectionConfig>,
        input: LineInput,
        error: Option<String>,
    },
    Error {
        title: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct Session {
    pub conn: ConnectionConfig,
    pub info: ServerInfo,
    pub catalog: Option<Catalog>,
    pub catalog_loading: bool,
    /// Set when the server connection dropped.
    pub lost: Option<String>,
}

/// An in-flight background operation, for the spinner.
#[derive(Debug, Clone)]
pub struct Busy {
    pub label: String,
    pub since: Instant,
}

/// Last completed operation (duration shown in the status bar).
#[derive(Debug, Clone)]
pub struct LastOp {
    pub label: String,
    pub elapsed: Duration,
}

/// What to do at startup.
#[derive(Debug, Clone)]
pub enum Start {
    Picker,
    Connect {
        conn: Box<ConnectionConfig>,
        password: Option<String>,
    },
}

pub struct App {
    pub config: Config,
    pub keymap: Keymap,
    pub theme: Theme,
    resolver: KeyResolver,

    pub screen: Screen,
    /// Connections listed in the picker (config + `DATABASE_URL`).
    pub connections: Vec<(ConnectionConfig, Option<String>)>,
    pub picker_cursor: usize,
    /// Config / keymap warnings shown in the picker.
    pub warnings: Vec<String>,

    pub connecting: Option<(ConnectionConfig, Instant)>,
    pub session: Option<Session>,
    pub tree: TreeState,
    pub focus: Focus,
    pub tab: Tab,
    /// Object opened in the center panel.
    pub current: Option<ObjectRef>,

    pub overlay: Option<Overlay>,
    pub status: Option<StatusMessage>,
    pub busy: Option<Busy>,
    pub last_op: Option<LastOp>,
    pub spinner: usize,
    pub should_quit: bool,
    dirty: bool,
    start: Option<Start>,
}

const MESSAGE_TTL: Duration = Duration::from_secs(6);

impl App {
    pub fn new(
        config: Config,
        keymap: Keymap,
        theme: Theme,
        warnings: Vec<String>,
        extra: Vec<(ConnectionConfig, Option<String>)>,
        start: Start,
    ) -> Self {
        let mut connections = extra;
        connections.extend(config.connections.iter().cloned().map(|c| (c, None)));
        let picker_cursor = config
            .general
            .default_connection
            .as_ref()
            .and_then(|d| connections.iter().position(|(c, _)| &c.name == d))
            .unwrap_or(0);
        Self {
            config,
            keymap,
            theme,
            resolver: KeyResolver::default(),
            screen: Screen::Picker,
            connections,
            picker_cursor,
            warnings,
            connecting: None,
            session: None,
            tree: TreeState::default(),
            focus: Focus::Tree,
            tab: Tab::Schema,
            current: None,
            overlay: None,
            status: None,
            busy: None,
            last_op: None,
            spinner: 0,
            should_quit: false,
            dirty: true,
            start: Some(start),
        }
    }

    /// Commands to run at startup.
    pub fn init(&mut self) -> Vec<DbCommand> {
        match self.start.take() {
            Some(Start::Connect { conn, password }) => self.start_connect(*conn, password, true),
            _ => Vec::new(),
        }
    }

    /// Returns whether a redraw is needed, and clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mode(&self) -> Mode {
        match &self.overlay {
            Some(Overlay::Command(_)) => Mode::Command,
            _ if self.tree.filter_editing => Mode::Filter,
            _ => Mode::Normal,
        }
    }

    /// Keymap contexts for the current focus, most specific first.
    pub fn contexts(&self) -> Vec<Context> {
        let ctx = match (self.screen, self.focus) {
            (Screen::Picker, _) => Context::Picker,
            (Screen::Main, Focus::Tree) => Context::Tree,
            (Screen::Main, Focus::Center) => Context::Center,
        };
        vec![ctx, Context::Global]
    }

    pub fn pending_keys(&self) -> Option<String> {
        self.resolver.pending_display()
    }

    pub fn update(&mut self, msg: Msg) -> Vec<DbCommand> {
        match msg {
            Msg::Key(k) => {
                self.dirty = true;
                self.on_key(k)
            }
            Msg::Paste(s) => {
                self.dirty = true;
                self.on_paste(&s);
                Vec::new()
            }
            Msg::Resize => {
                self.dirty = true;
                Vec::new()
            }
            Msg::Tick => self.on_tick(),
            Msg::Db(ev) => {
                self.dirty = true;
                self.on_db(ev)
            }
            Msg::Terminate => {
                self.should_quit = true;
                Vec::new()
            }
        }
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        match level {
            Level::Error => tracing::error!("{text}"),
            Level::Warning => tracing::warn!("{text}"),
            _ => tracing::info!("{text}"),
        }
        self.status = Some(StatusMessage {
            text,
            level,
            at: Instant::now(),
        });
        self.dirty = true;
    }

    fn on_tick(&mut self) -> Vec<DbCommand> {
        let now = Instant::now();
        let mut cmds = Vec::new();
        if self.resolver.is_pending() {
            // Keeps the pending-keys indicator up to date.
            self.dirty = true;
        }
        if let Some(action) = self.resolver.check_timeout(now) {
            cmds = self.dispatch(action);
        }
        if self.busy.is_some() || self.connecting.is_some() {
            self.spinner = self.spinner.wrapping_add(1);
            self.dirty = true;
        }
        if let Some(m) = &self.status
            && m.level != Level::Error
            && now.duration_since(m.at) > MESSAGE_TTL
        {
            self.status = None;
            self.dirty = true;
        }
        cmds
    }

    fn on_paste(&mut self, s: &str) {
        match &mut self.overlay {
            Some(Overlay::Command(input)) | Some(Overlay::Password { input, .. }) => {
                input.insert_str(s)
            }
            _ if self.tree.filter_editing => {
                self.tree.filter.insert_str(s);
                self.refresh_tree();
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) -> Vec<DbCommand> {
        if self.overlay.is_some() {
            return self.on_overlay_key(key);
        }
        if self.tree.filter_editing {
            self.on_filter_key(key);
            return Vec::new();
        }
        let contexts = self.contexts();
        match self
            .resolver
            .feed(&self.keymap, &contexts, KeyChord::from_event(&key))
        {
            Resolved::Action(a) => self.dispatch(a),
            Resolved::Pending => Vec::new(),
            Resolved::Unbound => {
                if key.code == KeyCode::Esc {
                    self.status = None;
                }
                Vec::new()
            }
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match self.tree.filter.handle_key(&key) {
            InputOutcome::Submit => self.tree.filter_editing = false,
            InputOutcome::Cancel => {
                self.tree.filter_editing = false;
                self.tree.filter.clear();
                self.refresh_tree();
            }
            InputOutcome::Changed => self.refresh_tree(),
            InputOutcome::Moved => {}
            InputOutcome::Ignored => match key.code {
                // Navigate matches without leaving the filter.
                KeyCode::Down => self.tree.move_by(1),
                KeyCode::Up => self.tree.move_by(-1),
                _ => {}
            },
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Vec<DbCommand> {
        let Some(overlay) = self.overlay.as_mut() else {
            return Vec::new();
        };
        match overlay {
            Overlay::Help { scroll } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::F(1) => {
                        self.overlay = None
                    }
                    KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                    _ => {}
                }
                Vec::new()
            }
            Overlay::Error { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.overlay = None;
                }
                Vec::new()
            }
            Overlay::Command(input) => match input.handle_key(&key) {
                InputOutcome::Submit => {
                    let line = input.text().trim().to_string();
                    self.overlay = None;
                    self.run_command(&line)
                }
                InputOutcome::Cancel => {
                    self.overlay = None;
                    Vec::new()
                }
                _ if key.code == KeyCode::Backspace && input.is_empty() => {
                    self.overlay = None;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Overlay::Password { conn, input, .. } => match input.handle_key(&key) {
                InputOutcome::Submit => {
                    let password = input.text().to_string();
                    let conn = (**conn).clone();
                    self.overlay = None;
                    self.start_connect(conn, Some(password), false)
                }
                InputOutcome::Cancel => {
                    self.overlay = None;
                    self.connecting = None;
                    Vec::new()
                }
                _ => Vec::new(),
            },
        }
    }

    // -------------------------------------------------------------- actions

    fn dispatch(&mut self, action: Action) -> Vec<DbCommand> {
        use Action::*;
        match action {
            Quit => self.should_quit = true,
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            CommandPalette => self.overlay = Some(Overlay::Command(LineInput::default())),
            Interrupt => {
                let quit = self
                    .keymap
                    .hint(Context::Global, Quit)
                    .unwrap_or_else(|| ":q".into());
                self.notify(
                    Level::Info,
                    format!("Rien à annuler — {quit} ou :q pour quitter"),
                );
            }
            _ if self.screen == Screen::Picker => return self.dispatch_picker(action),
            FocusTree => self.focus = Focus::Tree,
            FocusCenter => self.focus = Focus::Center,
            FocusNext => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Center,
                    Focus::Center => Focus::Tree,
                }
            }
            NextTab => self.tab = self.tab.cycle(1),
            PrevTab => self.tab = self.tab.cycle(-1),
            TabSchema => self.tab = Tab::Schema,
            TabData => self.tab = Tab::Data,
            TabSql => self.tab = Tab::Sql,
            ReloadCatalog => return self.reload_catalog(),
            _ if self.focus == Focus::Tree => self.dispatch_tree(action),
            _ => {}
        }
        Vec::new()
    }

    fn dispatch_picker(&mut self, action: Action) -> Vec<DbCommand> {
        let n = self.connections.len();
        if n == 0 {
            return Vec::new();
        }
        let page = 10;
        let cur = self.picker_cursor;
        self.picker_cursor = match action {
            Action::Up => cur.saturating_sub(1),
            Action::Down => (cur + 1).min(n - 1),
            Action::Top => 0,
            Action::Bottom => n - 1,
            Action::PageUp | Action::HalfPageUp => cur.saturating_sub(page),
            Action::PageDown | Action::HalfPageDown => (cur + page).min(n - 1),
            Action::Connect if self.connecting.is_none() => {
                let (conn, password) = self.connections[cur].clone();
                return self.start_connect(conn, password, true);
            }
            _ => cur,
        };
        Vec::new()
    }

    fn dispatch_tree(&mut self, action: Action) {
        let Some(catalog) = self.session.as_ref().and_then(|s| s.catalog.as_ref()) else {
            return;
        };
        let tree = &mut self.tree;
        match action {
            Action::Up => tree.move_by(-1),
            Action::Down => tree.move_by(1),
            Action::Top => tree.top(),
            Action::Bottom => tree.bottom(),
            Action::HalfPageDown => tree.move_by(tree.half_page()),
            Action::HalfPageUp => tree.move_by(-tree.half_page()),
            Action::PageDown => tree.move_by(tree.page()),
            Action::PageUp => tree.move_by(-tree.page()),
            Action::Expand => tree.expand(catalog),
            Action::Collapse => tree.collapse(catalog),
            Action::CollapseAll => tree.collapse_all(catalog),
            Action::Filter => tree.filter_editing = true,
            Action::ClearFilter => {
                if !tree.filter.is_empty() {
                    tree.filter.clear();
                    tree.rebuild(catalog);
                }
            }
            Action::Open => match tree.selected().map(|r| r.kind) {
                Some(RowKind::Object(_)) => {
                    let obj = tree.selected_object().cloned();
                    if let Some(obj) = obj {
                        // Clearing the filter keeps the object in context.
                        if !tree.filter.is_empty() {
                            tree.filter.clear();
                            tree.reveal(catalog, &obj);
                        }
                        self.open_object(obj);
                    }
                }
                Some(_) => tree.toggle(catalog),
                None => {}
            },
            _ => {}
        }
    }

    fn open_object(&mut self, obj: ObjectRef) {
        if !obj.kind.has_rows() && self.tab == Tab::Data {
            self.tab = Tab::Schema;
        }
        self.current = Some(obj);
        self.focus = Focus::Center;
    }

    fn refresh_tree(&mut self) {
        if let Some(catalog) = self.session.as_ref().and_then(|s| s.catalog.as_ref()) {
            self.tree.rebuild(catalog);
        }
    }

    // ------------------------------------------------------------- commands

    /// `:` command line. The full fuzzy palette comes in milestone 6.
    fn run_command(&mut self, line: &str) -> Vec<DbCommand> {
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else {
            return Vec::new();
        };
        let arg = parts.next();
        match cmd {
            "q" | "quit" | "q!" | "quit!" | "qa" | "qa!" => self.should_quit = true,
            "h" | "help" => self.overlay = Some(Overlay::Help { scroll: 0 }),
            "reload" | "refresh" => return self.reload_catalog(),
            "reconnect" => {
                if let Some(s) = &self.session {
                    let conn = s.conn.clone();
                    return self.start_connect(conn, None, true);
                }
                self.notify(Level::Warning, "Aucune session à reconnecter");
            }
            "connections" | "disconnect" => {
                self.session = None;
                self.current = None;
                self.tree = TreeState::default();
                self.screen = Screen::Picker;
                return vec![DbCommand::Disconnect];
            }
            "connect" | "c" => {
                let Some(name) = arg else {
                    self.notify(Level::Warning, "Usage : :connect <nom>");
                    return Vec::new();
                };
                match self
                    .connections
                    .iter()
                    .find(|(c, _)| c.name == name)
                    .cloned()
                {
                    Some((conn, pw)) => return self.start_connect(conn, pw, true),
                    None => self.notify(Level::Error, format!("Connexion inconnue : {name}")),
                }
            }
            "schema" => self.tab = Tab::Schema,
            "data" => self.tab = Tab::Data,
            "sql" => self.tab = Tab::Sql,
            other => self.notify(Level::Error, format!("Commande inconnue : {other}")),
        }
        Vec::new()
    }

    pub fn command_names() -> &'static [(&'static str, &'static str)] {
        &[
            ("q, quit", "Quitter"),
            ("help", "Aide"),
            ("reload", "Recharger le catalogue"),
            ("connect <nom>", "Se connecter à une autre connexion"),
            ("connections", "Revenir au sélecteur (déconnecte)"),
            ("reconnect", "Rouvrir la session courante"),
            ("schema | data | sql", "Changer d'onglet"),
        ]
    }

    // ------------------------------------------------------------------- db

    fn start_connect(
        &mut self,
        conn: ConnectionConfig,
        password: Option<String>,
        resolve: bool,
    ) -> Vec<DbCommand> {
        let statement_timeout_secs = conn.statement_timeout(&self.config.general);
        self.connecting = Some((conn.clone(), Instant::now()));
        self.busy = Some(Busy {
            label: format!("Connexion à {}…", conn.name),
            since: Instant::now(),
        });
        vec![DbCommand::Connect {
            request: Box::new(ConnectRequest {
                conn,
                password,
                statement_timeout_secs,
            }),
            resolve_password: resolve,
        }]
    }

    fn reload_catalog(&mut self) -> Vec<DbCommand> {
        let Some(s) = &mut self.session else {
            return Vec::new();
        };
        if s.lost.is_some() {
            self.notify(Level::Warning, "Connexion perdue — :reconnect");
            return Vec::new();
        }
        s.catalog_loading = true;
        self.busy = Some(Busy {
            label: "Chargement du catalogue…".into(),
            since: Instant::now(),
        });
        vec![DbCommand::LoadCatalog {
            include_system: self.config.general.show_system_schemas,
        }]
    }

    fn on_db(&mut self, ev: DbEvent) -> Vec<DbCommand> {
        match ev {
            DbEvent::Connected { info, elapsed } => {
                let Some((conn, _)) = self.connecting.take() else {
                    return Vec::new();
                };
                self.busy = None;
                self.last_op = Some(LastOp {
                    label: "connexion".into(),
                    elapsed,
                });
                let same = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.conn.name == conn.name);
                if !same {
                    self.tree = TreeState::default();
                    self.current = None;
                    self.focus = Focus::Tree;
                }
                let catalog = if same {
                    self.session.take().and_then(|s| s.catalog)
                } else {
                    None
                };
                self.notify(
                    Level::Success,
                    format!(
                        "Connecté à {} (PostgreSQL {}{})",
                        conn.name,
                        info.version,
                        if info.tls { ", TLS" } else { "" }
                    ),
                );
                self.session = Some(Session {
                    conn,
                    info,
                    catalog,
                    catalog_loading: false,
                    lost: None,
                });
                self.screen = Screen::Main;
                self.reload_catalog()
            }
            DbEvent::ConnectFailed {
                error,
                password_required,
            } => {
                self.busy = None;
                let Some((conn, _)) = self.connecting.take() else {
                    return Vec::new();
                };
                if password_required {
                    self.overlay = Some(Overlay::Password {
                        conn: Box::new(conn),
                        input: LineInput::default(),
                        error: Some(error),
                    });
                } else {
                    tracing::warn!(connection = %conn.name, "connection failed: {error}");
                    self.overlay = Some(Overlay::Error {
                        title: format!("Connexion à {} impossible", conn.name),
                        message: error,
                    });
                }
                Vec::new()
            }
            DbEvent::CatalogLoaded { catalog, elapsed } => {
                self.busy = None;
                let count = catalog.object_count();
                self.last_op = Some(LastOp {
                    label: "catalogue".into(),
                    elapsed,
                });
                if let Some(s) = &mut self.session {
                    s.catalog_loading = false;
                    self.tree.rebuild(&catalog);
                    // Drop the current object if it no longer exists.
                    if let Some(cur) = &self.current {
                        let exists = catalog.schemas.iter().any(|sc| {
                            sc.name == cur.schema
                                && sc
                                    .objects
                                    .iter()
                                    .any(|o| ObjectRef::new(&sc.name, o) == *cur)
                        });
                        if !exists {
                            self.current = None;
                        }
                    }
                    s.catalog = Some(catalog);
                }
                tracing::info!(objects = count, ?elapsed, "catalog loaded");
                Vec::new()
            }
            DbEvent::CatalogFailed(e) => {
                self.busy = None;
                if let Some(s) = &mut self.session {
                    s.catalog_loading = false;
                }
                self.notify(Level::Error, format!("Catalogue : {e}"));
                Vec::new()
            }
            DbEvent::ConnectionLost(e) => {
                if let Some(s) = &mut self.session {
                    s.lost = Some(e.clone());
                }
                self.busy = None;
                self.notify(Level::Error, format!("Connexion perdue : {e} — :reconnect"));
                Vec::new()
            }
        }
    }

    /// Schema shown in the status bar.
    pub fn current_schema(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|o| o.schema.as_str())
            .or_else(|| self.tree.selected_schema())
    }
}

#[cfg(test)]
mod tests;
