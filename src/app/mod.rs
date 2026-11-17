//! Application state and reducer.
//!
//! [`App::update`] is the only place where state changes: it takes a [`Msg`]
//! (key, DB event, tick) and returns [`Effect`]s (commands for the DB worker,
//! clipboard writes) that the event loop executes. It performs no I/O, which
//! keeps it unit-testable.

pub mod completion;
mod data_tab;
pub mod data_view;
pub mod discovery;
pub mod edit;
pub mod editor;
pub mod export;
pub mod export_dialog;
pub mod filter_editor;
pub mod follow;
pub mod format;
pub mod grid;
pub mod input;
pub mod palette;
pub mod plan;
pub mod reference;
pub mod schema_view;
pub mod server;
mod servers;
pub mod sql_tab;
pub mod staging;
pub mod textarea;
pub mod tree;
pub mod validate;
pub mod value;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

use crate::config::{Config, ConnectionConfig};
use crate::db::catalog::ObjectRef;
use crate::db::schema::ObjectDetails;
use crate::db::worker::{DbCommand, DbEvent};
use crate::keymap::{Action, Context, KeyChord, KeyResolver, Keymap, Resolved};
use crate::ui::theme::Theme;
use data_view::DataView;
use input::{InputOutcome, LineInput};
use schema_view::{SchemaView, Section};
use tree::{NodeKey, RowKind, TreeState};

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
    /// Saved queries found on disk (`:open` without a name).
    QueriesListed(Vec<String>),
    QueryLoaded {
        name: String,
        text: String,
    },
}

/// Side effects requested by the reducer, executed by the event loop.
#[derive(Debug)]
pub enum Effect {
    Db(DbCommand),
    /// Put text in the system clipboard.
    Copy(String),
    /// Append a connection to `config.toml`.
    SaveConnection(Box<ConnectionConfig>),
    /// Remove a connection from `config.toml`.
    DeleteConnection(String),
    /// Append a statement to the history file.
    AppendHistory(Box<sql_tab::HistoryEntry>),
    /// Write an export built in memory (SQL results).
    WriteFile {
        path: PathBuf,
        text: String,
        /// Success message.
        done: String,
    },
    /// Saved queries (`<data dir>/queries/<name>.sql`).
    SaveQuery {
        name: String,
        text: String,
    },
    LoadQuery(String),
    ListQueries,
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
    /// Editing a cell of an existing row.
    Edit,
    /// Editing a cell of a new row (or vim insert mode in the SQL editor).
    Insert,
    /// Typing in the classic SQL editor.
    Sql,
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
            Mode::Edit => "EDIT",
            Mode::Insert => "INSERT",
            Mode::Sql => "SQL",
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
    /// `K`: the row a foreign key cell points to.
    Reference(Box<reference::ReferencePeek>),
    /// User / password / database for a discovered server.
    Login(Box<discovery::LoginForm>),
    /// `dd` on a connection: confirm its removal.
    ConfirmDelete(String),
    /// Offer to save a discovered server in `config.toml`.
    SaveServer(Box<ConnectionConfig>),
    /// `e`: value of a cell (Data tab).
    CellEditor(Box<edit::CellEditor>),
    /// `D`: pending changes (diff and generated SQL).
    Changes(Box<edit::ChangesView>),
    /// `Ctrl+s`: confirm applying the pending changes; on production the
    /// connection name must be typed. `script`: hand-edited SQL.
    ConfirmApply {
        script: Option<String>,
        production: bool,
        input: LineInput,
        counts: crate::db::changes::ChangeCounts,
        /// Overlay to restore on cancel (the diff view).
        previous: Option<Box<Overlay>>,
    },
    /// `Ctrl+z` / `X`: confirm discarding the pending changes.
    ConfirmDiscard,
    /// Leaving the table with pending changes.
    PendingChanges(edit::After),
    /// `UPDATE` / `DELETE` without `WHERE` in what is about to run.
    ConfirmWrites {
        statements: Vec<(String, usize)>,
        target: (String, String),
        risky: Vec<String>,
    },
    /// Quitting / disconnecting with an open SQL transaction.
    ConfirmTxnExit(edit::After),
    /// `Ctrl+r` in the SQL tab.
    HistorySearch(Box<sql_tab::HistorySearch>),
    /// `:open` without a name.
    QueryList {
        names: Vec<String>,
        cursor: usize,
    },
    /// `E`: export to a file.
    Export(Box<export_dialog::ExportDialog>),
    /// `gd` with several targets, `gr`: where to jump.
    JumpList {
        title: String,
        jumps: Vec<follow::Jump>,
        cursor: usize,
    },
}

