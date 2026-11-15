//! Tree state: `database > schema > group > object`, flattened into rows.
//!
//! Every database of the server is a root node; its catalog is loaded when
//! the node is expanded (see `App::sync_catalogs`). Rows are rebuilt only
//! when catalogs, the expansion set or the filter change; rendering just
//! slices `rows[offset..offset + viewport]`.

use std::collections::HashSet;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use super::input::LineInput;
use crate::db::catalog::{Catalog, DatabaseInfo, ObjectGroup, ObjectKind, ObjectRef, Schema};

/// A database of the server and the loading state of its catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseEntry {
    pub info: DatabaseInfo,
    pub catalog: Option<Catalog>,
    pub loading: bool,
    pub error: Option<String>,
}

impl DatabaseEntry {
    pub fn new(info: DatabaseInfo) -> Self {
        Self {
            info,
            catalog: None,
            loading: false,
            error: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.info.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Database(String),
    /// (database, schema)
    Schema(String, String),
    /// (database, schema, group)
    Group(String, String, ObjectGroup),
    Object(ObjectRef),
}

impl NodeKey {
    pub fn database(&self) -> &str {
        match self {
            NodeKey::Database(d) | NodeKey::Schema(d, _) | NodeKey::Group(d, _, _) => d,
            NodeKey::Object(o) => &o.database,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Database,
    Schema,
    Group(ObjectGroup),
    Object(ObjectKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub key: NodeKey,
    pub kind: RowKind,
    pub depth: u8,
    pub label: String,
    pub expanded: bool,
    /// Number of children (objects in a group, matches when filtering).
    pub count: Option<usize>,
    /// Dim text after the label (database size, loading, error).
    pub note: Option<String>,
    /// Character indices of `label` matched by the filter.
    pub highlights: Vec<usize>,
    /// Fuzzy match score when filtering (higher is better), 0 otherwise.
    pub score: u32,
}

impl TreeRow {
    fn node(key: NodeKey, kind: RowKind, depth: u8, label: String, expanded: bool) -> Self {
        Self {
            key,
            kind,
            depth,
            label,
            expanded,
            count: None,
            note: None,
            highlights: Vec::new(),
            score: 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct TreeState {
    pub rows: Vec<TreeRow>,
    pub cursor: usize,
    pub offset: usize,
    /// Visible height, updated by the renderer.
    pub viewport: usize,
    pub filter: LineInput,
    pub filter_editing: bool,
    expanded: HashSet<NodeKey>,
    /// Databases whose first schema was auto-expanded once loaded.
    initialised: HashSet<String>,
}

impl TreeState {
    /// Rebuilds rows, keeping the cursor on the same node when it still exists.
    pub fn rebuild(&mut self, dbs: &[DatabaseEntry]) {
        self.rebuild_with(dbs, true);
    }

    /// To call when the filter text changed: the cursor jumps to the best
    /// match instead of staying on the object matched by a shorter query.
    pub fn filter_changed(&mut self, dbs: &[DatabaseEntry]) {
        // With an empty filter, staying on the last match keeps the context.
        let keep = self.filter.is_empty();
        self.rebuild_with(dbs, keep);
    }

    /// Expands a database node (the one connected to, at startup).
    pub fn expand_database(&mut self, name: &str) {
        self.expanded.insert(NodeKey::Database(name.to_string()));
    }

    pub fn is_expanded(&self, key: &NodeKey) -> bool {
        self.expanded.contains(key)
    }

    fn rebuild_with(&mut self, dbs: &[DatabaseEntry], keep_selection: bool) {
        // First load of a catalog: open its first user schema and its tables.
        for db in dbs {
            let Some(catalog) = &db.catalog else { continue };
            if self.initialised.insert(db.name().to_string())
                && let Some(first) = catalog.schemas.iter().find(|s| !s.is_system)
            {
                self.expanded
                    .insert(NodeKey::Schema(db.name().into(), first.name.clone()));
                self.expanded.insert(NodeKey::Group(
                    db.name().into(),
                    first.name.clone(),
                    ObjectGroup::Tables,
                ));
            }
        }
        // While filtering, only an object selection is worth keeping: a
        // database, schema or group would hide the first match.
        let selected = self
            .selected()
            .filter(|_| keep_selection)
            .filter(|r| self.filter.is_empty() || matches!(r.kind, RowKind::Object(_)))
            .map(|r| r.key.clone());
        self.rows = if self.filter.is_empty() {
            self.build_rows(dbs)
        } else {
            build_filtered(dbs, self.filter.text())
        };
        self.cursor = selected
            .and_then(|k| self.rows.iter().position(|r| r.key == k))
            .unwrap_or_else(|| {
                if self.filter.is_empty() {
                    self.cursor
                } else {
                    self.rows
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| matches!(r.kind, RowKind::Object(_)))
                        // First of the best scores (max_by_key keeps the last).
                        .min_by_key(|(_, r)| std::cmp::Reverse(r.score))
                        .map_or(0, |(i, _)| i)
                }
            })
            .min(self.rows.len().saturating_sub(1));
        self.scroll_into_view();
    }

    fn build_rows(&self, dbs: &[DatabaseEntry]) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for db in dbs {
            let dkey = NodeKey::Database(db.name().to_string());
            let d_open = self.expanded.contains(&dkey);
            let mut row = TreeRow::node(dkey, RowKind::Database, 0, db.name().to_string(), d_open);
            row.note = database_note(db);
            rows.push(row);
            let (true, Some(catalog)) = (d_open, &db.catalog) else {
                continue;
            };
            for schema in &catalog.schemas {
                let skey = NodeKey::Schema(db.name().into(), schema.name.clone());
                let s_open = self.expanded.contains(&skey);
                rows.push(TreeRow::node(
                    skey,
                    RowKind::Schema,
                    1,
                    schema.name.clone(),
                    s_open,
                ));
                if !s_open {
                    continue;
                }
                for group in ObjectGroup::ALL {
                    let objects: Vec<_> = schema.objects_in(group).collect();
                    if objects.is_empty() {
                        continue;
                    }
                    let gkey = NodeKey::Group(db.name().into(), schema.name.clone(), group);
                    let g_open = self.expanded.contains(&gkey);
                    let mut g =
                        TreeRow::node(gkey, RowKind::Group(group), 2, group.label().into(), g_open);
                    g.count = Some(objects.len());
                    rows.push(g);
                    if g_open {
                        rows.extend(
                            objects
                                .into_iter()
                                .map(|(_, o)| object_row(db.name(), schema, o)),
                        );
                    }
                }
            }
        }
        rows
    }

    pub fn selected(&self) -> Option<&TreeRow> {
        self.rows.get(self.cursor)
    }

    pub fn selected_object(&self) -> Option<&ObjectRef> {
        match &self.selected()?.key {
            NodeKey::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Schema of the node under the cursor.
    pub fn selected_schema(&self) -> Option<&str> {
        Some(match &self.selected()?.key {
            NodeKey::Schema(_, s) | NodeKey::Group(_, s, _) => s,
            NodeKey::Object(o) => &o.schema,
            NodeKey::Database(_) => return None,
        })
    }

    pub fn selected_database(&self) -> Option<&str> {
        self.selected().map(|r| r.key.database())
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(max);
        self.scroll_into_view();
    }

    pub fn top(&mut self) {
        self.cursor = 0;
        self.scroll_into_view();
    }

    pub fn bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
        self.scroll_into_view();
    }

    pub fn half_page(&self) -> isize {
        (self.viewport / 2).max(1) as isize
    }

    pub fn page(&self) -> isize {
        self.viewport.saturating_sub(1).max(1) as isize
    }

    /// Expands the node, or moves to its first child if already expanded.
    pub fn expand(&mut self, dbs: &[DatabaseEntry]) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if matches!(row.kind, RowKind::Object(_)) {
            return;
        }
        if row.expanded {
            // A database still loading has no child yet.
            if self
                .rows
                .get(self.cursor + 1)
                .is_some_and(|r| r.depth > row.depth)
            {
                self.move_by(1);
            }
        } else if self.filter.is_empty() {
            self.expanded.insert(row.key);
            self.rebuild(dbs);
        }
    }

    /// Collapses the node, or moves to its parent.
    pub fn collapse(&mut self, dbs: &[DatabaseEntry]) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if row.expanded && self.filter.is_empty() && !matches!(row.kind, RowKind::Object(_)) {
            self.expanded.remove(&row.key);
            self.rebuild(dbs);
            return;
        }
        if row.depth == 0 {
            return;
        }
        if let Some(parent) = self.rows[..self.cursor]
            .iter()
            .rposition(|r| r.depth < row.depth)
        {
            self.cursor = parent;
            self.scroll_into_view();
        }
    }

    /// Toggles a folder (Enter on a database, schema or group).
    pub fn toggle(&mut self, dbs: &[DatabaseEntry]) {
        match self.selected().map(|r| r.expanded) {
            Some(true) => self.collapse(dbs),
            Some(false) => self.expand(dbs),
            None => {}
        }
    }

    /// Collapses everything except the database under the cursor.
    pub fn collapse_all(&mut self, dbs: &[DatabaseEntry]) {
        let db = self.selected_database().map(str::to_string);
        self.expanded.clear();
        if let Some(d) = &db {
            self.expanded.insert(NodeKey::Database(d.clone()));
        }
        self.rebuild(dbs);
        if let Some(d) = db {
            let key = NodeKey::Database(d);
            if let Some(i) = self.rows.iter().position(|r| r.key == key) {
                self.cursor = i;
                self.scroll_into_view();
            }
        }
    }

    /// Expands the parents of `obj` and puts the cursor on it.
    pub fn reveal(&mut self, dbs: &[DatabaseEntry], obj: &ObjectRef) {
        self.expanded
            .insert(NodeKey::Database(obj.database.clone()));
        self.expanded
            .insert(NodeKey::Schema(obj.database.clone(), obj.schema.clone()));
        self.expanded.insert(NodeKey::Group(
            obj.database.clone(),
            obj.schema.clone(),
            obj.kind.group(),
        ));
        self.rebuild(dbs);
        let key = NodeKey::Object(obj.clone());
        if let Some(i) = self.rows.iter().position(|r| r.key == key) {
            self.cursor = i;
            self.scroll_into_view();
        }
    }

    pub fn scroll_into_view(&mut self) {
        let h = self.viewport.max(1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + h {
            self.offset = self.cursor + 1 - h;
        }
        let max_offset = self.rows.len().saturating_sub(h);
        self.offset = self.offset.min(max_offset);
    }
}

fn database_note(db: &DatabaseEntry) -> Option<String> {
    if !db.info.can_connect {
        Some("accès refusé".into())
    } else if db.loading {
        Some("chargement…".into())
    } else if db.error.is_some() {
        Some("erreur".into())
    } else {
        db.info.size_bytes.map(super::format::bytes)
    }
}

fn object_row(database: &str, schema: &Schema, o: &crate::db::catalog::DbObject) -> TreeRow {
    TreeRow::node(
        NodeKey::Object(ObjectRef::new(database, &schema.name, o)),
        RowKind::Object(o.kind),
        3,
        o.display_name(),
        false,
    )
}

/// Rows matching a fuzzy filter, across loaded databases: parents are shown
/// expanded, empty branches hidden. A filter containing `.` is matched
/// against `schema.object`.
fn build_filtered(dbs: &[DatabaseEntry], query: &str) -> Vec<TreeRow> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let qualified = query.contains('.');
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut rows = Vec::new();

    for db in dbs {
        let Some(catalog) = &db.catalog else { continue };
        let mut db_rows = Vec::new();
        for schema in &catalog.schemas {
            let mut schema_rows = Vec::new();
            for group in ObjectGroup::ALL {
                let mut group_rows = Vec::new();
                for (_, o) in schema.objects_in(group) {
                    let haystack_text = if qualified {
                        format!("{}.{}", schema.name, o.name)
                    } else {
                        o.name.clone()
                    };
                    indices.clear();
                    let haystack = Utf32Str::new(&haystack_text, &mut buf);
                    let Some(score) = pattern.indices(haystack, &mut matcher, &mut indices) else {
                        continue;
                    };
                    let skip = if qualified {
                        schema.name.chars().count() as u32 + 1
                    } else {
                        0
                    };
                    let mut highlights: Vec<usize> = indices
                        .iter()
                        .filter(|i| **i >= skip)
                        .map(|i| (*i - skip) as usize)
                        .collect();
                    highlights.sort_unstable();
                    highlights.dedup();
                    let mut row = object_row(db.name(), schema, o);
                    row.highlights = highlights;
                    row.score = score;
                    group_rows.push(row);
                }
                if group_rows.is_empty() {
                    continue;
                }
                // Best matches first; the sort is stable, so ties keep name order.
                group_rows.sort_by_key(|r| std::cmp::Reverse(r.score));
                let key = NodeKey::Group(db.name().into(), schema.name.clone(), group);
                let mut g =
                    TreeRow::node(key, RowKind::Group(group), 2, group.label().into(), true);
                g.count = Some(group_rows.len());
                schema_rows.push(g);
                schema_rows.append(&mut group_rows);
            }
            if schema_rows.is_empty() {
                continue;
            }
            let key = NodeKey::Schema(db.name().into(), schema.name.clone());
            db_rows.push(TreeRow::node(
                key,
                RowKind::Schema,
                1,
                schema.name.clone(),
                true,
            ));
            db_rows.append(&mut schema_rows);
        }
        if db_rows.is_empty() {
            continue;
        }
        let key = NodeKey::Database(db.name().into());
        rows.push(TreeRow::node(
            key,
            RowKind::Database,
            0,
            db.name().into(),
            true,
        ));
        rows.append(&mut db_rows);
    }
    rows
}

#[cfg(test)]
mod tests;
