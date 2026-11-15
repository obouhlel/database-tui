//! Tree state: `connection > database > schema > group > object`,
//! flattened into rows.
//!
//! Connections are the roots: expanding one connects it (see
//! `App::sync_connections`), expanding a database loads its catalog (see
//! `App::sync_catalogs`). Rows are rebuilt only when servers, the expansion
//! set or the filter change; rendering just slices
//! `rows[offset..offset + viewport]`.

use std::collections::HashSet;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use super::input::LineInput;
use super::server::{ConnState, Server};
use crate::db::catalog::{
    Catalog, DatabaseInfo, DbObject, ObjectGroup, ObjectKind, ObjectRef, Schema,
};

/// A database of a server and the loading state of its catalog.
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
    Connection(String),
    /// (connection, database)
    Database(String, String),
    /// (connection, database, schema)
    Schema(String, String, String),
    /// (connection, database, schema, group)
    Group(String, String, String, ObjectGroup),
    Object(ObjectRef),
}

impl NodeKey {
    pub fn connection(&self) -> &str {
        match self {
            NodeKey::Connection(c)
            | NodeKey::Database(c, _)
            | NodeKey::Schema(c, _, _)
            | NodeKey::Group(c, _, _, _) => c,
            NodeKey::Object(o) => &o.connection,
        }
    }

