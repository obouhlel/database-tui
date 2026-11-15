//! Application state and reducer.
//!
//! [`App::update`] is the only place where state changes: it takes a [`Msg`]
//! (key, DB event, tick) and returns [`Effect`]s (commands for the DB worker,
//! clipboard writes) that the event loop executes. It performs no I/O, which
//! keeps it unit-testable.

mod data_tab;
pub mod data_view;
pub mod discovery;
pub mod export;
pub mod filter_editor;
pub mod format;
pub mod grid;
pub mod input;
pub mod schema_view;
pub mod tree;
pub mod value;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

use crate::config::{Config, ConnectionConfig};
use crate::db::catalog::{DatabaseInfo, ObjectRef};
use crate::db::schema::ObjectDetails;
use crate::db::worker::{DbCommand, DbEvent};
use crate::db::{ConnectRequest, ServerInfo};
use crate::keymap::{Action, Context, KeyChord, KeyResolver, Keymap, Resolved};
use crate::ui::theme::Theme;
use data_view::DataView;
use input::{InputOutcome, LineInput};
use schema_view::{SchemaView, Section};
use tree::{DatabaseEntry, NodeKey, RowKind, TreeState};

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Tick,
    Db(DbEvent),
    /// SIGTERM / SIGHUP: leave immediately.
    Terminate,
    /// Feedback from the event loop (e.g. a clipboard failure).
    Notify(Level, String),
}

/// Side effects requested by the reducer, executed by the event loop.
#[derive(Debug)]
pub enum Effect {
    Db(DbCommand),
    /// Put text in the system clipboard.
    Copy(String),
    /// Append a connection to `config.toml`.
    SaveConnection(Box<ConnectionConfig>),
}

impl From<DbCommand> for Effect {
    fn from(c: DbCommand) -> Self {
        Effect::Db(c)
    }
}

/// Loading state of an object's details (Schema tab).
#[derive(Debug, Clone)]
pub enum DetailsState {
    Loading,
    Loaded(Arc<ObjectDetails>),
    Failed(String),
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
    Search,
    Visual,
    VisualLine,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Filter => "FILTER",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
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
    /// `/` in the Data tab; `previous` is restored on Esc.
    DataSearch {
        input: LineInput,
        previous: Option<String>,
    },
    /// `F`: free SQL condition.
    FreeWhere(LineInput),
    /// `f`: structured filter.
    FilterEditor(Box<filter_editor::FilterEditor>),
    /// `Y`: format chooser.
    CopyFormat {
        cursor: usize,
    },
    /// `Enter`: all fields of the current row.
    RowDetail {
        scroll: u16,
    },
    /// User / password / database for a discovered server.
    Login(Box<discovery::LoginForm>),
    /// Offer to save a discovered server in `config.toml`.
    SaveServer(Box<ConnectionConfig>),
}

