//! Tree state: `schema > group > object`, flattened into visible rows.
//!
//! Rows are rebuilt only when the catalog, the expansion set or the filter
//! change; rendering just slices `rows[offset..offset + viewport]`.

use std::collections::HashSet;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use super::input::LineInput;
use crate::db::catalog::{Catalog, ObjectGroup, ObjectKind, ObjectRef};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Schema(String),
    Group(String, ObjectGroup),
    Object(ObjectRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
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
    /// Character indices of `label` matched by the filter.
    pub highlights: Vec<usize>,
    /// Fuzzy match score when filtering (higher is better), 0 otherwise.
    pub score: u32,
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
    initialised: bool,
}

impl TreeState {
    /// Rebuilds rows from the catalog, keeping the cursor on the same node
    /// when it still exists.
    pub fn rebuild(&mut self, catalog: &Catalog) {
        self.rebuild_with(catalog, true);
    }

    /// To call when the filter text changed: the cursor jumps to the best
    /// match instead of staying on the object matched by a shorter query.
    pub fn filter_changed(&mut self, catalog: &Catalog) {
        // With an empty filter, staying on the last match keeps the context.
        let keep = self.filter.is_empty();
        self.rebuild_with(catalog, keep);
    }

    fn rebuild_with(&mut self, catalog: &Catalog, keep_selection: bool) {
        if !self.initialised {
            self.initialised = true;
            if let Some(first) = catalog.schemas.iter().find(|s| !s.is_system) {
                self.expanded.insert(NodeKey::Schema(first.name.clone()));
                self.expanded
                    .insert(NodeKey::Group(first.name.clone(), ObjectGroup::Tables));
            }
        }
        // While filtering, only an object selection is worth keeping: a
        // schema or group would hide the first match.
        let selected = self
            .selected()
            .filter(|_| keep_selection)
            .filter(|r| self.filter.is_empty() || matches!(r.kind, RowKind::Object(_)))
            .map(|r| r.key.clone());
        self.rows = if self.filter.is_empty() {
            self.build_rows(catalog)
        } else {
            build_filtered(catalog, self.filter.text())
        };
        self.cursor = selected
            .and_then(|k| self.rows.iter().position(|r| r.key == k))
            .unwrap_or_else(|| {
                // When filtering, land on the best match rather than a schema.
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

    fn build_rows(&self, catalog: &Catalog) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for schema in &catalog.schemas {
            let skey = NodeKey::Schema(schema.name.clone());
            let s_open = self.expanded.contains(&skey);
            rows.push(TreeRow {
                key: skey,
                kind: RowKind::Schema,
                depth: 0,
                label: schema.name.clone(),
                expanded: s_open,
                count: None,
                highlights: Vec::new(),
                score: 0,
            });
            if !s_open {
                continue;
            }
            for group in ObjectGroup::ALL {
                let objects: Vec<_> = schema.objects_in(group).collect();
                if objects.is_empty() {
                    continue;
                }
                let gkey = NodeKey::Group(schema.name.clone(), group);
                let g_open = self.expanded.contains(&gkey);
                rows.push(TreeRow {
                    key: gkey,
                    kind: RowKind::Group(group),
                    depth: 1,
                    label: group.label().to_string(),
                    expanded: g_open,
                    count: Some(objects.len()),
                    highlights: Vec::new(),
                    score: 0,
                });
                if g_open {
                    rows.extend(objects.into_iter().map(|(_, o)| TreeRow {
                        key: NodeKey::Object(ObjectRef::new(&schema.name, o)),
                        kind: RowKind::Object(o.kind),
                        depth: 2,
                        label: o.display_name(),
                        expanded: false,
                        count: None,
                        highlights: Vec::new(),
                        score: 0,
                    }));
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
            NodeKey::Schema(s) | NodeKey::Group(s, _) => s,
            NodeKey::Object(o) => &o.schema,
        })
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
    pub fn expand(&mut self, catalog: &Catalog) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if matches!(row.kind, RowKind::Object(_)) {
            return;
        }
        if row.expanded {
            self.move_by(1);
        } else if self.filter.is_empty() {
            self.expanded.insert(row.key);
            self.rebuild(catalog);
        }
    }

    /// Collapses the node, or moves to its parent.
    pub fn collapse(&mut self, catalog: &Catalog) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        if row.expanded && self.filter.is_empty() && !matches!(row.kind, RowKind::Object(_)) {
            self.expanded.remove(&row.key);
            self.rebuild(catalog);
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

    /// Toggles a schema / group (Enter on a folder).
    pub fn toggle(&mut self, catalog: &Catalog) {
        match self.selected().map(|r| r.expanded) {
            Some(true) => self.collapse(catalog),
            Some(false) => self.expand(catalog),
            None => {}
        }
    }

    pub fn collapse_all(&mut self, catalog: &Catalog) {
        let schema = self.selected_schema().map(str::to_string);
        self.expanded.clear();
        self.rebuild(catalog);
        if let Some(s) = schema {
            let key = NodeKey::Schema(s);
            if let Some(i) = self.rows.iter().position(|r| r.key == key) {
                self.cursor = i;
                self.scroll_into_view();
            }
        }
    }

    /// Expands the parents of `obj` and puts the cursor on it.
    pub fn reveal(&mut self, catalog: &Catalog, obj: &ObjectRef) {
        self.expanded.insert(NodeKey::Schema(obj.schema.clone()));
        self.expanded
            .insert(NodeKey::Group(obj.schema.clone(), obj.kind.group()));
        self.rebuild(catalog);
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

/// Rows matching a fuzzy filter: parents are shown expanded, empty branches hidden.
/// A filter containing `.` is matched against `schema.object`.
fn build_filtered(catalog: &Catalog, query: &str) -> Vec<TreeRow> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let qualified = query.contains('.');
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut rows = Vec::new();

    for schema in &catalog.schemas {
        let mut schema_rows = Vec::new();
        for group in ObjectGroup::ALL {
            let mut group_rows = Vec::new();
            for (_, o) in schema.objects_in(group) {
                let label = o.display_name();
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
                let highlights = if qualified {
                    let skip = schema.name.chars().count() as u32 + 1;
                    indices
                        .iter()
                        .filter(|i| **i >= skip)
                        .map(|i| (*i - skip) as usize)
                        .collect()
                } else {
                    indices.iter().map(|i| *i as usize).collect()
                };
                let mut highlights: Vec<usize> = highlights;
                highlights.sort_unstable();
                highlights.dedup();
                group_rows.push(TreeRow {
                    key: NodeKey::Object(ObjectRef::new(&schema.name, o)),
                    kind: RowKind::Object(o.kind),
                    depth: 2,
                    label,
                    expanded: false,
                    count: None,
                    highlights,
                    score,
                });
            }
            if group_rows.is_empty() {
                continue;
            }
            // Best matches first; the sort is stable, so ties keep name order.
            group_rows.sort_by_key(|r| std::cmp::Reverse(r.score));
            schema_rows.push(TreeRow {
                key: NodeKey::Group(schema.name.clone(), group),
                kind: RowKind::Group(group),
                depth: 1,
                label: group.label().to_string(),
                expanded: true,
                count: Some(group_rows.len()),
                highlights: Vec::new(),
                score: 0,
            });
            schema_rows.append(&mut group_rows);
        }
        if schema_rows.is_empty() {
            continue;
        }
        rows.push(TreeRow {
            key: NodeKey::Schema(schema.name.clone()),
            kind: RowKind::Schema,
            depth: 0,
            label: schema.name.clone(),
            expanded: true,
            count: None,
            highlights: Vec::new(),
            score: 0,
        });
        rows.append(&mut schema_rows);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::catalog::{DbObject, Schema};
    use pretty_assertions::assert_eq;

    fn obj(name: &str, kind: ObjectKind) -> DbObject {
        DbObject {
            name: name.into(),
            kind,
            comment: None,
            signature: (kind == ObjectKind::Function).then(|| "integer".to_string()),
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            schemas: vec![
                Schema {
                    name: "public".into(),
                    comment: None,
                    is_system: false,
                    objects: vec![
                        obj("customers", ObjectKind::Table),
                        obj("orders", ObjectKind::Table),
                        obj("order_totals", ObjectKind::View),
                        obj("next_id", ObjectKind::Function),
                    ],
                },
                Schema {
                    name: "hr".into(),
                    comment: None,
                    is_system: false,
                    objects: vec![obj("employees", ObjectKind::Table)],
                },
            ],
        }
    }

    fn labels(t: &TreeState) -> Vec<String> {
        t.rows
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth as usize), r.label))
            .collect()
    }

    #[test]
    fn initial_expansion() {
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.rebuild(&catalog());
        assert_eq!(
            labels(&t),
            vec![
                "public",
                "  Tables",
                "    customers",
                "    orders",
                "  Views",
                "  Functions",
                "hr"
            ]
        );
    }

    #[test]
    fn expand_collapse_and_parent() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.rebuild(&cat);
        t.bottom(); // hr
        t.expand(&cat);
        assert_eq!(t.rows.len(), 8);
        t.expand(&cat); // already open -> first child
        assert_eq!(t.selected().unwrap().label, "Tables");
        t.collapse(&cat); // closed group -> parent
        assert_eq!(t.selected().unwrap().label, "hr");
        t.collapse(&cat);
        assert_eq!(t.rows.len(), 7);
    }

    #[test]
    fn cursor_follows_node_across_rebuilds() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.rebuild(&cat);
        t.move_by(3); // orders
        assert_eq!(t.selected().unwrap().label, "orders");
        t.rebuild(&cat);
        assert_eq!(t.selected().unwrap().label, "orders");
    }

    #[test]
    fn filter_shows_matches_with_parents() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.filter = LineInput::with_text("ord");
        t.rebuild(&cat);
        assert_eq!(
            labels(&t),
            vec![
                "public",
                "  Tables",
                "    orders",
                "  Views",
                "    order_totals"
            ]
        );
        assert_eq!(t.selected().unwrap().label, "orders");
        assert_eq!(t.rows[2].highlights, vec![0, 1, 2]);
    }