impl Overlay {
    pub fn is_data(&self) -> bool {
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
    /// Connections of the tree (config, `--url`, `DATABASE_URL`, added).
    pub servers: Vec<server::Server>,
    pub picker_cursor: usize,
    /// PostgreSQL servers found on the local machine.
    pub discovered: Vec<crate::db::discover::DiscoveredServer>,
    pub discovering: bool,
    /// Login in progress on a discovered server (kept to retry on failure).
    login: Option<discovery::LoginForm>,
    /// Where discovered servers are saved.
    pub config_path: Option<PathBuf>,
    /// History and saved queries live here.
    pub data_dir: Option<PathBuf>,
    /// Default directory of exports (the working directory).
    pub export_dir: Option<PathBuf>,
    /// Virtual foreign keys (`relations.toml`), merged into loaded details.
    pub relations: crate::config::relations::Relations,
    /// `gd` / `gr` in flight: the filter to set once the target's Data
    /// view exists.
    pending_jump: Option<follow::Jump>,
    /// Data filters of the tables left by a jump, restored on the way back.
    saved_filters: HashMap<ObjectRef, crate::db::data::Filter>,
    /// Id of the export in flight.
    exporting: Option<u64>,
    /// Config / keymap warnings shown in the picker.
    pub warnings: Vec<String>,

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
    /// The overlay belongs to the active SQL result (see `with_results`).
    pub overlay_in_results: bool,
    /// The apply in flight comes from this SQL result.
    apply_in_results: Option<usize>,
    /// Reload a SQL result after applying its changes.
    rerun_results: bool,
    /// Suggestion chosen in the command palette (`None`: the typed text).
    pub palette_cursor: Option<usize>,
    /// Action postponed until the pending changes are applied.
    after_apply: Option<edit::After>,
    /// What each change of the apply in flight refers to.
    apply_origins: Vec<staging::Origin>,
    pub sql: sql_tab::SqlState,
    /// `data` currently holds a SQL result (swapped in).
    pub results_mode: bool,

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
        let mut servers: Vec<server::Server> = extra
            .into_iter()
            .map(|(c, pw)| server::Server::new(c, pw, false))
            .collect();
        servers.extend(
            config
                .connections
                .iter()
                .cloned()
                .map(|c| server::Server::new(c, None, true)),
        );
        let default = config.general.default_connection.clone();
        let mut tree = TreeState::default();
        tree.rebuild(&servers);
        if let Some(d) = &default {
            tree.select(&NodeKey::Connection(d.clone()));
        }
        let sql = sql_tab::SqlState::new(config.editor.mode, config.editor.tab_width as usize);
        Self {
            sql,
            config,
            keymap,
            theme,
            resolver: KeyResolver::default(),
            screen: Screen::Main,
            servers,
            picker_cursor: 0,
            warnings,
            tree,
            focus: Focus::Tree,
            tab: Tab::Schema,
            current: None,
            schema: SchemaView::default(),
            back_stack: Vec::new(),
            data: DataView::default(),
            request_seq: 0,
            after_apply: None,
            palette_cursor: None,
            overlay_in_results: false,
            apply_in_results: None,
            rerun_results: false,
            apply_origins: Vec::new(),
            results_mode: false,
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
            data_dir: None,
            export_dir: None,
            relations: Default::default(),
            pending_jump: None,
            saved_filters: HashMap::new(),
            exporting: None,
        }
    }