impl Overlay {
    fn is_data(&self) -> bool {
        matches!(
            self,
            Overlay::DataSearch { .. }
                | Overlay::FreeWhere(_)
                | Overlay::FilterEditor(_)
                | Overlay::CopyFormat { .. }
                | Overlay::RowDetail { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub conn: ConnectionConfig,
    pub info: ServerInfo,
    /// Every database of the server, with its catalog once loaded.
    pub databases: Vec<DatabaseEntry>,
    /// Set when the server connection dropped.
    pub lost: Option<String>,
    /// Schema tab cache, cleared when the catalog is reloaded.
    pub details: HashMap<ObjectRef, DetailsState>,
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
    /// PostgreSQL servers found on the local machine.
    pub discovered: Vec<crate::db::discover::DiscoveredServer>,
    pub discovering: bool,
    /// Login in progress on a discovered server (kept to retry on failure).
    login: Option<discovery::LoginForm>,
    /// Where discovered servers are saved.
    pub config_path: Option<PathBuf>,
    /// Config / keymap warnings shown in the picker.
    pub warnings: Vec<String>,

    pub connecting: Option<(ConnectionConfig, Instant)>,
    pub session: Option<Session>,
    pub tree: TreeState,
    pub focus: Focus,
    pub tab: Tab,
    /// Object opened in the center panel.
    pub current: Option<ObjectRef>,
    pub schema: SchemaView,
    /// Objects visited before the current one (Ctrl+o goes back).
    pub back_stack: Vec<ObjectRef>,
    pub data: DataView,
    request_seq: u64,

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
            schema: SchemaView::default(),
            back_stack: Vec::new(),
            data: DataView::default(),
            request_seq: 0,
            overlay: None,
            status: None,
            busy: None,
            last_op: None,
            spinner: 0,
            should_quit: false,
            dirty: true,
            start: Some(start),
            discovered: Vec::new(),
            discovering: false,
            login: None,
            config_path: None,
        }
    }

    /// Commands to run at startup.
    pub fn init(&mut self) -> Vec<Effect> {
        match self.start.take() {
            Some(Start::Connect { conn, password }) => self.start_connect(*conn, password, true),
            _ => self.discover(),
        }
    }

    /// Returns whether a redraw is needed, and clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mode(&self) -> Mode {
        match &self.overlay {
            Some(Overlay::Command(_)) => Mode::Command,
            Some(Overlay::DataSearch { .. }) => Mode::Search,
            Some(Overlay::FilterEditor(_) | Overlay::FreeWhere(_)) => Mode::Filter,
            _ if self.tree.filter_editing => Mode::Filter,
            _ if self.tab == Tab::Data && self.focus == Focus::Center => match self.data.visual {
                Some(v) if v.line => Mode::VisualLine,
                Some(_) => Mode::Visual,
                None => Mode::Normal,
            },
            _ => Mode::Normal,
        }
    }

    /// Keymap contexts for the current focus, most specific first.
    pub fn contexts(&self) -> Vec<Context> {
        let ctx = match (self.screen, self.focus) {
            (Screen::Picker, _) => Context::Picker,
            (Screen::Main, Focus::Tree) => Context::Tree,
            (Screen::Main, Focus::Center) if self.tab == Tab::Schema => {
                return vec![Context::Schema, Context::Center, Context::Global];
            }
            (Screen::Main, Focus::Center) if self.tab == Tab::Data => {
                return vec![Context::Data, Context::Center, Context::Global];
            }
            (Screen::Main, Focus::Center) => Context::Center,
        };
        vec![ctx, Context::Global]
    }

    pub fn pending_keys(&self) -> Option<String> {
        self.resolver.pending_display()
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let mut effects = self.handle(msg);
        effects.extend(self.sync_catalogs());
        effects.extend(self.sync_details());
        effects.extend(self.sync_data());
        effects
    }

