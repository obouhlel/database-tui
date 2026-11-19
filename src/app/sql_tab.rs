//! SQL tab: editor, execution (statement under the cursor, selection or
//! whole script, one statement at a time), results in sub-tabs, manual
//! transactions, history and saved queries. Part of the reducer: no I/O.
//!
//! Statements run on the session connection of the target database: the
//! database of the current object or tree selection, locked while a
//! transaction is open. Results reuse [`DataView`] (read-only, fully
//! loaded), swapped into `App::data` while the Data machinery handles them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

use super::data_view::DataView;
use super::edit::After;
use super::editor::{EditOutcome, Editor, VimMode};
use super::input::{InputOutcome, LineInput};
use super::{App, Busy, Effect, Focus, Level, Overlay, Tab};
use crate::config::EditorMode;
use crate::db::exec::{ExecError, StatementResult};
use crate::db::worker::{DbCommand, DbEvent};
use crate::keymap::{Action, Context, KeyChord};
use crate::sql::script::{self, TxnEffect};

/// Rows kept per result.
pub const MAX_ROWS: usize = 10_000;
/// History entries kept in memory (and in the file).
pub const MAX_HISTORY: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SqlFocus {
    #[default]
    Editor,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxnState {
    #[default]
    Idle,
    Open,
    /// An error occurred in the transaction: only ROLLBACK is accepted.
    Aborted,
}

#[derive(Debug, Clone)]
pub struct ResultTab {
    /// Short title (`SELECT 3`, `UPDATE 12`, `ERREUR`).
    pub label: String,
    pub statement: String,
    /// Rows, for statements that return some.
    pub view: Option<DataView>,
    pub message: String,
    pub error: bool,
    pub elapsed: Duration,
    /// `EXPLAIN` result.
    pub plan: Option<super::plan::Plan>,
    /// First line shown (plans).
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct Running {
    pub id: u64,
    /// Statement in flight and its byte offset in the editor text.
    pub current: (String, usize),
    pub queue: VecDeque<(String, usize)>,
    pub total: usize,
    pub started: Instant,
    /// `Some(analyze)` for an EXPLAIN.
    pub explain: Option<bool>,
}

/// One line of `~/.local/share/pgtui/history` (JSON lines).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix time, seconds.
    pub at: i64,
    pub connection: String,
    pub database: String,
    pub sql: String,
    pub ok: bool,
    pub ms: u64,
}

#[derive(Debug, Clone)]
pub struct HistorySearch {
    pub input: LineInput,
    /// Indices into the history, best match first.
    pub matches: Vec<usize>,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SqlState {
    pub editor: Editor,
    pub focus: SqlFocus,
    pub results: Vec<ResultTab>,
    pub active: usize,
    /// (connection, database) of the last run; fixed while a transaction is open.
    pub target: Option<(String, String)>,
    pub txn: TxnState,
    pub running: Option<Running>,
    pub history: Vec<HistoryEntry>,
    /// Name of the saved query being edited.
    pub file: Option<String>,
    /// Completion popup.
    pub popup: Option<super::completion::CompletionPopup>,
    /// Columns for the completion, per database.
    pub columns: super::completion::ColumnsCache,
}

impl SqlState {
    pub fn new(mode: EditorMode, tab_width: usize) -> Self {
        Self {
            editor: Editor::new(mode, tab_width),
            ..Default::default()
        }
    }

    pub fn active_view(&self) -> Option<&DataView> {
        self.results.get(self.active).and_then(|r| r.view.as_ref())
    }
}

/// Valid name for a saved query (a file name).
pub fn valid_query_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
}

impl App {
    /// Runs `f` with the active result swapped into `self.data`, so that the
    /// Data tab code (navigation, search, copy, detail) works on it.
    pub(super) fn with_results<R>(&mut self, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let i = self.sql.active;
        let mut view = self.sql.results.get_mut(i)?.view.take()?;
        std::mem::swap(&mut self.data, &mut view);
        self.results_mode = true;
        let r = f(self);
        self.results_mode = false;
        // An overlay opened (or kept) here belongs to the result.
        self.overlay_in_results = self.overlay.is_some();
        std::mem::swap(&mut self.data, &mut view);
        self.sql.results[i].view = Some(view);
        Some(r)
    }

    /// Swaps the active result in or out of `self.data` (for rendering).
    pub fn swap_results(&mut self) -> bool {
        let i = self.sql.active;
        let Some(tab) = self.sql.results.get_mut(i) else {
            return false;
        };
        match tab.view.as_mut() {
            Some(view) => {
                std::mem::swap(&mut self.data, view);
                self.results_mode = !self.results_mode;
                true
            }
            None => false,
        }
    }

    pub fn sql_editor_focused(&self) -> bool {
        self.tab == Tab::Sql && self.focus == Focus::Center && self.sql.focus == SqlFocus::Editor
    }

    /// Keys of the SQL editor. Returns `None` to let the keymap handle it.
    pub(super) fn on_sql_editor_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.sql.editor.inserting()
            && let Some(effects) = self.on_completion_key(&key)
        {
            return Some(effects);
        }
        let popup = self.sql.popup.is_some();
        let ed = &mut self.sql.editor;
        if ed.inserting() {
            // Esc leaves the classic editor for the results.
            if ed.mode == EditorMode::Classic && key.code == KeyCode::Esc && ed.anchor.is_none() {
                if !self.sql.results.is_empty() {
                    self.sql.focus = SqlFocus::Results;
                }
                return Some(Vec::new());
            }
            if key.code == KeyCode::Char('c') && ctrl {
                if self.sql.running.is_some() {
                    return None; // Interrupt
                }
                return Some(match self.sql.editor.copy_selection() {
                    Some(text) => {
                        self.notify(Level::Success, "Sélection copiée");
                        vec![Effect::Copy(text)]
                    }
                    None => Vec::new(),
                });
            }
            let special = ctrl || alt || matches!(key.code, KeyCode::F(_));
            if special {
                let chord = KeyChord::from_event(&key);
                if let Some(action) = self
                    .keymap
                    .single(&[Context::SqlEditor, Context::Global], &chord)
                {
                    return Some(self.dispatch(action));
                }
            }
            let mut effects = self.editor_outcome(key);
            if popup {
                effects.extend(self.follow_completion(&key));
            }
            return Some(effects);
        }
        // Vim normal / visual: the editor first, then the keymap.
        match self.sql.editor.handle_key(&key) {
            EditOutcome::Ignored => None,
            outcome => Some(self.after_edit(outcome)),
        }
    }

    fn editor_outcome(&mut self, key: KeyEvent) -> Vec<Effect> {
        let outcome = self.sql.editor.handle_key(&key);
        self.after_edit(outcome)
    }

    fn after_edit(&mut self, outcome: EditOutcome) -> Vec<Effect> {
        match outcome {
            EditOutcome::Copied(text) => vec![Effect::Copy(text)],
            _ => Vec::new(),
        }
    }

    pub(super) fn on_sql_paste(&mut self, s: &str) {
        let ed = &mut self.sql.editor;
        if ed.mode == EditorMode::Vim && ed.vim != VimMode::Insert {
            ed.vim = VimMode::Insert;
        }
        ed.insert_str(s);
    }

    pub(super) fn dispatch_sql(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        match action {
            ExecuteStatement => return self.execute(false),
            Complete => return self.complete(true),
            Explain | ExplainAnalyze => return self.explain(action == ExplainAnalyze),
            _ if self.sql.focus == SqlFocus::Results
                && self
                    .sql
                    .results
                    .get(self.sql.active)
                    .is_some_and(|r| r.plan.is_some()) =>
            {
                return self.dispatch_plan(action);
            }
            ExecuteScript => return self.execute(true),
            HistorySearch => self.open_history_search(),
            ClearEditor => self.clear_editor(),
            FocusResults => {
                if self.sql.results.is_empty() {
                    self.notify(Level::Info, "Aucun résultat");
                } else {
                    self.sql.focus = SqlFocus::Results;
                }
            }
            FocusEditor => self.sql.focus = SqlFocus::Editor,
            NextResult | PrevResult => {
                let n = self.sql.results.len();
                if n > 1 {
                    self.sql.active = if action == NextResult {
                        (self.sql.active + 1) % n
                    } else {
                        (self.sql.active + n - 1) % n
                    };
                }
            }
            _ if self.sql.focus == SqlFocus::Results => {
                return self
                    .with_results(|app| app.dispatch_data(action))
                    .unwrap_or_default();
            }
            _ => {}
        }
        Vec::new()
    }

    /// `Alt+c` / `:clear`: empties the editor, as one undo step.
    pub(super) fn clear_editor(&mut self) {
        if self.sql.editor.is_empty() {
            self.notify(Level::Info, "L'éditeur est déjà vide");
            return;
        }
        self.sql.editor.set_text("");
        self.sql.focus = SqlFocus::Editor;
        self.notify(
            Level::Info,
            "SQL effacé — Ctrl+z (ou u en vim) pour le récupérer",
        );
    }

    /// Database the next statement runs on.
    pub fn sql_target(&self) -> Option<(String, String)> {
        if self.sql.txn != TxnState::Idle {
            return self.sql.target.clone();
        }
        let s = self.current_server().filter(|s| s.is_connected())?;
        let db = self.current_database()?.to_string();
        Some((s.name().to_string(), db))
    }

    /// `script`: the whole editor; else the selection or the statement
    /// under the cursor.
    fn execute(&mut self, script: bool) -> Vec<Effect> {
        if self.sql.running.is_some() {
            self.notify(Level::Info, "Requête en cours (Ctrl+c pour l'annuler)");
            return Vec::new();
        }
        if self.results_pending() {
            self.notify(
                Level::Warning,
                "Changements en attente sur un résultat : Ctrl+s pour les appliquer, Ctrl+z pour les abandonner",
            );
            return Vec::new();
        }
        let text = self.sql.editor.text();
        let ranges: Vec<std::ops::Range<usize>> = if script {
            script::split(&text)
        } else if let Some(sel) = self.sql.editor.selection() {
            script::split(&text[sel.clone()])
                .into_iter()
                .map(|r| r.start + sel.start..r.end + sel.start)
                .collect()
        } else {
            let at = self.sql.editor.offset(self.sql.editor.cursor);
            script::statement_at(&text, at).into_iter().collect()
        };
        if ranges.is_empty() {
            self.notify(Level::Info, "Rien à exécuter");
            return Vec::new();
        }
        let Some(target) = self.sql_target() else {
            self.notify(
                Level::Warning,
                "Aucune connexion ouverte : connectez-vous dans l'arbre (Entrée sur une connexion)",
            );
            return Vec::new();
        };
        let statements: Vec<(String, usize)> = ranges
            .into_iter()
            .map(|r| (text[r.clone()].to_string(), r.start))
            .collect();
        let risky: Vec<String> = statements
            .iter()
            .filter_map(|(s, _)| {
                script::unrestricted_write(s).map(|verb| {
                    let first = s.lines().next().unwrap_or("").trim();
                    format!("{verb} sans WHERE : {first}")
                })
            })
            .collect();
        if !risky.is_empty() {
            self.overlay = Some(Overlay::ConfirmWrites {
                statements,
                target,
                risky,
            });
            return Vec::new();
        }
        self.start_run(statements, target)
    }

    pub(super) fn on_confirm_writes_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::ConfirmWrites {
            statements,
            target,
            risky,
        }) = self.overlay.take()
        else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('o' | 'O' | 'y' | 'Y') => self.start_run(statements, target),
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => {
                self.notify(Level::Info, "Exécution annulée");
                Vec::new()
            }
            _ => {
                self.overlay = Some(Overlay::ConfirmWrites {
                    statements,
                    target,
                    risky,
                });
                Vec::new()
            }
        }
    }

    /// F6 / F7: EXPLAIN [ANALYZE] of the statement under the cursor.
    fn explain(&mut self, analyze: bool) -> Vec<Effect> {
        if self.sql.running.is_some() {
            self.notify(Level::Info, "Requête en cours (Ctrl+c pour l'annuler)");
            return Vec::new();
        }
        let text = self.sql.editor.text();
        let range = match self.sql.editor.selection() {
            Some(sel) => script::split(&text[sel.clone()])
                .into_iter()
                .next()
                .map(|r| r.start + sel.start..r.end + sel.start),
            None => script::statement_at(&text, self.sql.editor.offset(self.sql.editor.cursor)),
        };
        let Some(range) = range else {
            self.notify(Level::Info, "Rien à expliquer");
            return Vec::new();
        };
        let Some(target) = self.sql_target() else {
            self.notify(
                Level::Warning,
                "Aucune connexion ouverte : connectez-vous dans l'arbre (Entrée sur une connexion)",
            );
            return Vec::new();
        };
        let stmt = text[range.clone()].to_string();
        // The first command is replaced by an EXPLAIN.
        let _ = self.start_run(vec![(stmt, range.start)], target);
        if let Some(run) = &mut self.sql.running {
            run.explain = Some(analyze);
        }
        self.send_current()
    }

    fn dispatch_plan(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        let i = self.sql.active;
        let r = &mut self.sql.results[i];
        let Some(plan) = &r.plan else {
            return Vec::new();
        };
        let n = plan.lines.len() * 2 + plan.summary.len();
        match action {
            Down => r.scroll = (r.scroll + 1).min(n.saturating_sub(1)),
            Up => r.scroll = r.scroll.saturating_sub(1),
            HalfPageDown | PageDown => r.scroll = (r.scroll + 10).min(n.saturating_sub(1)),
            HalfPageUp | PageUp => r.scroll = r.scroll.saturating_sub(10),
            Top => r.scroll = 0,
            Bottom => r.scroll = n.saturating_sub(1),
            Copy | CopyRow | CopyAs => {
                let text = plan.to_text();
                self.notify(Level::Success, "Plan copié");
                return vec![Effect::Copy(text)];
            }
            _ => {}
        }
        Vec::new()
    }

    /// Runs one statement again (a result after its changes were applied).
    pub(super) fn rerun(
        &mut self,
        stmt: String,
        offset: usize,
        target: (String, String),
    ) -> Vec<Effect> {
        self.start_run(vec![(stmt, offset)], target)
    }

    /// Pending edits on a result (lost if the results are replaced).
    pub fn results_pending(&self) -> bool {
        self.sql
            .results
            .iter()
            .any(|r| r.view.as_ref().is_some_and(|v| !v.staging.is_empty()))
    }

    fn start_run(
        &mut self,
        statements: Vec<(String, usize)>,
        target: (String, String),
    ) -> Vec<Effect> {
        let total = statements.len();
        let mut queue: VecDeque<(String, usize)> = statements.into();
        let Some(first) = queue.pop_front() else {
            return Vec::new();
        };
        self.sql.results.clear();
        self.sql.active = 0;
        self.sql.target = Some(target);
        self.request_seq += 1;
        self.sql.running = Some(Running {
            id: self.request_seq,
            current: first,
            queue,
            total,
            started: Instant::now(),
            explain: None,
        });
        self.busy = Some(Busy {
            label: "requête".into(),
            since: Instant::now(),
        });
        self.status = None;
        self.send_current()
    }

    fn send_current(&mut self) -> Vec<Effect> {
        let (Some(run), Some((connection, database))) = (&self.sql.running, &self.sql.target)
        else {
            return Vec::new();
        };
        let cmd = match run.explain {
            Some(analyze) => DbCommand::Explain {
                id: run.id,
                connection: connection.clone(),
                database: database.clone(),
                sql: run.current.0.clone(),
                analyze,
                in_transaction: self.sql.txn != TxnState::Idle,
            },
            None => DbCommand::Execute {
                id: run.id,
                connection: connection.clone(),
                database: database.clone(),
                sql: run.current.0.clone(),
                max_rows: MAX_ROWS,
            },
        };
        vec![cmd.into()]
    }

    fn history_entry(&self, sql: &str, ok: bool, elapsed: Duration) -> Option<HistoryEntry> {
        let (connection, database) = self.sql.target.clone()?;
        Some(HistoryEntry {
            at: chrono::Utc::now().timestamp(),
            connection,
            database,
            sql: sql.to_string(),
            ok,
            ms: elapsed.as_millis() as u64,
        })
    }

    fn record(&mut self, entry: Option<HistoryEntry>) -> Option<Effect> {
        let entry = entry?;
        let dup = self
            .sql
            .history
            .last()
            .is_some_and(|l| l.sql == entry.sql && l.connection == entry.connection);
        if dup {
            return None;
        }
        self.sql.history.push(entry.clone());
        if self.sql.history.len() > MAX_HISTORY {
            let extra = self.sql.history.len() - MAX_HISTORY;
            self.sql.history.drain(..extra);
        }
        Some(Effect::AppendHistory(Box::new(entry)))
    }

    pub(super) fn on_sql_event(&mut self, ev: DbEvent) -> Vec<Effect> {
        let id = match &ev {
            DbEvent::StatementDone { id, .. }
            | DbEvent::StatementFailed { id, .. }
            | DbEvent::Explained { id, .. } => *id,
            _ => return Vec::new(),
        };
        if self.sql.running.as_ref().is_none_or(|r| r.id != id) {
            return Vec::new();
        }
        let (stmt, offset) = self.sql.running.as_ref().expect("checked").current.clone();
        let mut effects = Vec::new();
        match ev {
            DbEvent::StatementDone {
                result, elapsed, ..
            } => {
                match script::txn_effect(&stmt) {
                    TxnEffect::Begin => self.sql.txn = TxnState::Open,
                    TxnEffect::End => self.sql.txn = TxnState::Idle,
                    TxnEffect::None => {}
                }
                let entry = self.history_entry(&stmt, true, elapsed);
                effects.extend(self.record(entry));
                let editable = match (&result.source, &self.sql.target) {
                    (Some(src), Some((connection, database))) => {
                        let read_only = self.server(connection).is_some_and(|s| s.conn.read_only);
                        (!read_only).then(|| {
                            (
                                crate::db::catalog::ObjectRef {
                                    connection: connection.clone(),
                                    database: database.clone(),
                                    schema: src.schema.clone(),
                                    name: src.table.clone(),
                                    kind: crate::db::catalog::ObjectKind::Table,
                                    signature: None,
                                },
                                src.key.clone(),
                            )
                        })
                    }
                    _ => None,
                };
                self.sql
                    .results
                    .push(result_tab(&stmt, *result, elapsed, editable));
                let next = self.sql.running.as_mut().and_then(|r| r.queue.pop_front());
                match next {
                    Some(next) => {
                        self.request_seq += 1;
                        let run = self.sql.running.as_mut().expect("running");
                        run.id = self.request_seq;
                        run.current = next;
                        effects.extend(self.send_current());
                    }
                    None => self.finish_run(None),
                }
            }
            DbEvent::Explained { json, elapsed, .. } => {
                let analyze = self
                    .sql
                    .running
                    .as_ref()
                    .and_then(|r| r.explain)
                    .unwrap_or(false);
                let label = if analyze {
                    "EXPLAIN ANALYZE"
                } else {
                    "EXPLAIN"
                };
                let tab = match super::plan::parse(&json) {
                    Ok(plan) => ResultTab {
                        label: label.into(),
                        statement: stmt.clone(),
                        view: None,
                        message: if plan.summary.is_empty() {
                            format!("{} nœud(s)", plan.lines.len())
                        } else {
                            plan.summary.join(" · ")
                        },
                        error: false,
                        elapsed,
                        plan: Some(plan),
                        scroll: 0,
                    },
                    Err(e) => error_tab(
                        &stmt,
                        &ExecError {
                            message: e,
                            position: None,
                        },
                        elapsed,
                    ),
                };
                self.sql.results.push(tab);
                self.finish_run(None);
            }
            DbEvent::StatementFailed { error, elapsed, .. } => {
                self.sql.txn = match (self.sql.txn, script::txn_effect(&stmt)) {
                    // A failed COMMIT ends the transaction (rolled back).
                    (_, TxnEffect::End) => TxnState::Idle,
                    (TxnState::Open | TxnState::Aborted, _) => TxnState::Aborted,
                    (TxnState::Idle, _) => TxnState::Idle,
                };
                let entry = self.history_entry(&stmt, false, elapsed);
                effects.extend(self.record(entry));
                if let Some(p) = error.position {
                    let byte = stmt
                        .char_indices()
                        .nth(p.saturating_sub(1) as usize)
                        .map_or(stmt.len(), |(i, _)| i);
                    self.sql.editor.jump_to(offset + byte);
                }
                self.sql.results.push(error_tab(&stmt, &error, elapsed));
                self.finish_run(Some(error));
            }
            _ => {}
        }
        effects
    }

    fn finish_run(&mut self, error: Option<ExecError>) {
        let Some(run) = self.sql.running.take() else {
            return;
        };
        self.busy = None;
        let elapsed = run.started.elapsed();
        self.last_op = Some(super::LastOp {
            label: "SQL".into(),
            elapsed,
        });
        // The error if any (the run stops on it), else the first result
        // with rows: `]` then goes through the others in order.
        let n = self.sql.results.len();
        self.sql.active = self
            .sql
            .results
            .iter()
            .position(|r| r.error)
            .or_else(|| {
                self.sql
                    .results
                    .iter()
                    .position(|r| r.view.is_some() || r.plan.is_some())
            })
            .unwrap_or(n.saturating_sub(1));
        let done = self.sql.results.iter().filter(|r| !r.error).count();
        match error {
            None => {
                let msg = if run.total == 1 {
                    self.sql
                        .results
                        .last()
                        .map_or_else(String::new, |r| r.message.clone())
                } else {
                    format!("{done} instruction(s) exécutée(s)")
                };
                self.notify(
                    Level::Success,
                    format!("{msg} — {}", crate::ui::fmt_elapsed(elapsed)),
                );
            }
            Some(e) => {
                let skipped = run.queue.len();
                let mut msg = format!("Erreur : {}", e.message.lines().next().unwrap_or(""));
                if skipped > 0 {
                    msg.push_str(&format!(" — {skipped} instruction(s) non exécutée(s)"));
                }
                self.notify(Level::Error, msg);
            }
        }
    }

    /// Ctrl+c while a statement runs.
    pub(super) fn cancel_sql(&mut self) -> Option<Vec<Effect>> {
        self.sql.running.as_ref()?;
        let (connection, database) = self.sql.target.clone()?;
        self.notify(Level::Warning, "Annulation de la requête demandée");
        Some(vec![
            DbCommand::Cancel {
                connection,
                database,
            }
            .into(),
        ])
    }

    /// The connection of the SQL target was closed or lost.
    pub(super) fn sql_connection_closed(&mut self, connection: &str) {
        if self
            .sql
            .target
            .as_ref()
            .is_some_and(|(c, _)| c == connection)
        {
            if self.sql.txn != TxnState::Idle {
                self.notify(
                    Level::Warning,
                    format!("Transaction sur {connection} perdue (annulée par le serveur)"),
                );
            }
            self.sql.txn = TxnState::Idle;
            if self.sql.running.take().is_some() {
                self.busy = None;
            }
        }
    }

    /// Quitting or disconnecting with an open transaction: `true` if the
    /// action must wait for a confirmation.
    pub(super) fn guard_transaction(&mut self, after: &After) -> bool {
        if self.sql.txn == TxnState::Idle {
            return false;
        }
        let concerned = match after {
            After::Quit => true,
            After::Disconnect(n) => self.sql.target.as_ref().is_some_and(|(c, _)| c == n),
            _ => false,
        };
        if concerned {
            self.overlay = Some(Overlay::ConfirmTxnExit(after.clone()));
        }
        concerned
    }

    pub(super) fn on_confirm_txn_exit_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::ConfirmTxnExit(after)) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('o' | 'O' | 'y' | 'Y') => {
                // Closing the session rolls the transaction back.
                self.sql.txn = TxnState::Idle;
                self.perform_after(after)
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Vec::new(),
            _ => {
                self.overlay = Some(Overlay::ConfirmTxnExit(after));
                Vec::new()
            }
        }
    }

    // -------------------------------------------------------------- history

    fn open_history_search(&mut self) {
        let mut s = HistorySearch {
            input: LineInput::default(),
            matches: Vec::new(),
            cursor: 0,
        };
        self.rank_history(&mut s);
        self.overlay = Some(Overlay::HistorySearch(Box::new(s)));
    }

    fn rank_history(&self, s: &mut HistorySearch) {
        let h = &self.sql.history;
        let q = s.input.text().trim();
        s.cursor = 0;
        if q.is_empty() {
            s.matches = (0..h.len()).rev().collect();
            return;
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = h
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let hay = Utf32Str::new(&e.sql, &mut buf);
                pattern.score(hay, &mut matcher).map(|sc| (sc, i))
            })
            .collect();
        // Best score first, then the most recent.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        s.matches = scored.into_iter().map(|(_, i)| i).collect();
    }

    pub(super) fn on_history_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::HistorySearch(mut s)) = self.overlay.take() else {
            return Vec::new();
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Up => s.cursor = s.cursor.saturating_sub(1),
            KeyCode::Down => s.cursor = (s.cursor + 1).min(s.matches.len().saturating_sub(1)),
            KeyCode::Char('p') if ctrl => s.cursor = s.cursor.saturating_sub(1),
            KeyCode::Char('n' | 'r') if ctrl => {
                s.cursor = (s.cursor + 1).min(s.matches.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(e) = s
                    .matches
                    .get(s.cursor)
                    .and_then(|i| self.sql.history.get(*i))
                {
                    let sql = e.sql.clone();
                    let ed = &mut self.sql.editor;
                    if ed.is_empty() {
                        ed.set_text(&sql);
                    } else {
                        let at_line_start = ed.cursor.col == 0;
                        let text = if at_line_start {
                            format!("{sql};\n")
                        } else {
                            format!("\n{sql};")
                        };
                        ed.insert_str(&text);
                    }
                    self.sql.focus = SqlFocus::Editor;
                }
                return Vec::new();
            }
            _ => {
                if s.input.handle_key(&key) == InputOutcome::Changed {
                    self.rank_history(&mut s);
                }
            }
        }
        self.overlay = Some(Overlay::HistorySearch(s));
        Vec::new()
    }

    // -------------------------------------------------------- saved queries

    pub(super) fn save_query(&mut self, name: Option<&str>) -> Vec<Effect> {
        let Some(name) = name.map(str::to_string).or_else(|| self.sql.file.clone()) else {
            self.notify(Level::Warning, "Usage : :save <nom>");
            return Vec::new();
        };
        if !valid_query_name(&name) {
            self.notify(
                Level::Error,
                "Nom invalide : lettres, chiffres, espace, _ - . uniquement",
            );
            return Vec::new();
        }
        self.sql.file = Some(name.clone());
        vec![Effect::SaveQuery {
            name,
            text: self.sql.editor.text(),
        }]
    }

    pub(super) fn open_query(&mut self, name: Option<&str>) -> Vec<Effect> {
        match name {
            Some(n) if valid_query_name(n) => vec![Effect::LoadQuery(n.to_string())],
            Some(_) => {
                self.notify(Level::Error, "Nom de requête invalide");
                Vec::new()
            }
            None => vec![Effect::ListQueries],
        }
    }

    pub(super) fn on_queries_listed(&mut self, names: Vec<String>) {
        if names.is_empty() {
            self.notify(Level::Info, "Aucune requête enregistrée (:save <nom>)");
        } else {
            self.overlay = Some(Overlay::QueryList { names, cursor: 0 });
        }
    }

    pub(super) fn on_query_list_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::QueryList { names, mut cursor }) = self.overlay.take() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(names.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Enter => return vec![Effect::LoadQuery(names[cursor].clone())],
            _ => {}
        }
        self.overlay = Some(Overlay::QueryList { names, cursor });
        Vec::new()
    }

    pub(super) fn on_query_loaded(&mut self, name: String, text: String) {
        self.sql.editor.set_text(&text);
        self.sql.file = Some(name.clone());
        self.tab = Tab::Sql;
        self.focus = Focus::Center;
        self.sql.focus = SqlFocus::Editor;
        self.notify(Level::Success, format!("Requête « {name} » ouverte"));
    }
}