    pub fn database(&self) -> Option<&str> {
        match self {
            NodeKey::Connection(_) => None,
            NodeKey::Database(_, d) | NodeKey::Schema(_, d, _) | NodeKey::Group(_, d, _, _) => {
                Some(d)
            }
            NodeKey::Object(o) => Some(&o.database),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Connection,
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
    /// Dim text after the label (state, size, ...).
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
    initialised: HashSet<(String, String)>,
}

impl TreeState {
    /// Rebuilds rows, keeping the cursor on the same node when it still exists.
    pub fn rebuild(&mut self, servers: &[Server]) {
        self.rebuild_with(servers, true);
    }

    /// To call when the filter text changed: the cursor jumps to the best
    /// match instead of staying on the object matched by a shorter query.
    pub fn filter_changed(&mut self, servers: &[Server]) {
        // With an empty filter, staying on the last match keeps the context.
        let keep = self.filter.is_empty();
        self.rebuild_with(servers, keep);
    }

    pub fn expand_node(&mut self, key: NodeKey) {
        self.expanded.insert(key);
    }

    pub fn collapse_node(&mut self, key: &NodeKey) {
        self.expanded.remove(key);
    }

    pub fn is_expanded(&self, key: &NodeKey) -> bool {
        self.expanded.contains(key)
    }

    /// Forgets the expansion state below a connection (after disconnecting).
    pub fn forget_connection(&mut self, connection: &str) {
        self.expanded
            .retain(|k| k.connection() != connection || matches!(k, NodeKey::Connection(_)));
        self.initialised.retain(|(c, _)| c != connection);
    }

    /// Puts the cursor on a node if it is visible.
    pub fn select(&mut self, key: &NodeKey) -> bool {
        match self.rows.iter().position(|r| &r.key == key) {
            Some(i) => {
                self.cursor = i;
                self.scroll_into_view();
                true
            }
            None => false,
        }
    }

    fn rebuild_with(&mut self, servers: &[Server], keep_selection: bool) {
        // First load of a catalog: open its first user schema and its tables.
        for s in servers {
            for db in &s.databases {
                let Some(catalog) = &db.catalog else { continue };
                if self.initialised.insert((s.name().into(), db.name().into()))
                    && let Some(first) = catalog.schemas.iter().find(|x| !x.is_system)
                {
                    let (c, d) = (s.name().to_string(), db.name().to_string());
                    self.expanded
                        .insert(NodeKey::Schema(c.clone(), d.clone(), first.name.clone()));
                    self.expanded.insert(NodeKey::Group(
                        c,
                        d,
                        first.name.clone(),
                        ObjectGroup::Tables,
                    ));
                }
            }
        }
        // While filtering, only an object selection is worth keeping.
        let selected = self
            .selected()
            .filter(|_| keep_selection)
            .filter(|r| self.filter.is_empty() || matches!(r.kind, RowKind::Object(_)))
            .map(|r| r.key.clone());
        self.rows = if self.filter.is_empty() {
            self.build_rows(servers)
        } else {
            build_filtered(servers, self.filter.text())
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

    fn build_rows(&self, servers: &[Server]) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for s in servers {
            let c = s.name().to_string();
            let ckey = NodeKey::Connection(c.clone());
            let c_open = self.expanded.contains(&ckey);
            let mut row = TreeRow::node(ckey, RowKind::Connection, 0, c.clone(), c_open);
            row.note = connection_note(s);
            rows.push(row);
            if !(c_open && s.is_connected()) {
                continue;
            }
            for db in &s.databases {
                let dkey = NodeKey::Database(c.clone(), db.name().to_string());
                let d_open = self.expanded.contains(&dkey);
                let mut row =
                    TreeRow::node(dkey, RowKind::Database, 1, db.name().to_string(), d_open);
                row.note = database_note(db);
                rows.push(row);
                let (true, Some(catalog)) = (d_open, &db.catalog) else {
                    continue;
                };
                for schema in &catalog.schemas {
                    let skey = NodeKey::Schema(c.clone(), db.name().into(), schema.name.clone());
                    let s_open = self.expanded.contains(&skey);
                    rows.push(TreeRow::node(
                        skey,
                        RowKind::Schema,
                        2,
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
                        let gkey =
                            NodeKey::Group(c.clone(), db.name().into(), schema.name.clone(), group);
                        let g_open = self.expanded.contains(&gkey);
                        let mut g = TreeRow::node(
                            gkey,
                            RowKind::Group(group),
                            3,
                            group.label().into(),
                            g_open,
                        );
                        g.count = Some(objects.len());
                        rows.push(g);
                        if g_open {
                            rows.extend(
                                objects
                                    .into_iter()
                                    .map(|(_, o)| object_row(&c, db.name(), schema, o)),
                            );
                        }
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
            NodeKey::Schema(_, _, s) | NodeKey::Group(_, _, s, _) => s,
            NodeKey::Object(o) => &o.schema,
            NodeKey::Connection(_) | NodeKey::Database(..) => return None,
        })
    }

    pub fn selected_database(&self) -> Option<&str> {
        self.selected()?.key.database()
    }

    pub fn selected_connection(&self) -> Option<&str> {
        self.selected().map(|r| r.key.connection())
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
    pub fn expand(&mut self, servers: &[Server]) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if matches!(row.kind, RowKind::Object(_)) {
            return;
        }
        if row.expanded {
            // A node still connecting / loading has no child yet.
            if self
                .rows
                .get(self.cursor + 1)
                .is_some_and(|r| r.depth > row.depth)
            {
                self.move_by(1);
            }
        } else if self.filter.is_empty() {
            self.expanded.insert(row.key);
            self.rebuild(servers);
        }
    }

    /// Collapses the node, or moves to its parent.
    pub fn collapse(&mut self, servers: &[Server]) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if row.expanded && self.filter.is_empty() && !matches!(row.kind, RowKind::Object(_)) {
            self.expanded.remove(&row.key);
            self.rebuild(servers);
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

    /// Toggles a folder (Enter on a connection, database, schema or group).
    pub fn toggle(&mut self, servers: &[Server]) {
        match self.selected().map(|r| r.expanded) {
            Some(true) => self.collapse(servers),
            Some(false) => self.expand(servers),
            None => {}
        }
    }

    /// Collapses everything below the connections, keeping the current
    /// connection and database open.
    pub fn collapse_all(&mut self, servers: &[Server]) {
        let conn = self.selected_connection().map(str::to_string);
        let db = self.selected_database().map(str::to_string);
        self.expanded
            .retain(|k| matches!(k, NodeKey::Connection(_)));
        let target = match (conn, db) {
            (Some(c), Some(d)) => {
                let key = NodeKey::Database(c, d);
                self.expanded.insert(key.clone());
                Some(key)
            }
            (Some(c), None) => Some(NodeKey::Connection(c)),
            _ => None,
        };
        self.rebuild(servers);
        if let Some(key) = target {
            self.select(&key);
        }
    }

    /// Expands the parents of `obj` and puts the cursor on it.
    pub fn reveal(&mut self, servers: &[Server], obj: &ObjectRef) {
        let (c, d, s) = (
            obj.connection.clone(),
            obj.database.clone(),
            obj.schema.clone(),
        );
        self.expanded.insert(NodeKey::Connection(c.clone()));
        self.expanded
            .insert(NodeKey::Database(c.clone(), d.clone()));
        self.expanded
            .insert(NodeKey::Schema(c.clone(), d.clone(), s.clone()));
        self.expanded
            .insert(NodeKey::Group(c, d, s, obj.kind.group()));
        self.rebuild(servers);
        self.select(&NodeKey::Object(obj.clone()));
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

fn connection_note(s: &Server) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match &s.state {
        ConnState::Disconnected => {}
        ConnState::Connecting(_) => {
            parts.push(s.progress.clone().unwrap_or_else(|| "connexion…".into()))
        }
        ConnState::Connected => {
            if let Some(i) = &s.info {
                parts.push(format!("PostgreSQL {}", i.version));
            }
        }
        ConnState::Failed(_) => parts.push("erreur".into()),
        ConnState::Lost(_) => parts.push("déconnecté".into()),
    }
    if s.conn.tunnel_cmd.is_some() {
        parts.push("tunnel".into());
    }
    if s.conn.read_only {
        parts.push("RO".into());
    }
    if s.conn.is_production() {
        parts.push("PROD".into());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
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

fn object_row(connection: &str, database: &str, schema: &Schema, o: &DbObject) -> TreeRow {
    TreeRow::node(
        NodeKey::Object(ObjectRef::new(connection, database, &schema.name, o)),
        RowKind::Object(o.kind),
        4,
        o.display_name(),
        false,
    )
}

/// Rows matching a fuzzy filter, across loaded catalogs: parents are shown
/// expanded, empty branches hidden. A filter containing `.` is matched
/// against `schema.object`.
fn build_filtered(servers: &[Server], query: &str) -> Vec<TreeRow> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let qualified = query.contains('.');
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut rows = Vec::new();

    for s in servers {
        let c = s.name();
        let mut conn_rows = Vec::new();
        for db in &s.databases {
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
                        let Some(score) = pattern.indices(haystack, &mut matcher, &mut indices)
                        else {
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
                        let mut row = object_row(c, db.name(), schema, o);
                        row.highlights = highlights;
                        row.score = score;
                        group_rows.push(row);
                    }
                    if group_rows.is_empty() {
                        continue;
                    }
                    // Best matches first; the sort is stable, so ties keep name order.
                    group_rows.sort_by_key(|r| std::cmp::Reverse(r.score));
                    let key =
                        NodeKey::Group(c.into(), db.name().into(), schema.name.clone(), group);
                    let mut g =
                        TreeRow::node(key, RowKind::Group(group), 3, group.label().into(), true);
                    g.count = Some(group_rows.len());
                    schema_rows.push(g);
                    schema_rows.append(&mut group_rows);
                }
                if schema_rows.is_empty() {
                    continue;
                }
                let key = NodeKey::Schema(c.into(), db.name().into(), schema.name.clone());
                db_rows.push(TreeRow::node(
                    key,
                    RowKind::Schema,
                    2,
                    schema.name.clone(),
                    true,
                ));
                db_rows.append(&mut schema_rows);
            }
            if db_rows.is_empty() {
                continue;
            }
            let key = NodeKey::Database(c.into(), db.name().into());
            conn_rows.push(TreeRow::node(
                key,
                RowKind::Database,
                1,
                db.name().into(),
                true,
            ));
            conn_rows.append(&mut db_rows);
        }
        if conn_rows.is_empty() {
            continue;
        }
        let key = NodeKey::Connection(c.into());
        rows.push(TreeRow::node(key, RowKind::Connection, 0, c.into(), true));
        rows.append(&mut conn_rows);
    }
    rows
}

#[cfg(test)]
mod tests;