    /// Commands to run at startup.
    pub fn init(&mut self) -> Vec<Effect> {
        if let Some(Start::Connect { conn, password }) = self.start.take() {
            if self.server(&conn.name).is_none() {
                self.servers
                    .insert(0, server::Server::new(*conn.clone(), password, false));
            }
            self.open_connection(&conn.name);
        }
        // Servers of this machine are listed on the connections page.
        let mut effects = self.discover();
        effects.extend(self.sync_connections());
        effects
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
            Some(Overlay::CellEditor(e)) => match e.target {
                edit::EditTarget::Insert(_) => Mode::Insert,
                edit::EditTarget::Existing(_) => Mode::Edit,
            },
            _ if self.tree.filter_editing => Mode::Filter,
            _ if self.sql_editor_focused() => match (self.sql.editor.mode, self.sql.editor.vim) {
                (crate::config::EditorMode::Classic, _) => Mode::Sql,
                (_, editor::VimMode::Insert) => Mode::Insert,
                (_, editor::VimMode::Visual) => Mode::Visual,
                (_, editor::VimMode::VisualLine) => Mode::VisualLine,
                (_, editor::VimMode::Normal) => Mode::Normal,
            },
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
            (Screen::Main, Focus::Center) if self.tab == Tab::Sql => {
                return match self.sql.focus {
                    sql_tab::SqlFocus::Editor => vec![Context::SqlEditor, Context::Global],
                    sql_tab::SqlFocus::Results => vec![
                        Context::SqlResults,
                        Context::Data,
                        Context::Center,
                        Context::Global,
                    ],
                };
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
        effects.extend(self.sync_connections());
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
            Msg::QueriesListed(names) => {
                self.dirty = true;
                self.on_queries_listed(names);
                Vec::new()
            }
            Msg::QueryLoaded { name, text } => {
                self.dirty = true;
                self.on_query_loaded(name, text);
                Vec::new()
            }
        }
    }

    /// Requests the details of the object shown in the Schema / Data tab
    /// when they are not cached, and feeds cached details to the Schema view.
    fn sync_details(&mut self) -> Vec<Effect> {
        let Some(obj) = self.current.clone() else {
            return Vec::new();
        };
        let wanted = matches!(self.tab, Tab::Schema | Tab::Data);
        let Some(s) = self
            .servers
            .iter_mut()
            .find(|s| s.conn.name == obj.connection)
        else {
            return Vec::new();
        };
        let catalog_loaded = s
            .database(&obj.database)
            .is_some_and(|d| d.catalog.is_some());
        match s.details.get(&obj) {
            Some(DetailsState::Loaded(d)) => {
                let shown = self
                    .schema
                    .details
                    .as_ref()
                    .is_some_and(|x| Arc::ptr_eq(x, d));
                if !shown {
                    self.schema.show(d.clone());
                    self.dirty = true;
                }
                Vec::new()
            }
            Some(_) => Vec::new(),
            None if wanted && s.is_connected() && catalog_loaded => {
                s.details.insert(obj.clone(), DetailsState::Loading);
                if self.schema.object.as_ref() != Some(&obj) {
                    self.schema.clear();
                }
                vec![DbCommand::LoadDetails(obj).into()]
            }
            None => Vec::new(),
        }
    }