fn result_tab(
    stmt: &str,
    r: StatementResult,
    elapsed: Duration,
    editable: Option<(crate::db::catalog::ObjectRef, Vec<String>)>,
) -> ResultTab {
    let verb = script::verb(stmt);
    let n = r.row_count;
    let (label, message) = if r.returns_rows {
        let mut msg = format!("{n} ligne(s)");
        if r.truncated {
            msg.push_str(&format!(" — {} premières affichées", r.rows.len()));
        }
        // A result named by its table reads better in a row of tabs.
        let label = match script::main_table(stmt) {
            Some(table) => format!("{table} {n}"),
            None => format!("{verb} {n}"),
        };
        (label, msg)
    } else if matches!(
        verb.as_str(),
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "COPY"
    ) {
        (
            format!("{verb} {n}"),
            format!("{verb} : {n} ligne(s) affectée(s)"),
        )
    } else {
        (verb.clone(), format!("{verb} : OK"))
    };
    ResultTab {
        label,
        statement: stmt.to_string(),
        view: r
            .returns_rows
            .then(|| DataView::from_result(&r.columns, r.rows, editable)),
        message,
        error: false,
        elapsed,
        plan: None,
        scroll: 0,
    }
}

fn error_tab(stmt: &str, e: &ExecError, elapsed: Duration) -> ResultTab {
    ResultTab {
        label: "ERREUR".into(),
        statement: stmt.to_string(),
        view: None,
        message: e.message.clone(),
        error: true,
        elapsed,
        plan: None,
        scroll: 0,
    }
}
