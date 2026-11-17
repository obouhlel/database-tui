//! Data tab: loading pages, actions and overlays (search, filters, detail,
//! copy format). Part of the reducer: no I/O, only [`Effect`]s.

use crossterm::event::{KeyCode, KeyEvent};

use super::data_view::{COUNT_BOUND, DataView, Pending, Total};
use super::export::{self, CopyFormat};
use super::filter_editor::{FilterEditor, Outcome};
use super::input::{InputOutcome, LineInput};
use super::{App, DetailsState, Effect, Level, Overlay, Tab};
use crate::db::worker::{DbCommand, DbEvent};
use crate::keymap::Action;

impl App {
    fn next_request_id(&mut self) -> u64 {
        self.request_seq += 1;
        self.request_seq
    }

    /// Creates the view when its object's details are known, then loads
    /// the visible page and counts rows when needed.
    pub(super) fn sync_data(&mut self) -> Vec<Effect> {
        if self.tab != Tab::Data {
            return Vec::new();
        }
        let Some(obj) = self.current.clone().filter(|o| o.kind.has_rows()) else {
            return Vec::new();
        };
        let Some(server) = self.server(&obj.connection).filter(|s| s.is_connected()) else {
            return Vec::new();
        };
        if !self.data.is_for(&obj) {
            let Some(DetailsState::Loaded(d)) = server.details.get(&obj) else {
                return Vec::new();
            };
            self.data = DataView::new(d, self.config.general.page_size, server.conn.read_only);
            self.dirty = true;
        }

        let mut effects = Vec::new();
        if let Some((offset, limit, mode)) = self.data.plan_fetch() {
            let id = self.next_request_id();
            self.data.loading = Some(Pending {
                id,
                offset,
                limit,
                mode,
            });
            let request = Box::new(self.data.request(offset, limit));
            effects.push(DbCommand::FetchRows { id, request }.into());
        }
        if self.data.counting.is_none() && !self.data.count_requested && !self.data.exhausted {
            let id = self.next_request_id();
            self.data.counting = Some(id);
            self.data.count_requested = true;
            let request = Box::new(self.data.request(0, 0));
            effects.push(
                DbCommand::CountRows {
                    id,
                    request,
                    bound: COUNT_BOUND,
                }
                .into(),
            );
        }
        effects
    }

    pub(super) fn on_data_event(&mut self, ev: DbEvent) {
        match ev {
            DbEvent::RowsLoaded {
                id,
                offset,
                rows,
                elapsed,
            } => {
                if self.data.apply_rows(id, offset as usize, rows.rows) {
                    self.data.last_elapsed = Some(elapsed);
                    self.last_op = Some(super::LastOp {
                        label: "lignes".into(),
                        elapsed,
                    });
                }
            }
            DbEvent::RowsFailed { id, error } if self.data.loading.is_some_and(|p| p.id == id) => {
                self.data.loading = None;
                self.data.error = Some(error);
            }
            DbEvent::RowsCounted { id, count } => self.data.apply_count(id, count),
            DbEvent::CountFailed { id, error } if self.data.counting == Some(id) => {
                self.data.counting = None;
                tracing::warn!("row count failed: {error}");
            }
            _ => {}
        }
    }

    /// Reloads the rows, keeping the cursor (after applying changes).
    pub(super) fn requery_data(&mut self) {
        if self.results_mode {
            // A SQL result reloads by running its statement again.
            self.rerun_results = true;
            return;
        }
        self.requery(true);
        self.data.error = None;
    }

    /// Reloads rows after a query change, keeping the cursor where possible.
    fn requery(&mut self, keep_position: bool) {
        let (row, offset) = (self.data.grid.row, self.data.grid.row_offset);
        self.data.reset_rows();
        if keep_position {
            self.data.grid.row = row;
            self.data.grid.row_offset = offset;
            self.data.restore = Some((row, offset));
        }
    }