    #[test]
    fn filter_ranks_best_match_first() {
        let mut cat = catalog();
        // `order_items` sorts before `orders` alphabetically but matches worse.
        cat.schemas[0]
            .objects
            .insert(1, obj("order_items", ObjectKind::Table));
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.filter = LineInput::with_text("orders");
        t.rebuild(&cat);
        assert_eq!(t.rows[2].label, "orders");
        assert_eq!(t.rows[3].label, "order_items");
        assert_eq!(t.selected().unwrap().label, "orders");
    }

    #[test]
    fn typing_the_filter_follows_the_best_match() {
        let mut cat = catalog();
        cat.schemas[0]
            .objects
            .insert(0, obj("order_items", ObjectKind::Table));
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.rebuild(&cat);
        // As typed in the UI: one character at a time.
        for c in "orders".chars() {
            let mut text = t.filter.text().to_string();
            text.push(c);
            t.filter = LineInput::with_text(text);
            t.filter_changed(&cat);
        }
        assert_eq!(t.selected().unwrap().label, "orders");
        // Clearing the filter keeps the cursor on the object.
        t.filter.clear();
        t.filter_changed(&cat);
        assert_eq!(t.selected().unwrap().label, "orders");
    }

    #[test]
    fn qualified_filter() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.filter = LineInput::with_text("hr.emp");
        t.rebuild(&cat);
        assert_eq!(labels(&t), vec!["hr", "  Tables", "    employees"]);
    }

    #[test]
    fn reveal_object() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 10,
            ..Default::default()
        };
        t.rebuild(&cat);
        let target = ObjectRef::new("hr", &cat.schemas[1].objects[0]);
        t.reveal(&cat, &target);
        assert_eq!(t.selected_object(), Some(&target));
    }

    #[test]
    fn scrolling_keeps_cursor_visible() {
        let cat = catalog();
        let mut t = TreeState {
            viewport: 3,
            ..Default::default()
        };
        t.rebuild(&cat);
        t.bottom();
        assert_eq!(t.offset, t.rows.len() - 3);
        t.top();
        assert_eq!(t.offset, 0);
    }
}