    /// Details state of the current object, for the renderer.
    pub fn current_details(&self) -> Option<&DetailsState> {
        let obj = self.current.as_ref()?;
        self.server(&obj.connection)?.details.get(obj)
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
        let loading = self.servers.iter().any(|s| {
            matches!(s.state, server::ConnState::Connecting(_))
                || s.databases.iter().any(|d| d.loading)
        });
        if self.busy.is_some() || self.discovering || loading {
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
        let sql_editor = self.sql_editor_focused();
        // Single-line inputs: line breaks become nothing, and the blanks
        // around a copied value (a cell copy ends with a newline) are dropped.
        let line = s.trim();
        match &mut self.overlay {
            Some(Overlay::Command(input)) | Some(Overlay::Password { input, .. }) => {
                input.insert_str(line)
            }
            _ if self.tree.filter_editing => {
                self.tree.filter.insert_str(line);
                self.filter_changed();
            }
            Some(Overlay::CellEditor(ed)) => match &mut ed.input {
                edit::EditorInput::Line(l) => l.insert_str(s),
                edit::EditorInput::Multi(t) => t.insert_str(s),
            },
            Some(Overlay::FilterEditor(editor)) => editor.paste(s),
            Some(Overlay::FreeWhere(input)) => input.insert_str(&s.replace(['\n', '\r'], " ")),
            Some(Overlay::ConfirmApply { input, .. }) => input.insert_str(line),
            Some(Overlay::Login(form)) => form.fields[form.field].insert_str(line),
            Some(Overlay::Export(d)) => {
                d.path.insert_str(line);
                d.edited = true;
            }
            Some(Overlay::HistorySearch(h)) => {
                h.input.insert_str(line);
                let Some(Overlay::HistorySearch(mut h)) = self.overlay.take() else {
                    unreachable!()
                };
                self.rank_history(&mut h);
                self.overlay = Some(Overlay::HistorySearch(h));
            }
            Some(Overlay::DataSearch { input, .. }) => {
                input.insert_str(line);
                let text = input.text().to_string();
                let search = move |app: &mut App| app.incremental_search(&text);
                if self.tab == Tab::Sql {
                    self.with_results(search);
                } else {
                    search(self);
                }
            }
            None if sql_editor => self.on_sql_paste(s),
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
        if self.screen == Screen::Main
            && self.sql_editor_focused()
            && !self.resolver.is_pending()
            && let Some(effects) = self.on_sql_editor_key(key)
        {
            return effects;
        }
        let contexts = self.contexts();
        match self
            .resolver
            .feed(&self.keymap, &contexts, KeyChord::from_event(&key))
        {
            Resolved::Action(a) => self.dispatch(a),
            Resolved::Pending => Vec::new(),
            Resolved::PendingWith(a) => self.dispatch(a),
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
        // Overlays opened on a SQL result work on that result.
        if self.overlay_in_results && !self.results_mode {
            if let Some(effects) = self.with_results(|app| app.on_overlay_key(key)) {
                return effects;
            }
            self.overlay_in_results = false;
        }
        match &self.overlay {
            Some(Overlay::Login(_)) => return self.on_login_key(key),
            Some(Overlay::SaveServer(_)) => return self.on_save_server_key(key),
            Some(Overlay::ConfirmDelete(_)) => return self.on_confirm_delete_key(key),
            Some(Overlay::CellEditor(_)) => return self.on_cell_editor_key(key),
            Some(Overlay::Changes(_)) => return self.on_changes_key(key),
            Some(Overlay::ConfirmApply { .. }) => return self.on_confirm_apply_key(key),
            Some(Overlay::ConfirmDiscard) => return self.on_confirm_discard_key(key),
            Some(Overlay::PendingChanges(_)) => return self.on_pending_changes_key(key),
            Some(Overlay::Reference(_)) => return self.on_reference_key(key),
            _ => {}
        }
        if self.overlay.as_ref().is_some_and(Overlay::is_data) {
            if self.tab == Tab::Sql {
                return self
                    .with_results(|app| app.on_data_overlay_key(key))
                    .unwrap_or_else(|| {
                        self.overlay = None;
                        Vec::new()
                    });
            }
            return self.on_data_overlay_key(key);
        }
        match &self.overlay {
            Some(Overlay::ConfirmWrites { .. }) => return self.on_confirm_writes_key(key),
            Some(Overlay::ConfirmTxnExit(_)) => return self.on_confirm_txn_exit_key(key),
            Some(Overlay::HistorySearch(_)) => return self.on_history_key(key),
            Some(Overlay::QueryList { .. }) => return self.on_query_list_key(key),
            Some(Overlay::JumpList { .. }) => return self.on_jump_list_key(key),
            Some(Overlay::Export(_)) => return self.on_export_key(key),
            _ => {}
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
            | Overlay::Reference(_)
            | Overlay::Login(_)
            | Overlay::SaveServer(_)
            | Overlay::ConfirmDelete(_)
            | Overlay::CellEditor(_)
            | Overlay::Changes(_)
            | Overlay::ConfirmApply { .. }
            | Overlay::ConfirmDiscard
            | Overlay::PendingChanges(_)
            | Overlay::ConfirmWrites { .. }
            | Overlay::ConfirmTxnExit(_)
            | Overlay::HistorySearch(_)
            | Overlay::QueryList { .. }
            | Overlay::JumpList { .. }
            | Overlay::Export(_) => Vec::new(),
            Overlay::Error { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.overlay = None;
                }
                Vec::new()
            }
            Overlay::Command(input) => match key.code {
                KeyCode::Down => {
                    self.palette_cursor = Some(self.palette_cursor.map_or(0, |c| c + 1));
                    Vec::new()
                }
                KeyCode::Up => {
                    self.palette_cursor = self.palette_cursor.and_then(|c| c.checked_sub(1));
                    Vec::new()
                }
                KeyCode::Tab => {
                    let text = input.text().to_string();
                    let chosen = self
                        .palette_matches(&text)
                        .into_iter()
                        .nth(self.palette_cursor.unwrap_or(0));
                    if let Some(palette::PaletteEntry {
                        target: palette::PaletteTarget::Command { name, arg },
                        ..
                    }) = chosen
                        && let Some(Overlay::Command(input)) = &mut self.overlay
                    {
                        *input = LineInput::with_text(if arg {
                            format!("{name} ")
                        } else {
                            name.to_string()
                        });
                        self.palette_cursor = None;
                    }
                    Vec::new()
                }
                _ => match input.handle_key(&key) {
                    InputOutcome::Submit => {
                        let line = input.text().trim().to_string();
                        self.overlay = None;
                        let cursor = self.palette_cursor.take();
                        self.run_palette(&line, cursor)
                    }
                    InputOutcome::Changed => {
                        self.palette_cursor = None;
                        Vec::new()
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
            },
            Overlay::Password { conn, input, .. } => match input.handle_key(&key) {
                InputOutcome::Submit => {
                    let password = input.text().to_string();
                    let name = conn.name.clone();
                    self.overlay = None;
                    self.connect_server(&name, Some(password), false)
                }
                InputOutcome::Cancel => {
                    self.overlay = None;
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
            // On the connections page, `q` / Esc go back to the tree.
            Quit | Cancel if self.screen == Screen::Picker => self.screen = Screen::Main,
            Quit => {
                if !self.guard(edit::After::Quit) {
                    self.should_quit = true;
                }
            }
            ConnectionsPage => {
                self.screen = Screen::Picker;
                return self.discover();
            }
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            CommandPalette => {
                self.palette_cursor = None;
                self.overlay = Some(Overlay::Command(LineInput::default()));
            }
            Interrupt => {
                if let Some(effects) = self.cancel_sql() {
                    return effects;
                }
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
            NextTab => self.switch_tab(self.tab.cycle(1)),
            PrevTab => self.switch_tab(self.tab.cycle(-1)),
            TabSchema => self.switch_tab(Tab::Schema),
            TabData => self.switch_tab(Tab::Data),
            TabSql => self.switch_tab(Tab::Sql),
            ReloadCatalog => return self.reload_catalog(),
            _ if self.focus == Focus::Tree => return self.dispatch_tree(action),
            _ if self.tab == Tab::Schema => return self.dispatch_schema(action),
            _ if self.tab == Tab::Data => return self.dispatch_data(action),
            _ if self.tab == Tab::Sql => return self.dispatch_sql(action),
            _ => {}
        }
        Vec::new()
    }

    /// The SQL tab needs no object: it takes the focus (editor).
    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if tab == Tab::Sql {
            self.focus = Focus::Center;
        }
    }

    fn dispatch_schema(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        match action {
            NextSection => self.schema.cycle_section(1),
            PrevSection => self.schema.cycle_section(-1),
            JumpBack => self.jump_back(),
            FollowLink => {
                let target = self.schema.table().and_then(|t| t.selected_link()).cloned();
                match target {
                    // A virtual relation can point to a database this server lacks.
                    Some(t) if !self.has_database(&t) => self.notify(
                        Level::Warning,
                        format!(
                            "Base « {} » absente de la connexion {}",
                            t.database, t.connection
                        ),
                    ),
                    Some(t) => {
                        if !self.guard(edit::After::Follow(t.clone())) {
                            if let Some(cur) = self.current.take() {
                                self.back_stack.push(cur);
                            }
                            self.go_to(t);
                        }
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
                if let Some(obj) = self.current.clone()
                    && let Some(s) = self.server_mut(&obj.connection)
                {
                    s.details.remove(&obj);
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
    /// `Ctrl+o`: the object visited before the current one.
    fn jump_back(&mut self) {
        match self.back_stack.last() {
            Some(_) => {
                if !self.guard(edit::After::Back) {
                    let prev = self.back_stack.pop().expect("checked above");
                    self.go_to(prev);
                }
            }
            None => self.notify(Level::Info, "Pas d'objet précédent"),
        }
    }

    /// The object's database is listed on its connection.
    fn has_database(&self, obj: &ObjectRef) -> bool {
        self.server(&obj.connection)
            .is_some_and(|s| s.database(&obj.database).is_some())
    }

    fn go_to(&mut self, obj: ObjectRef) {
        if !self.tree.filter.is_empty() {
            self.tree.filter.clear();
        }
        self.tree.reveal(&self.servers, &obj);
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
            Action::Connect => {
                match self.picker_item(cur) {
                    Some(discovery::PickerItem::Configured(conn)) => {
                        let name = conn.name.clone();
                        self.screen = Screen::Main;
                        self.focus = Focus::Tree;
                        self.open_connection(&name);
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

    fn dispatch_tree(&mut self, action: Action) -> Vec<Effect> {
        let servers = &self.servers;
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
            Action::Collapse => tree.collapse(servers),
            Action::CollapseAll => tree.collapse_all(servers),
            Action::Filter => tree.filter_editing = true,
            Action::ClearFilter => {
                if !tree.filter.is_empty() {
                    tree.filter.clear();
                    tree.rebuild(servers);
                }
            }
            Action::Expand | Action::Open => {
                let Some(row) = tree.selected().cloned() else {
                    return Vec::new();
                };
                match row.kind {
                    RowKind::Object(_) if action == Action::Open => {
                        if let NodeKey::Object(obj) = row.key {
                            // Clearing the filter keeps the object in context.
                            if !tree.filter.is_empty() {
                                tree.filter.clear();
                                tree.reveal(servers, &obj);
                            }
                            self.open_object(obj);
                        }
                    }
                    RowKind::Object(_) => {}
                    RowKind::Connection => {
                        // A failed / lost connection is retried.
                        let name = row.label.clone();
                        let retry = self.server(&name).is_some_and(|s| {
                            matches!(
                                s.state,
                                server::ConnState::Failed(_) | server::ConnState::Lost(_)
                            )
                        });
                        if retry || !row.expanded {
                            self.open_connection(&name);
                        } else if action == Action::Open {
                            self.tree.toggle(&self.servers);
                        } else {
                            self.tree.expand(&self.servers);
                        }
                    }
                    _ if action == Action::Open => tree.toggle(servers),
                    _ => tree.expand(servers),
                }
            }
            Action::Disconnect => {
                if let Some(name) = tree.selected_connection().map(str::to_string) {
                    return self.disconnect(&name);
                }
            }
            Action::DeleteConnection => match tree.selected().map(|r| (r.kind, r.label.clone())) {
                Some((RowKind::Connection, name)) => self.ask_delete(&name),
                _ => self.notify(
                    Level::Info,
                    "Placez le curseur sur une connexion pour la supprimer",
                ),
            },
            _ => {}
        }
        Vec::new()
    }

    fn open_object(&mut self, obj: ObjectRef) {
        if self.guard(edit::After::Open(obj.clone())) {
            return;
        }
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
        self.tree.filter_changed(&self.servers);
    }

    // ------------------------------------------------------------- commands

    /// `:` command line. The full fuzzy palette comes in milestone 6.
    fn run_command(&mut self, line: &str) -> Vec<Effect> {
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else {
            return Vec::new();
        };
        let arg = parts.next();
        let current = self.current_server().map(|s| s.name().to_string());
        match cmd {
            "q!" | "quit!" | "qa!" => self.should_quit = true,
            "q" | "quit" | "qa" => {
                if !self.guard(edit::After::Quit) {
                    self.should_quit = true;
                }
            }
            "h" | "help" => self.overlay = Some(Overlay::Help { scroll: 0 }),
            "reload" | "refresh" => return self.reload_catalog(),
            "connections" | "conn" => {
                self.screen = Screen::Picker;
                return self.discover();
            }
            "reconnect" => match current {
                Some(name) => return self.connect_server(&name, None, true),
                None => self.notify(Level::Warning, "Aucune connexion sélectionnée"),
            },
            "disconnect" => match current {
                Some(name) => return self.disconnect(&name),
                None => self.notify(Level::Warning, "Aucune connexion sélectionnée"),
            },
            "connect" | "c" => {
                let Some(name) = arg else {
                    self.notify(Level::Warning, "Usage : :connect <nom>");
                    return Vec::new();
                };
                if self.server(name).is_some() {
                    self.screen = Screen::Main;
                    self.open_connection(name);
                } else {
                    self.notify(Level::Error, format!("Connexion inconnue : {name}"));
                }
            }
            "save" | "w" => return self.save_query(arg),
            "open" | "e" => return self.open_query(arg),
            "new" | "enew" => self.new_script(),
            "close" => self.close_script(),
            "clear" => {
                self.sql.editor.set_text("");
                self.switch_tab(Tab::Sql);
                self.notify(Level::Info, "Script vidé (u / Ctrl+z pour annuler)");
            }
            "history" => {
                self.tab = Tab::Sql;
                self.focus = Focus::Center;
                return self.dispatch_sql(Action::HistorySearch);
            }
            "schema" => self.tab = Tab::Schema,
            "data" => self.tab = Tab::Data,
            "sql" => self.switch_tab(Tab::Sql),
            other => self.notify(
                Level::Error,
                format!(
                    "Commande inconnue : {other} (Up/Down et Entrée pour choisir une suggestion)"
                ),
            ),
        }
        Vec::new()
    }

    pub fn command_names() -> &'static [(&'static str, &'static str)] {
        &[
            ("q, quit", "Quitter"),
            ("help", "Aide"),
            ("reload", "Recharger les catalogues"),
            ("connections", "Page des connexions (serveurs détectés)"),
            ("connect <nom>", "Ouvrir une connexion dans l'arbre"),
            ("reconnect", "Rouvrir la connexion courante"),
            ("disconnect", "Fermer la connexion courante"),
            ("save [nom]", "Enregistrer la requête de l'onglet SQL"),
            ("open [nom]", "Ouvrir une requête enregistrée"),
            ("history", "Rechercher dans l'historique SQL"),
            ("schema | data | sql", "Changer d'onglet"),
        ]
    }

    // ------------------------------------------------------------------- db

    fn on_db(&mut self, ev: DbEvent) -> Vec<Effect> {
        match ev {
            ev @ (DbEvent::RowsLoaded { .. }
            | DbEvent::RowsFailed { .. }
            | DbEvent::RowsCounted { .. }
            | DbEvent::CountFailed { .. }) => {
                self.on_data_event(ev);
                Vec::new()
            }
            ev @ DbEvent::ReferenceLoaded { .. } => {
                self.on_reference_event(ev);
                Vec::new()
            }
            ev @ (DbEvent::ChangesApplied { .. } | DbEvent::ChangesFailed { .. }) => {
                self.on_apply_event(ev)
            }
            DbEvent::CompletionLoaded {
                connection,
                database,
                columns,
                search_path,
            } => {
                self.on_completion_loaded(connection, database, columns, search_path);
                Vec::new()
            }
            ev @ (DbEvent::ExportProgress { .. }
            | DbEvent::ExportDone { .. }
            | DbEvent::ExportFailed { .. }) => {
                self.on_export_event(ev);
                Vec::new()
            }
            ev @ (DbEvent::StatementDone { .. }
            | DbEvent::StatementFailed { .. }
            | DbEvent::Explained { .. }) => self.on_sql_event(ev),
            DbEvent::Discovered(servers) => {
                self.on_discovered(servers);
                Vec::new()
            }
            ev => self.on_server_event(ev),
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