    fn handle(&mut self, msg: Msg) -> Vec<Effect> {
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
            Msg::Notify(level, text) => {
                self.notify(level, text);
                Vec::new()
            }
        }
    }

    /// Requests the details of the object shown in the Schema tab when they
    /// are not cached, and feeds cached details to the Schema view.
    fn sync_details(&mut self) -> Vec<Effect> {
        let Some(obj) = self.current.clone() else {
            return Vec::new();
        };
        let Some(session) = &mut self.session else {
            return Vec::new();
        };
        match session.details.get(&obj) {
            Some(DetailsState::Loaded(d)) => {
                let shown = self
                    .schema
                    .details
                    .as_ref()
                    .is_some_and(|s| Arc::ptr_eq(s, d));
                if !shown {
                    self.schema.show(d.clone());
                    self.dirty = true;
                }
                Vec::new()
            }
            Some(_) => Vec::new(),
            None if matches!(self.tab, Tab::Schema | Tab::Data)
                && session.lost.is_none()
                && session
                    .databases
                    .iter()
                    .any(|d| d.name() == obj.database && d.catalog.is_some()) =>
            {
                session.details.insert(obj.clone(), DetailsState::Loading);
                if self.schema.object.as_ref() != Some(&obj) {
                    self.schema.clear();
                }
                vec![DbCommand::LoadDetails(obj).into()]
            }
            None => Vec::new(),
        }
    }

    /// Loads the catalog of every expanded database that has none yet.
    fn sync_catalogs(&mut self) -> Vec<Effect> {
        let Some(s) = &mut self.session else {
            return Vec::new();
        };
        if s.lost.is_some() {
            return Vec::new();
        }
        let include_system = self.config.general.show_system_schemas;
        let mut effects = Vec::new();
        for db in &mut s.databases {
            let wanted = self
                .tree
                .is_expanded(&NodeKey::Database(db.name().to_string()));
            if wanted
                && db.catalog.is_none()
                && !db.loading
                && db.error.is_none()
                && db.info.can_connect
            {
                db.loading = true;
                effects.push(
                    DbCommand::LoadCatalog {
                        database: db.name().to_string(),
                        include_system,
                    }
                    .into(),
                );
            }
        }
        if !effects.is_empty() {
            self.tree.rebuild(&s.databases);
            self.dirty = true;
        }
        effects
    }

    /// Database of the current object, else of the tree selection.
    pub fn current_database(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|o| o.database.as_str())
            .or_else(|| self.tree.selected_database())
            .or_else(|| self.session.as_ref().map(|s| s.info.database.as_str()))
    }

    /// Catalog of a database, if loaded.
    pub fn catalog(&self, database: &str) -> Option<&crate::db::catalog::Catalog> {
        self.session
            .as_ref()?
            .databases
            .iter()
            .find(|d| d.name() == database)?
            .catalog
            .as_ref()
    }

    /// Details state of the current object, for the renderer.
    pub fn current_details(&self) -> Option<&DetailsState> {
        let obj = self.current.as_ref()?;
        self.session.as_ref()?.details.get(obj)
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

    fn on_tick(&mut self) -> Vec<Effect> {
        let now = Instant::now();
        let mut cmds = Vec::new();
        if self.resolver.is_pending() {
            // Keeps the pending-keys indicator up to date.
            self.dirty = true;
        }
        if let Some(action) = self.resolver.check_timeout(now) {
            cmds = self.dispatch(action);
        }
        let loading_db = self
            .session
            .as_ref()
            .is_some_and(|s| s.databases.iter().any(|d| d.loading));
        if self.busy.is_some() || self.connecting.is_some() || self.discovering || loading_db {
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
                self.filter_changed();
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
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
            InputOutcome::Changed => self.filter_changed(),
            InputOutcome::Moved => {}
            InputOutcome::Ignored => match key.code {
                // Navigate matches without leaving the filter.
                KeyCode::Down => self.tree.move_by(1),
                KeyCode::Up => self.tree.move_by(-1),
                _ => {}
            },
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match &self.overlay {
            Some(Overlay::Login(_)) => return self.on_login_key(key),
            Some(Overlay::SaveServer(_)) => return self.on_save_server_key(key),
            _ => {}
        }
        if self.overlay.as_ref().is_some_and(Overlay::is_data) {
            return self.on_data_overlay_key(key);
        }
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
            // Routed to `on_data_overlay_key` above.
            Overlay::DataSearch { .. }
            | Overlay::FreeWhere(_)
            | Overlay::FilterEditor(_)
            | Overlay::CopyFormat { .. }
            | Overlay::RowDetail { .. }
            | Overlay::Login(_)
            | Overlay::SaveServer(_) => Vec::new(),
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

    fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        match action {
            Quit => self.should_quit = true,
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            CommandPalette => self.overlay = Some(Overlay::Command(LineInput::default())),
            Interrupt => {
                if let Some(effects) = self.cancel_data() {
                    return effects;
                }
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
            _ if self.tab == Tab::Schema => return self.dispatch_schema(action),
            _ if self.tab == Tab::Data => return self.dispatch_data(action),
            _ => {}
        }
        Vec::new()
    }

    fn dispatch_schema(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        match action {
            NextSection => self.schema.cycle_section(1),
            PrevSection => self.schema.cycle_section(-1),
            JumpBack => match self.back_stack.pop() {
                Some(prev) => self.go_to(prev),
                None => self.notify(Level::Info, "Pas d'objet précédent"),
            },
            FollowLink => {
                let target = self.schema.table().and_then(|t| t.selected_link()).cloned();
                match target {
                    Some(t) => {
                        if let Some(cur) = self.current.take() {
                            self.back_stack.push(cur);
                        }
                        self.go_to(t);
                    }
                    None => {
                        if matches!(
                            self.schema.current_section(),
                            Some(Section::Columns | Section::Relations)
                        ) {
                            self.notify(Level::Info, "Pas de table liée sur cette ligne");
                        }
                    }
                }
            }
            Copy => return self.copy_schema(),
            Refresh => {
                if let (Some(obj), Some(s)) = (&self.current, &mut self.session) {
                    s.details.remove(obj);
                }
            }
            _ => self.navigate_schema(action),
        }
        Vec::new()
    }

    fn navigate_schema(&mut self, action: Action) {
        use Action::*;
        if self.schema.current_section() == Some(Section::Ddl) {
            let lines = self.schema.ddl_lines;
            let t = &mut self.schema.ddl;
            let half = (t.viewport / 2).max(1) as isize;
            let page = t.viewport.saturating_sub(1).max(1) as isize;
            match action {
                Up => t.scroll_by(-1, lines),
                Down => t.scroll_by(1, lines),
                Top => t.scroll = 0,
                Bottom => t.scroll_by(isize::MAX / 2, lines),
                HalfPageDown => t.scroll_by(half, lines),
                HalfPageUp => t.scroll_by(-half, lines),
                PageDown => t.scroll_by(page, lines),
                PageUp => t.scroll_by(-page, lines),
                ScrollLeft => t.hscroll = t.hscroll.saturating_sub(4),
                ScrollRight => t.hscroll += 4,
                FirstColumn => t.hscroll = 0,
                _ => {}
            }
            return;
        }
        let Some(table) = self.schema.table_mut() else {
            return;
        };
        let rows = table.grid.rows.len();
        let cols = table.grid.headers.len();
        let frozen = table.grid.frozen;
        let st = &mut table.state;
        match action {
            Up => st.move_by(-1, rows),
            Down => st.move_by(1, rows),
            Top => st.top(),
            Bottom => st.bottom(rows),
            HalfPageDown => st.move_by(st.half_page(), rows),
            HalfPageUp => st.move_by(-st.half_page(), rows),
            PageDown => st.move_by(st.page(), rows),
            PageUp => st.move_by(-st.page(), rows),
            ScrollLeft => st.scroll_cols(-1, cols, frozen),
            ScrollRight => st.scroll_cols(1, cols, frozen),
            FirstColumn => st.col_offset = 0,
            LastColumn => st.scroll_cols(isize::MAX / 2, cols, frozen),
            _ => {}
        }
    }

    fn copy_schema(&mut self) -> Vec<Effect> {
        if self.schema.current_section() == Some(Section::Ddl) {
            let Some(d) = &self.schema.details else {
                return Vec::new();
            };
            let text = d.ddl.clone();
            self.notify(
                Level::Success,
                format!("DDL copié ({} lignes)", text.lines().count()),
            );
            return vec![Effect::Copy(text)];
        }
        let Some(t) = self.schema.table() else {
            return Vec::new();
        };
        match t.grid.row_tsv(t.state.row) {
            Some(text) => {
                self.notify(Level::Success, "Ligne copiée (TSV)");
                vec![Effect::Copy(text)]
            }
            None => Vec::new(),
        }
    }

    /// Opens `obj` in the center panel and reveals it in the tree.
    fn go_to(&mut self, obj: ObjectRef) {
        if let Some(s) = &self.session {
            if !self.tree.filter.is_empty() {
                self.tree.filter.clear();
            }
            self.tree.reveal(&s.databases, &obj);
        }
        self.current = Some(obj);
    }

    fn dispatch_picker(&mut self, action: Action) -> Vec<Effect> {
        if action == Action::Refresh {
            return self.discover();
        }
        let n = self.picker_len();
        if n == 0 {
            return Vec::new();
        }
        let page = 10;
        let cur = self.picker_cursor.min(n - 1);
        self.picker_cursor = match action {
            Action::Up => cur.saturating_sub(1),
            Action::Down => (cur + 1).min(n - 1),
            Action::Top => 0,
            Action::Bottom => n - 1,
            Action::PageUp | Action::HalfPageUp => cur.saturating_sub(page),
            Action::PageDown | Action::HalfPageDown => (cur + page).min(n - 1),
            Action::Connect if self.connecting.is_none() => {
                match self.picker_item(cur) {
                    Some(discovery::PickerItem::Configured(_)) => {
                        let (conn, password) = self.connections[cur].clone();
                        return self.start_connect(conn, password, true);
                    }
                    Some(discovery::PickerItem::Discovered(server)) => {
                        let server = server.clone();
                        self.open_login(server);
                    }
                    None => {}
                }
                cur
            }
            _ => cur,
        };
        Vec::new()
    }

    fn dispatch_tree(&mut self, action: Action) {
        let Some(catalog) = self.session.as_ref().map(|s| s.databases.as_slice()) else {
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
        if let Some(cur) = self.current.take()
            && cur != obj
        {
            self.back_stack.push(cur);
        }
        self.current = Some(obj);
        self.focus = Focus::Center;
    }

    fn filter_changed(&mut self) {
        if let Some(s) = &self.session {
            self.tree.filter_changed(&s.databases);
        }
    }

    fn refresh_tree(&mut self) {
        if let Some(s) = &self.session {
            self.tree.rebuild(&s.databases);
        }
    }

    // ------------------------------------------------------------- commands

    /// `:` command line. The full fuzzy palette comes in milestone 6.
    fn run_command(&mut self, line: &str) -> Vec<Effect> {
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
                self.schema = SchemaView::default();
                self.data = DataView::default();
                self.back_stack.clear();
                self.tree = TreeState::default();
                self.screen = Screen::Picker;
                let mut effects = vec![DbCommand::Disconnect.into()];
                effects.extend(self.discover());
                return effects;
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
    ) -> Vec<Effect> {
        let statement_timeout_secs = conn.statement_timeout(&self.config.general);
        self.connecting = Some((conn.clone(), Instant::now()));
        self.busy = Some(Busy {
            label: format!("Connexion à {}…", conn.name),
            since: Instant::now(),
        });
        vec![
            DbCommand::Connect {
                request: Box::new(ConnectRequest {
                    conn,
                    password,
                    statement_timeout_secs,
                }),
                resolve_password: resolve,
            }
            .into(),
        ]
    }

    /// Reloads every loaded catalog (and retries failed ones).
    fn reload_catalog(&mut self) -> Vec<Effect> {
        let Some(s) = &mut self.session else {
            return Vec::new();
        };
        if s.lost.is_some() {
            self.notify(Level::Warning, "Connexion perdue — :reconnect");
            return Vec::new();
        }
        let include_system = self.config.general.show_system_schemas;
        let mut effects = Vec::new();
        for db in &mut s.databases {
            if (db.catalog.is_some() || db.error.is_some()) && !db.loading {
                db.loading = true;
                db.error = None;
                effects.push(
                    DbCommand::LoadCatalog {
                        database: db.name().to_string(),
                        include_system,
                    }
                    .into(),
                );
            }
        }
        self.tree.rebuild(&s.databases);
        effects
    }

    fn on_db(&mut self, ev: DbEvent) -> Vec<Effect> {
        match ev {
            DbEvent::Connected {
                info,
                databases,
                elapsed,
            } => {
                let Some((conn, _)) = self.connecting.take() else {
                    return Vec::new();
                };
                self.busy = None;
                self.last_op = Some(LastOp {
                    label: "connexion".into(),
                    elapsed,
                });
                let previous = self.session.take().filter(|s| s.conn.name == conn.name);
                if previous.is_none() {
                    self.tree = TreeState::default();
                    self.current = None;
                    self.schema = SchemaView::default();
                    self.data = DataView::default();
                    self.back_stack.clear();
                    self.focus = Focus::Tree;
                }
                // The connected database first, then the others by name.
                let mut infos = databases;
                if !infos.iter().any(|d| d.name == info.database) {
                    infos.push(DatabaseInfo {
                        name: info.database.clone(),
                        comment: None,
                        size_bytes: None,
                        can_connect: true,
                    });
                }
                infos.sort_by_key(|d| (d.name != info.database, d.name.clone()));
                let databases: Vec<DatabaseEntry> = infos
                    .into_iter()
                    .map(|i| {
                        // A reconnection keeps the catalogs already loaded.
                        let old = previous
                            .as_ref()
                            .and_then(|p| p.databases.iter().find(|d| d.name() == i.name));
                        DatabaseEntry {
                            catalog: old.and_then(|d| d.catalog.clone()),
                            ..DatabaseEntry::new(i)
                        }
                    })
                    .collect();
                self.notify(
                    Level::Success,
                    format!(
                        "Connecté à {} (PostgreSQL {}{}) — {} base(s)",
                        conn.name,
                        info.version,
                        if info.tls { ", TLS" } else { "" },
                        databases.len()
                    ),
                );
                self.tree.expand_database(&info.database);
                self.tree.rebuild(&databases);
                self.login_succeeded(&conn);
                self.session = Some(Session {
                    conn,
                    info,
                    databases,
                    lost: None,
                    details: HashMap::new(),
                });
                self.screen = Screen::Main;
                // `sync_catalogs` loads the expanded database.
                Vec::new()
            }
            DbEvent::ConnectFailed {
                error,
                password_required,
            } => {
                self.busy = None;
                let Some((conn, _)) = self.connecting.take() else {
                    return Vec::new();
                };
                if self.login_failed(error.clone()) {
                    return Vec::new();
                }
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
                let count = catalog.object_count();
                self.last_op = Some(LastOp {
                    label: "catalogue".into(),
                    elapsed,
                });
                let Some(s) = &mut self.session else {
                    return Vec::new();
                };
                let db_name = catalog.database.clone();
                // Drop the current object if it no longer exists.
                if let Some(cur) = self.current.as_ref().filter(|c| c.database == db_name) {
                    let exists = catalog.schemas.iter().any(|sc| {
                        sc.name == cur.schema
                            && sc
                                .objects
                                .iter()
                                .any(|o| ObjectRef::new(&db_name, &sc.name, o) == *cur)
                    });
                    if !exists {
                        self.current = None;
                        self.schema = SchemaView::default();
                        self.data = DataView::default();
                    }
                }
                s.details.retain(|o, _| o.database != db_name);
                if let Some(db) = s.databases.iter_mut().find(|d| d.name() == db_name) {
                    db.catalog = Some(catalog);
                    db.loading = false;
                    db.error = None;
                }
                self.tree.rebuild(&s.databases);
                tracing::info!(database = %db_name, objects = count, ?elapsed, "catalog loaded");
                Vec::new()
            }
            DbEvent::CatalogFailed { database, error } => {
                if let Some(s) = &mut self.session {
                    if let Some(db) = s.databases.iter_mut().find(|d| d.name() == database) {
                        db.loading = false;
                        db.error = Some(error.clone());
                    }
                    self.tree.rebuild(&s.databases);
                }
                self.notify(Level::Error, format!("Base {database} : {error}"));
                Vec::new()
            }
            DbEvent::DetailsLoaded { details, elapsed } => {
                self.last_op = Some(LastOp {
                    label: "détails".into(),
                    elapsed,
                });
                if let Some(s) = &mut self.session {
                    s.details.insert(
                        details.object.clone(),
                        DetailsState::Loaded(Arc::new(*details)),
                    );
                }
                Vec::new()
            }
            DbEvent::DetailsFailed { object, error } => {
                if let Some(s) = &mut self.session {
                    s.details.insert(object, DetailsState::Failed(error));
                }
                Vec::new()
            }
            ev @ (DbEvent::RowsLoaded { .. }
            | DbEvent::RowsFailed { .. }
            | DbEvent::RowsCounted { .. }
            | DbEvent::CountFailed { .. }) => {
                self.on_data_event(ev);
                Vec::new()
            }
            DbEvent::Progress(label) => {
                if let Some(b) = &mut self.busy {
                    b.label = label;
                }
                Vec::new()
            }
            DbEvent::Discovered(servers) => {
                self.on_discovered(servers);
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
