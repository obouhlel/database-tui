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

fn info(name: &str) -> DatabaseInfo {
    DatabaseInfo {
        name: name.into(),
        comment: None,
        size_bytes: Some(20 * 1024 * 1024),
        can_connect: true,
    }
}

fn catalog() -> Catalog {
    Catalog {
        database: "demo".into(),
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

/// `demo` (loaded) and `other` (not loaded yet).
fn dbs() -> Vec<DatabaseEntry> {
    let mut demo = DatabaseEntry::new(info("demo"));
    demo.catalog = Some(catalog());
    vec![demo, DatabaseEntry::new(info("other"))]
}

fn tree(viewport: usize, dbs: &[DatabaseEntry]) -> TreeState {
    let mut t = TreeState {
        viewport,
        ..Default::default()
    };
    t.expand_database("demo");
    t.rebuild(dbs);
    t
}

fn labels(t: &TreeState) -> Vec<String> {
    t.rows
        .iter()
        .map(|r| format!("{}{}", "  ".repeat(r.depth as usize), r.label))
        .collect()
}

#[test]
fn databases_at_the_root_and_initial_expansion() {
    let t = tree(20, &dbs());
    assert_eq!(
        labels(&t),
        vec![
            "demo",
            "  public",
            "    Tables",
            "      customers",
            "      orders",
            "    Views",
            "    Functions",
            "  hr",
            "other"
        ]
    );
    assert_eq!(t.rows[0].note.as_deref(), Some("20 MB"));
    assert!(!t.is_expanded(&NodeKey::Database("other".into())));
}

#[test]
fn expanding_an_unloaded_database() {
    let mut dbs = dbs();
    let mut t = tree(20, &dbs);
    t.bottom(); // other
    t.expand(&dbs);
    assert!(t.is_expanded(&NodeKey::Database("other".into())));
    // Still no child while loading: `l` again stays on the node.
    dbs[1].loading = true;
    t.rebuild(&dbs);
    assert_eq!(t.rows.last().unwrap().note.as_deref(), Some("chargement…"));
    t.expand(&dbs);
    assert_eq!(t.selected().unwrap().label, "other");
    // Loaded: its first schema opens automatically.
    let mut cat = catalog();
    cat.database = "other".into();
    dbs[1].catalog = Some(cat);
    dbs[1].loading = false;
    t.rebuild(&dbs);
    let other = t.rows.iter().position(|r| r.label == "other").unwrap();
    assert_eq!(t.rows[other + 1].label, "public");
    assert_eq!(
        t.rows[other + 3].key,
        NodeKey::Object(ObjectRef::new(
            "other",
            "public",
            &catalog().schemas[0].objects[0]
        ))
    );
}

#[test]
fn expand_collapse_and_parent() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    let hr = t.rows.iter().position(|r| r.label == "hr").unwrap();
    t.cursor = hr;
    t.expand(&dbs);
    assert_eq!(t.rows.len(), 10, "hr gains its Tables group");
    t.expand(&dbs); // already open -> first child
    assert_eq!(t.selected().unwrap().label, "Tables");
    t.collapse(&dbs); // closed group -> parent
    assert_eq!(t.selected().unwrap().label, "hr");
    t.collapse(&dbs);
    assert_eq!(t.rows.len(), 9);
    t.collapse(&dbs); // closed schema -> database
    assert_eq!(t.selected().unwrap().label, "demo");
}

#[test]
fn cursor_follows_node_across_rebuilds() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    t.move_by(4); // orders
    assert_eq!(t.selected().unwrap().label, "orders");
    assert_eq!(t.selected_database(), Some("demo"));
    t.rebuild(&dbs);
    assert_eq!(t.selected().unwrap().label, "orders");
}

#[test]
fn filter_shows_matches_with_parents() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    t.filter = LineInput::with_text("ord");
    t.filter_changed(&dbs);
    assert_eq!(
        labels(&t),
        vec![
            "demo",
            "  public",
            "    Tables",
            "      orders",
            "    Views",
            "      order_totals"
        ]
    );
    assert_eq!(t.selected().unwrap().label, "orders");
    assert_eq!(t.rows[3].highlights, vec![0, 1, 2]);
}

#[test]
fn filter_ranks_best_match_first() {
    let mut dbs = dbs();
    // `order_items` sorts before `orders` alphabetically but matches worse.
    dbs[0].catalog.as_mut().unwrap().schemas[0]
        .objects
        .insert(1, obj("order_items", ObjectKind::Table));
    let mut t = tree(20, &dbs);
    t.filter = LineInput::with_text("orders");
    t.filter_changed(&dbs);
    assert_eq!(t.rows[3].label, "orders");
    assert_eq!(t.rows[4].label, "order_items");
    assert_eq!(t.selected().unwrap().label, "orders");
}

#[test]
fn typing_the_filter_follows_the_best_match() {
    let mut dbs = dbs();
    dbs[0].catalog.as_mut().unwrap().schemas[0]
        .objects
        .insert(0, obj("order_items", ObjectKind::Table));
    let mut t = tree(20, &dbs);
    // As typed in the UI: one character at a time.
    for c in "orders".chars() {
        let mut text = t.filter.text().to_string();
        text.push(c);
        t.filter = LineInput::with_text(text);
        t.filter_changed(&dbs);
    }
    assert_eq!(t.selected().unwrap().label, "orders");
    // Clearing the filter keeps the cursor on the object.
    t.filter.clear();
    t.filter_changed(&dbs);
    assert_eq!(t.selected().unwrap().label, "orders");
}

#[test]
fn qualified_filter() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    t.filter = LineInput::with_text("hr.emp");
    t.filter_changed(&dbs);
    assert_eq!(
        labels(&t),
        vec!["demo", "  hr", "    Tables", "      employees"]
    );
}

#[test]
fn reveal_object() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    let target = ObjectRef::new("demo", "hr", &catalog().schemas[1].objects[0]);
    t.reveal(&dbs, &target);
    assert_eq!(t.selected_object(), Some(&target));
}

#[test]
fn inaccessible_database_note() {
    let mut dbs = dbs();
    dbs[1].info.can_connect = false;
    let t = tree(20, &dbs);
    assert_eq!(t.rows.last().unwrap().note.as_deref(), Some("accès refusé"));
}

#[test]
fn collapse_all_keeps_the_current_database_open() {
    let dbs = dbs();
    let mut t = tree(20, &dbs);
    t.move_by(4);
    t.collapse_all(&dbs);
    assert_eq!(labels(&t), vec!["demo", "  public", "  hr", "other"]);
    assert_eq!(t.selected().unwrap().label, "demo");
}

#[test]
fn scrolling_keeps_cursor_visible() {
    let dbs = dbs();
    let mut t = tree(3, &dbs);
    t.bottom();
    assert_eq!(t.offset, t.rows.len() - 3);
    t.top();
    assert_eq!(t.offset, 0);
}
