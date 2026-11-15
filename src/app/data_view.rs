//! Data tab state: columns, a sliding window of loaded rows, sort, filters,
//! search, visual selection.
//!
//! Rows are addressed by their absolute index in the (filtered, sorted)
//! result. Only a window `[start, start + rows.len())` is kept in memory;
//! [`DataView::plan_fetch`] decides which page to load next from the visible
//! range (lazy loading when approaching the end, jumps with `G`), and the
//! window is trimmed to [`MAX_WINDOW_PAGES`] pages around the cursor.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use super::export::Extract;
use super::grid::{CellStyle, GridSource, GridState, display_width};
use super::staging::{CellChange, Staging};
use super::value::{ValueKind, cell_text};
use crate::db::catalog::{ObjectKind, ObjectRef};
use crate::db::changes::NewValue;
use crate::db::data::{ColumnSpec, DataRequest, Filter, SortDir, SortKey};
use crate::db::schema::{ConstraintKind, ObjectDetails};

/// Pages kept in memory around the cursor.
pub const MAX_WINDOW_PAGES: usize = 10;
/// Rows counted at most (beyond: "more than N").
pub const COUNT_BOUND: u64 = 1_000_000;
/// Automatic column width cap (user resizing goes beyond).
const AUTO_WIDTH_MAX: u16 = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataColumn {
    pub name: String,
    pub data_type: String,
    pub kind: ValueKind,
    pub key: bool,
    /// Filled by the server when left out (default, identity, generated).
    pub has_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Total {
    #[default]
    Unknown,
    Exact(u64),
    AtLeast(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    Replace,
    Append,
    Prepend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub offset: usize,
    pub limit: usize,
    pub mode: LoadMode,
}

/// Visual selection anchored at (`row`, `col`); `line` selects whole rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visual {
    pub row: usize,
    pub col: usize,
    pub line: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DataView {
    pub object: Option<ObjectRef>,
    pub columns: Vec<DataColumn>,
    base_order: Vec<usize>,
    hidden: HashSet<usize>,
    /// Visible columns, in display order (indices into `columns`).
    pub order: Vec<usize>,
    width_override: HashMap<usize, u16>,
    auto_width: Vec<u16>,
    headers: Vec<String>,
    widths: Vec<u16>,
    frozen: usize,
    /// Columns identifying a row (primary key, or a NOT NULL unique key).
    pub key_columns: Vec<String>,
    /// Why rows cannot be edited (milestone 4), `None` if they can.
    pub read_only_reason: Option<String>,
    with_row_version: bool,
    order_by_ctid: bool,

    pub start: usize,
    pub rows: Vec<Vec<Option<String>>>,
    pub total: Total,
    /// The last page was short: the end of the result is loaded.
    pub exhausted: bool,
    pub loading: Option<Pending>,
    pub counting: Option<u64>,
    /// A count was issued for the current query (successful or not).
    pub count_requested: bool,
    pub error: Option<String>,

    pub filter: Filter,
    pub sort: Vec<SortKey>,
    pub grid: GridState,
    /// Committed search (highlighted, `n` / `N`).
    pub search: Option<String>,
    pub visual: Option<Visual>,
    /// `G` pressed before the row count was known.
    pub jump_end: bool,
    pub page_size: usize,
    pub estimate: Option<i64>,
    pub last_elapsed: Option<Duration>,

    /// Edits not yet written. New rows are shown above the loaded ones.
    pub staging: Staging,
    /// Id of the apply in flight, if any.
    pub applying: Option<u64>,
    /// Cursor and scroll to restore once a reload arrives (the grid shrinks
    /// to one placeholder row meanwhile).
    pub restore: Option<(usize, usize)>,
}

impl DataView {
    /// Sets up the view for an object from its details.
    pub fn new(details: &ObjectDetails, page_size: usize, read_only_connection: bool) -> Self {
        let obj = &details.object;
        let columns: Vec<DataColumn> = details
            .columns
            .iter()
            .map(|c| DataColumn {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                kind: ValueKind::of(&c.data_type),
                key: false,
                has_default: c.default.is_some() || c.identity.is_some() || c.generated.is_some(),
            })
            .collect();

        // Row identity: primary key, else a unique constraint on NOT NULL columns.
        let mut key_columns: Vec<String> = details
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect();
        if key_columns.is_empty() {
            key_columns = details
                .constraints
                .iter()
                .filter(|c| c.kind == ConstraintKind::Unique)
                .find(|c| {
                    c.columns.iter().all(|n| {
                        details
                            .columns
                            .iter()
                            .any(|col| &col.name == n && !col.nullable)
                    })
                })
                .map(|c| c.columns.clone())
                .unwrap_or_default();
        }
        let is_table = matches!(obj.kind, ObjectKind::Table | ObjectKind::PartitionedTable);
        let read_only_reason = if !is_table {
            Some(format!("{} : lecture seule", obj.kind.label()))
        } else if key_columns.is_empty() {
            Some(
                "lecture seule : ni clé primaire ni contrainte unique sur des colonnes NOT NULL"
                    .into(),
            )
        } else if read_only_connection {
            Some("connexion en lecture seule".into())
        } else {
            None
        };

        let mut v = DataView {
            object: Some(obj.clone()),
            columns,
            key_columns,
            read_only_reason,
            with_row_version: matches!(
                obj.kind,
                ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::MaterializedView
            ),
            page_size: page_size.max(10),
            estimate: details.stats.as_ref().and_then(|s| s.estimated_rows),
            ..Default::default()
        };
        for c in &mut v.columns {
            c.key = v.key_columns.contains(&c.name);
        }
        let key_idx = v
            .key_columns
            .iter()
            .filter_map(|k| v.columns.iter().position(|c| &c.name == k))
            .collect();
        v.staging = Staging::new(key_idx, v.columns.len());
        v.order_by_ctid = v.key_columns.is_empty()
            && matches!(obj.kind, ObjectKind::Table | ObjectKind::MaterializedView);
        // Key columns first (frozen), then the others in table order.
        let (keys, others): (Vec<usize>, Vec<usize>) =
            (0..v.columns.len()).partition(|i| v.columns[*i].key);
        v.base_order = keys.into_iter().chain(others).collect();
        v.auto_width = v
            .columns
            .iter()
            .map(|c| display_width(&c.name).max(4))
            .collect();
        v.relayout();
        v
    }

    pub fn is_for(&self, obj: &ObjectRef) -> bool {
        self.object.as_ref() == Some(obj)
    }

    pub fn column_specs(&self) -> Vec<ColumnSpec> {
        self.columns
            .iter()
            .map(|c| ColumnSpec {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
            })
            .collect()
    }

    /// Staged new rows, shown first in the grid.
    pub fn ins(&self) -> usize {
        self.staging.inserts()
    }

    /// Index in the result of grid row `row` (`None` for a new row).
    pub fn data_index(&self, row: usize) -> Option<usize> {
        row.checked_sub(self.ins())
    }

    /// Loaded row shown at grid row `row`.
    pub fn row_at(&self, row: usize) -> Option<&Vec<Option<String>>> {
        self.data_index(row).and_then(|i| self.row(i))
    }

    /// The request for `[offset, offset + limit)`.
    pub fn request(&self, offset: usize, limit: usize) -> DataRequest {
        DataRequest {
            object: self.object.clone().expect("data view without object"),
            columns: self.column_specs(),
            filter: self.filter.clone(),
            sort: self.sort.clone(),
            tiebreak: self.key_columns.clone(),
            order_by_ctid: self.order_by_ctid,
            with_row_version: self.with_row_version,
            offset: offset as u64,
            limit: limit as u64,
        }
    }

    /// Forgets loaded rows after a sort / filter change (keeps the column).
    pub fn reset_rows(&mut self) {
        self.restore = None;
        self.rows.clear();
        self.start = 0;
        self.total = Total::Unknown;
        self.exhausted = false;
        self.loading = None;
        self.counting = None;
        self.count_requested = false;
        self.error = None;
        self.visual = None;
        self.jump_end = false;
        self.grid.row = 0;
        self.grid.row_offset = 0;
    }

    pub fn end(&self) -> usize {
        self.start + self.rows.len()
    }

    /// Known number of rows, if any.
    pub fn known_total(&self) -> Option<usize> {
        match self.total {
            Total::Exact(n) => Some(n as usize),
            _ if self.exhausted => Some(self.end()),
            _ => None,
        }
    }

    /// Rows the grid can scroll through: the total when known, else the
    /// loaded end plus one placeholder row that triggers the next page.
    pub fn row_count(&self) -> usize {
        self.ins() + self.known_total().unwrap_or_else(|| self.end() + 1)
    }

    pub fn row(&self, absolute: usize) -> Option<&Vec<Option<String>>> {
        absolute
            .checked_sub(self.start)
            .and_then(|i| self.rows.get(i))
    }

    /// Loaded value of display column `dcol` at grid row `row`: `None` if
    /// not loaded (or a new row), `Some(None)` for SQL NULL.
    pub fn value(&self, row: usize, dcol: usize) -> Option<Option<&str>> {
        let col = *self.order.get(dcol)?;
        self.row_at(row)
            .map(|r| r.get(col).and_then(|v| v.as_deref()))
    }

    /// Which page to load for the visible rows, if any.
    pub fn plan_fetch(&self) -> Option<(usize, usize, LoadMode)> {
        if self.loading.is_some() || self.error.is_some() || self.object.is_none() {
            return None;
        }
        let page = self.page_size;
        let view = self.grid.viewport_rows.max(1);
        let first = self
            .grid
            .row_offset
            .min(self.grid.row)
            .saturating_sub(self.ins());
        let mut last = first + view + page / 4;
        if let Some(t) = self.known_total() {
            last = last.min(t);
        }
        let (start, end) = (self.start, self.end());
        let replace = || {
            let offset = first.saturating_sub(page / 4);
            Some((offset, page.max(view + page / 2), LoadMode::Replace))
        };
        if self.rows.is_empty() {
            return if self.known_total() == Some(0) {
                None
            } else {
                replace()
            };
        }
        if first >= start && last <= end {
            return None;
        }
        if first < start {
            if last > end || start - first > page {
                return replace();
            }
            let offset = start.saturating_sub(page);
            return Some((offset, start - offset, LoadMode::Prepend));
        }
        // last > end
        if self.exhausted {
            None
        } else if first <= end + page {
            Some((end, page, LoadMode::Append))
        } else {
            replace()
        }
    }

    /// Integrates a loaded page. Returns `false` if the response is stale.
    pub fn apply_rows(&mut self, id: u64, offset: usize, rows: Vec<Vec<Option<String>>>) -> bool {
        let Some(p) = self.loading.filter(|p| p.id == id) else {
            return false;
        };
        self.loading = None;
        let n = rows.len();
        let short = n < p.limit;
        let mode = match p.mode {
            LoadMode::Append if offset == self.end() && !self.rows.is_empty() => LoadMode::Append,
            LoadMode::Prepend if offset + n == self.start => LoadMode::Prepend,
            _ => LoadMode::Replace,
        };
        match mode {
            LoadMode::Append => self.rows.extend(rows),
            LoadMode::Prepend => {
                self.rows.splice(0..0, rows);
                self.start = offset;
            }
            LoadMode::Replace => {
                self.rows = rows;
                self.start = offset;
                self.exhausted = false;
            }
        }
        if short && mode != LoadMode::Prepend {
            self.exhausted = true;
            self.total = Total::Exact((offset + n) as u64);
        }
        if mode == LoadMode::Replace {
            self.measure_widths();
            if let Some((row, offset)) = self.restore.take() {
                self.grid.row = row;
                self.grid.row_offset = offset;
            }
        }
        self.trim_window();
        if let Some(t) = self.known_total() {
            self.grid.row = self.grid.row.min((t + self.ins()).saturating_sub(1));
        }
        true
    }

    /// Keeps at most `MAX_WINDOW_PAGES` pages, centered on the cursor.
    fn trim_window(&mut self) {
        let max = self.page_size * MAX_WINDOW_PAGES;
        if self.rows.len() <= max {
            return;
        }
        let end = self.end();
        let wanted = self
            .grid
            .row
            .saturating_sub(self.ins())
            .saturating_sub(max / 2)
            .clamp(self.start, end - max);
        let drop_front = wanted - self.start;
        self.rows.drain(..drop_front);
        self.rows.truncate(max);
        self.start = wanted;
        if self.end() < end {
            // Rows past the window must be fetched again.
            self.exhausted = false;
        }
    }

    pub fn apply_count(&mut self, id: u64, count: crate::db::data::RowCount) {
        if self.counting != Some(id) {
            return;
        }
        self.counting = None;
        self.total = match count {
            crate::db::data::RowCount::Exact(n) => Total::Exact(n),
            crate::db::data::RowCount::AtLeast(n) if !self.exhausted => Total::AtLeast(n),
            crate::db::data::RowCount::AtLeast(_) => self.total,
        };
        if self.jump_end {
            self.jump_end = false;
            if let Some(t) = self.known_total() {
                self.grid.bottom(t + self.ins());
            }
        }
    }

    // --------------------------------------------------------------- layout

    fn measure_widths(&mut self) {
        for (i, c) in self.columns.iter().enumerate() {
            let content = self
                .rows
                .iter()
                .take(self.page_size)
                .map(|r| match &r[i] {
                    Some(v) => display_width(cell_text(c.kind, v)),
                    None => 4,
                })
                .max()
                .unwrap_or(0);
            self.auto_width[i] = content
                .max(display_width(&c.name) + 3)
                .clamp(4, AUTO_WIDTH_MAX);
        }
        self.relayout();
    }

    /// Recomputes visible columns, headers (with sort markers) and widths.
    pub fn relayout(&mut self) {
        self.order = self
            .base_order
            .iter()
            .copied()
            .filter(|i| !self.hidden.contains(i))
            .collect();
        self.frozen = self
            .order
            .iter()
            .take_while(|i| self.columns[**i].key)
            .count();
        self.headers = self
            .order
            .iter()
            .map(|i| {
                let name = &self.columns[*i].name;
                match self.sort.iter().position(|k| &k.column == name) {
                    Some(pos) => {
                        let arrow = if self.sort[pos].dir == SortDir::Asc {
                            "↑"
                        } else {
                            "↓"
                        };
                        if self.sort.len() > 1 {
                            format!("{name} {arrow}{}", pos + 1)
                        } else {
                            format!("{name} {arrow}")
                        }
                    }
                    None => name.clone(),
                }
            })
            .collect();
        self.widths = self
            .order
            .iter()
            .map(|i| {
                self.width_override
                    .get(i)
                    .copied()
                    .unwrap_or(self.auto_width[*i])
            })
            .collect();
        self.grid.col = self.grid.col.min(self.order.len().saturating_sub(1));
    }

    pub fn current_column(&self) -> Option<&DataColumn> {
        self.order.get(self.grid.col).map(|i| &self.columns[*i])
    }

    pub fn resize_current(&mut self, delta: i16) {
        if let Some(&i) = self.order.get(self.grid.col) {
            let w = self
                .width_override
                .get(&i)
                .copied()
                .unwrap_or(self.auto_width[i]);
            let w = (w as i16 + delta).clamp(3, 200) as u16;
            self.width_override.insert(i, w);
            self.relayout();
        }
    }

    pub fn auto_width_current(&mut self) {
        if let Some(i) = self.order.get(self.grid.col) {
            self.width_override.remove(i);
            self.relayout();
        }
    }

    /// Hides the current column; returns `false` for the last visible one.
    pub fn hide_current(&mut self) -> bool {
        if self.order.len() <= 1 {
            return false;
        }
        if let Some(&i) = self.order.get(self.grid.col) {
            self.hidden.insert(i);
            self.relayout();
        }
        true
    }

    pub fn show_all_columns(&mut self) -> usize {
        let n = self.hidden.len();
        self.hidden.clear();
        self.relayout();
        n
    }

    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// `s` on a column: none -> ascending -> descending -> none.
    pub fn cycle_sort(&mut self) {
        let Some(name) = self.current_column().map(|c| c.name.clone()) else {
            return;
        };
        match self.sort.iter().position(|k| k.column == name) {
            None => self.sort.push(SortKey {
                column: name,
                dir: SortDir::Asc,
            }),
            Some(i) if self.sort[i].dir == SortDir::Asc => self.sort[i].dir = SortDir::Desc,
            Some(i) => {
                self.sort.remove(i);
            }
        }
        self.relayout();
        self.reset_rows();
    }

    // --------------------------------------------------------------- cursor

    pub fn move_col(&mut self, delta: isize) {
        let n = self.order.len();
        if n > 0 {
            self.grid.col = self.grid.col.saturating_add_signed(delta).min(n - 1);
        }
    }

    // --------------------------------------------------------------- search

    fn search_matches(&self, value: Option<&str>, kind: ValueKind) -> bool {
        let (Some(q), Some(v)) = (self.search.as_deref(), value) else {
            return false;
        };
        contains(cell_text(kind, v), q)
    }

    /// Next loaded cell matching the search, from the cursor, wrapping.
    pub fn find_match(&self, forward: bool) -> Option<(usize, usize)> {
        self.search.as_ref()?;
        let ncols = self.order.len();
        let total = self.rows.len() * ncols;
        if total == 0 {
            return None;
        }
        let cur_row = self
            .grid
            .row
            .saturating_sub(self.ins())
            .checked_sub(self.start)
            .filter(|r| *r < self.rows.len())
            .unwrap_or(0);
        let cur = cur_row * ncols + self.grid.col.min(ncols - 1);
        for step in 1..=total {
            let idx = if forward {
                (cur + step) % total
            } else {
                (cur + total - step % total) % total
            };
            let (r, dc) = (idx / ncols, idx % ncols);
            let col = self.order[dc];
            if self.search_matches(self.rows[r][col].as_deref(), self.columns[col].kind) {
                return Some((self.ins() + self.start + r, dc));
            }
        }
        None
    }

    pub fn match_count(&self) -> usize {
        if self.search.is_none() {
            return 0;
        }
        self.rows
            .iter()
            .map(|r| {
                self.order
                    .iter()
                    .filter(|c| self.search_matches(r[**c].as_deref(), self.columns[**c].kind))
                    .count()
            })
            .sum()
    }

    pub fn is_match(&self, row: usize, dcol: usize) -> bool {
        let Some(&col) = self.order.get(dcol) else {
            return false;
        };
        matches!(self.value(row, dcol), Some(v) if self.search_matches(v, self.columns[col].kind))
    }

    // ------------------------------------------------------------ selection

    /// Selected rows and display columns: the visual selection, else the
    /// current row (all columns) when `whole_row`, else the current cell.
    pub fn selection(&self, whole_row: bool) -> (Range<usize>, Vec<usize>) {
        let all: Vec<usize> = (0..self.order.len()).collect();
        match self.visual {
            Some(v) => {
                let rows = v.row.min(self.grid.row)..v.row.max(self.grid.row) + 1;
                let cols = if v.line {
                    all
                } else {
                    (v.col.min(self.grid.col)..=v.col.max(self.grid.col)).collect()
                };
                (rows, cols)
            }
            None if whole_row => (self.grid.row..self.grid.row + 1, all),
            None => (self.grid.row..self.grid.row + 1, vec![self.grid.col]),
        }
    }

    pub fn is_selected(&self, row: usize, dcol: usize) -> bool {
        if self.visual.is_none() {
            return false;
        }
        let (rows, cols) = self.selection(false);
        rows.contains(&row) && cols.contains(&dcol)
    }

    /// Loaded part of a selection, for copying. Also returns how many
    /// selected rows were not loaded.
    pub fn extract(&self, rows: Range<usize>, dcols: &[usize]) -> (Extract<'_>, usize) {
        let obj = self.object.as_ref().expect("data view without object");
        let columns = dcols
            .iter()
            .map(|d| {
                let c = &self.columns[self.order[*d]];
                (c.name.as_str(), c.kind)
            })
            .collect();
        let mut missing = 0;
        let mut out = Vec::new();
        for r in rows {
            match self.row_at(r) {
                Some(row) => out.push(
                    dcols
                        .iter()
                        .map(|d| row[self.order[*d]].as_deref())
                        .collect(),
                ),
                None => missing += 1,
            }
        }
        (
            Extract {
                schema: &obj.schema,
                table: &obj.name,
                columns,
                rows: out,
            },
            missing,
        )
    }

    /// Short description of sort and filters for the header line.
    pub fn describe_query(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.sort.is_empty() {
            let keys: Vec<String> = self
                .sort
                .iter()
                .map(|k| {
                    format!(
                        "{} {}",
                        k.column,
                        if k.dir == SortDir::Asc { "asc" } else { "desc" }
                    )
                })
                .collect();
            parts.push(format!("tri : {}", keys.join(", ")));
        }
        let n = self.filter.conditions.len();
        if n > 0 {
            let comb = if n > 1 {
                format!(" ({})", self.filter.combinator.keyword())
            } else {
                String::new()
            };
            parts.push(format!(
                "filtre : {n} condition{}{comb}",
                if n > 1 { "s" } else { "" }
            ));
        }
        if let Some(w) = &self.filter.free_where {
            parts.push(format!("WHERE {w}"));
        }
        if !self.hidden.is_empty() {
            parts.push(format!("{} colonne(s) masquée(s)", self.hidden.len()));
        }
        parts
    }
}

/// Smart-case substring search: case-insensitive unless the query has
/// an uppercase letter.
pub fn contains(haystack: &str, query: &str) -> bool {
    if query.chars().any(char::is_uppercase) {
        haystack.contains(query)
    } else {
        haystack.to_lowercase().contains(&query.to_lowercase())
    }
}

impl GridSource for DataView {
    fn headers(&self) -> &[String] {
        &self.headers
    }

    fn widths(&self) -> &[u16] {
        &self.widths
    }

    fn row_count(&self) -> usize {
        self.row_count()
    }

    fn cell(&self, row: usize, dcol: usize) -> (Cow<'_, str>, CellStyle) {
        let Some(&col) = self.order.get(dcol) else {
            return (Cow::Borrowed(""), CellStyle::Normal);
        };
        let kind = self.columns[col].kind;
        if row < self.ins() {
            let v = &self.staging.state.inserts[row][col];
            let text = match v {
                NewValue::Text(t) => cell_text(kind, t),
                other => other.display(),
            };
            return (Cow::Borrowed(text), CellStyle::Inserted);
        }
        if let Some(r) = self.row_at(row)
            && let Some(change) = self.staging.cell(r, col)
        {
            return match change {
                CellChange::Deleted => (
                    Cow::Borrowed(r[col].as_deref().map_or("NULL", |v| cell_text(kind, v))),
                    CellStyle::Deleted,
                ),
                CellChange::Modified(v) | CellChange::Conflict(v) => {
                    let text = match v {
                        NewValue::Text(t) => cell_text(kind, t),
                        other => other.display(),
                    };
                    let style = if matches!(change, CellChange::Conflict(_)) {
                        CellStyle::Conflict
                    } else {
                        CellStyle::Modified
                    };
                    (Cow::Borrowed(text), style)
                }
            };
        }
        match self.value(row, dcol) {
            None => (Cow::Borrowed("…"), CellStyle::Dim),
            Some(None) => (Cow::Borrowed("NULL"), CellStyle::Null),
            Some(Some(v)) => {
                let style = if self.columns[col].key {
                    CellStyle::Key
                } else {
                    CellStyle::Normal
                };
                (Cow::Borrowed(cell_text(self.columns[col].kind, v)), style)
            }
        }
    }

    fn frozen(&self) -> usize {
        self.frozen
    }

    fn right_aligned(&self, dcol: usize) -> bool {
        self.order
            .get(dcol)
            .is_some_and(|i| self.columns[*i].kind.is_numeric())
    }
}

#[cfg(test)]
mod tests;