    pub(super) fn dispatch_data(&mut self, action: Action) -> Vec<Effect> {
        use Action::*;
        if !self.results_mode && self.current.as_ref().is_none_or(|o| !self.data.is_for(o)) {
            return Vec::new();
        }
        if self.results_mode
            && matches!(
                action,
                FilterEditor | FreeWhere | ClearFilters | Sort | ClearSort | Refresh
            )
        {
            self.notify(
                Level::Info,
                "Sur un résultat de requête : modifiez la requête (ORDER BY, WHERE) et relancez-la",
            );
            return Vec::new();
        }
        if matches!(
            action,
            EditCell
                | InsertRow
                | DuplicateRow
                | DeleteRow
                | Undo
                | Redo
                | ShowChanges
                | ApplyChanges
                | DiscardChanges
        ) {
            return self.dispatch_edit(action);
        }
        let rows = self.data.row_count();
        let ins = self.data.ins();
        let g = &mut self.data.grid;
        match action {
            Up => g.move_by(-1, rows),
            Down => g.move_by(1, rows),
            HalfPageDown => g.move_by(g.half_page(), rows),
            HalfPageUp => g.move_by(-g.half_page(), rows),
            PageDown => g.move_by(g.page(), rows),
            PageUp => g.move_by(-g.page(), rows),
            Top => g.top(),
            Bottom => match (self.data.known_total(), self.data.total) {
                (Some(t), _) => self.data.grid.bottom(t + ins),
                (None, Total::AtLeast(n)) => {
                    let end = self.data.end() + ins;
                    self.data.grid.bottom(end);
                    self.notify(
                        Level::Info,
                        format!(
                            "Plus de {} lignes : fin non chargée",
                            super::format::count(n as i64)
                        ),
                    );
                }
                _ => {
                    self.data.jump_end = true;
                    self.notify(Level::Info, "Comptage des lignes en cours…");
                }
            },
            CellLeft | PrevColumn => self.data.move_col(-1),
            CellRight | NextColumn => self.data.move_col(1),
            FirstColumn => self.data.grid.col = 0,
            LastColumn => self.data.move_col(isize::MAX / 2),
            Detail => {
                if self.data.row_at(self.data.grid.row).is_some() {
                    self.overlay = Some(Overlay::RowDetail { scroll: 0 });
                }
            }
            Visual | VisualLine => {
                let line = action == VisualLine;
                self.data.visual = match self.data.visual {
                    Some(v) if v.line == line => None,
                    _ => Some(super::data_view::Visual {
                        row: self.data.grid.row,
                        col: self.data.grid.col,
                        line,
                    }),
                };
            }
            Copy => return self.copy_data(CopyFormat::Tsv, false, false),
            CopyRow => return self.copy_data(CopyFormat::Tsv, true, false),
            CopyAs => self.overlay = Some(Overlay::CopyFormat { cursor: 0 }),
            Export => self.open_export(),
            Search => {
                self.overlay = Some(Overlay::DataSearch {
                    input: LineInput::default(),
                    previous: self.data.search.clone(),
                })
            }
            SearchNext | SearchPrev => self.jump_to_match(action == SearchNext),
            FilterEditor => {
                let columns = self.data.columns.iter().map(|c| c.name.clone()).collect();
                let current = self.data.current_column().map(|c| c.name.clone());
                let editor = super::filter_editor::FilterEditor::new(
                    columns,
                    &self.data.filter,
                    current.as_deref(),
                );
                self.overlay = Some(Overlay::FilterEditor(Box::new(editor)));
            }
            FreeWhere => {
                let text = self.data.filter.free_where.clone().unwrap_or_default();
                self.overlay = Some(Overlay::FreeWhere(LineInput::with_text(text)));
            }
            ClearFilters => {
                if !self.data.filter.is_empty() {
                    self.data.filter = crate::db::data::Filter::default();
                    self.requery(false);
                    self.notify(Level::Info, "Filtres effacés");
                }
            }
            Sort => self.data.cycle_sort(),
            ClearSort => {
                if !self.data.sort.is_empty() {
                    self.data.sort.clear();
                    self.data.relayout();
                    self.requery(false);
                }
            }
            NarrowColumn => self.data.resize_current(-2),
            WidenColumn => self.data.resize_current(2),
            AutoWidth => self.data.auto_width_current(),
            HideColumn => {
                if !self.data.hide_current() {
                    self.notify(
                        Level::Info,
                        "Impossible de masquer la dernière colonne visible",
                    );
                }
            }
            ShowColumns => {
                let n = self.data.show_all_columns();
                if n > 0 {
                    self.notify(Level::Info, format!("{n} colonne(s) réaffichée(s)"));
                }
            }
            Refresh => {
                self.requery(true);
                self.data.error = None;
            }
            Cancel => {
                if self.data.visual.is_some() {
                    self.data.visual = None;
                } else if self.data.search.is_some() {
                    self.data.search = None;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn jump_to_match(&mut self, forward: bool) {
        if self.data.search.is_none() {
            self.notify(Level::Info, "Aucune recherche (/)");
            return;
        }
        match self.data.find_match(forward) {
            Some((row, col)) => {
                self.data.grid.row = row;
                self.data.grid.col = col;
                let rows = self.data.row_count();
                self.data.grid.scroll_into_view(rows);
            }
            None => self.notify(
                Level::Info,
                "Aucune correspondance dans les lignes chargées",
            ),
        }
    }

    /// `y` (cell or selection), `yy` (row), or a format from the chooser.
    fn copy_data(&mut self, fmt: CopyFormat, whole_row: bool, header: bool) -> Vec<Effect> {
        let visual = self.data.visual.is_some();
        let (rows, cols) = self.data.selection(whole_row);
        let single_cell = !visual && !whole_row;
        let (text, n, missing) = {
            let (extract, missing) = self.data.extract(rows, &cols);
            if extract.rows.is_empty() {
                (None, 0, missing)
            } else if single_cell {
                // A single cell is copied raw (NULL as an empty string).
                (
                    Some(extract.rows[0][0].unwrap_or("").to_string()),
                    1,
                    missing,
                )
            } else {
                (
                    Some(export::render(fmt, &extract, header)),
                    extract.rows.len(),
                    missing,
                )
            }
        };
        let Some(text) = text else {
            self.notify(Level::Warning, "Rien à copier (ligne non chargée)");
            return Vec::new();
        };
        let mut msg = if single_cell {
            match self.data.value(self.data.grid.row, self.data.grid.col) {
                Some(None) => "NULL copié (chaîne vide)".to_string(),
                _ => "Cellule copiée".to_string(),
            }
        } else {
            format!("{n} ligne(s) copiée(s) en {}", fmt.label())
        };
        if missing > 0 {
            msg.push_str(&format!(" — {missing} ligne(s) non chargée(s) ignorée(s)"));
        }
        self.data.visual = None;
        self.notify(
            if missing > 0 {
                Level::Warning
            } else {
                Level::Success
            },
            msg,
        );
        vec![Effect::Copy(text)]
    }

    pub(super) fn on_data_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(overlay) = self.overlay.take() else {
            return Vec::new();
        };
        match overlay {
            Overlay::DataSearch {
                mut input,
                previous,
            } => match input.handle_key(&key) {
                InputOutcome::Submit => {
                    self.data.search = (!input.is_empty()).then(|| input.text().to_string());
                    let n = self.data.match_count();
                    if self.data.search.is_some() {
                        self.notify(
                            Level::Info,
                            format!("{n} correspondance(s) dans les lignes chargées"),
                        );
                    }
                }
                InputOutcome::Cancel => self.data.search = previous,
                outcome => {
                    if outcome == InputOutcome::Changed {
                        // Incremental: highlight and move to the first match.
                        self.data.search = (!input.is_empty()).then(|| input.text().to_string());
                        if let Some((row, col)) = self.data.find_match(true) {
                            self.data.grid.row = row;
                            self.data.grid.col = col;
                            let rows = self.data.row_count();
                            self.data.grid.scroll_into_view(rows);
                        }
                    }
                    self.overlay = Some(Overlay::DataSearch { input, previous });
                }
            },
            Overlay::FreeWhere(mut input) => match input.handle_key(&key) {
                InputOutcome::Submit => {
                    let text = input.text().trim().to_string();
                    let text = text
                        .strip_prefix("WHERE ")
                        .or_else(|| text.strip_prefix("where "))
                        .unwrap_or(&text)
                        .to_string();
                    self.data.filter.free_where = (!text.is_empty()).then_some(text);
                    self.requery(false);
                }
                InputOutcome::Cancel => {}
                _ => self.overlay = Some(Overlay::FreeWhere(input)),
            },
            Overlay::FilterEditor(mut editor) => match editor.handle_key(&key) {
                Outcome::Apply => {
                    let free = self.data.filter.free_where.clone();
                    self.data.filter = FilterEditor::into_filter(*editor, free);
                    self.requery(false);
                }
                Outcome::Cancel => {}
                Outcome::Continue => self.overlay = Some(Overlay::FilterEditor(editor)),
            },
            Overlay::CopyFormat { mut cursor } => {
                let n = CopyFormat::ALL.len();
                let chosen = match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
                    KeyCode::Enter => Some(CopyFormat::ALL[cursor]),
                    // Letters pick a format (`j` is JSON), so only arrows move.
                    KeyCode::Down if cursor + 1 < n => {
                        cursor += 1;
                        None
                    }
                    KeyCode::Up => {
                        cursor = cursor.saturating_sub(1);
                        None
                    }
                    KeyCode::Char(c) => CopyFormat::ALL.iter().copied().find(|f| f.key() == c),
                    _ => None,
                };
                match chosen {
                    Some(fmt) => return self.copy_data(fmt, true, true),
                    None => self.overlay = Some(Overlay::CopyFormat { cursor }),
                }
            }
            Overlay::RowDetail { mut scroll } => {
                let rows = self.data.row_count();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return Vec::new(),
                    KeyCode::Char('j') | KeyCode::Down => scroll = scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::Char('g') => scroll = 0,
                    KeyCode::Char(']') => {
                        self.data.grid.move_by(1, rows);
                        scroll = 0;
                    }
                    KeyCode::Char('[') => {
                        self.data.grid.move_by(-1, rows);
                        scroll = 0;
                    }
                    KeyCode::Char('y') => {
                        self.overlay = Some(Overlay::RowDetail { scroll });
                        return self.copy_data(CopyFormat::Json, true, true);
                    }
                    _ => {}
                }
                self.overlay = Some(Overlay::RowDetail { scroll });
            }
            other => self.overlay = Some(other),
        }
        Vec::new()
    }

    /// Cancels the running data query (Ctrl+c).
    pub(super) fn cancel_data(&mut self) -> Option<Vec<Effect>> {
        self.data.loading.take()?;
        self.notify(Level::Warning, "Annulation de la requête demandée");
        self.data.error = Some("requête annulée".into());
        let (connection, database) = self
            .data
            .object
            .as_ref()
            .map(|o| (o.connection.clone(), o.database.clone()))
            .unwrap_or_default();
        Some(vec![
            DbCommand::Cancel {
                connection,
                database,
            }
            .into(),
        ])
    }

    /// Visible rows / total, for the status bar.
    pub fn data_position(&self) -> Option<String> {
        let obj = self.current.as_ref()?;
        if self.tab != Tab::Data || !self.data.is_for(obj) {
            return None;
        }
        let fmt = |n: usize| super::format::count(n as i64);
        let total = match (self.data.known_total(), self.data.total) {
            (Some(t), _) => fmt(t),
            (None, Total::AtLeast(n)) => format!("> {}", fmt(n as usize)),
            _ => match self.data.estimate {
                Some(e) if self.data.filter.is_empty() => format!("~{}", fmt(e.max(0) as usize)),
                _ => "?".into(),
            },
        };
        let ins = self.data.ins();
        if self.data.grid.row < ins {
            return Some(format!("nouvelle ligne {} / {ins}", self.data.grid.row + 1));
        }
        if self.data.known_total() == Some(0) {
            return Some("0 ligne".into());
        }
        Some(format!(
            "ligne {} / {total}",
            fmt(self.data.grid.row - ins + 1)
        ))
    }
}
