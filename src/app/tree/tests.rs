use super::*;
use crate::config::ConnectionConfig;
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

fn catalog(connection: &str, database: &str) -> Catalog {
    Catalog {
        connection: connection.into(),
        database: database.into(),
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

/// `srv` (connected: `demo` loaded, `other` not) and `prod` (not connected).
fn servers() -> Vec<Server> {
    let mut srv = Server::new(ConnectionConfig::named("srv"), None, true);
    srv.state = ConnState::Connected;
    let mut demo = DatabaseEntry::new(info("demo"));
    demo.catalog = Some(catalog("srv", "demo"));
    srv.databases = vec![demo, DatabaseEntry::new(info("other"))];
    let mut prod = ConnectionConfig::named("prod");
    prod.color = Some("red".into());
    vec![srv, Server::new(prod, None, true)]
}

fn tree(viewport: usize, servers: &[Server]) -> TreeState {
    let mut t = TreeState {
        viewport,
        ..Default::default()
    };
    t.expand_node(NodeKey::Connection("srv".into()));
    t.expand_node(NodeKey::Database("srv".into(), "demo".into()));
    t.rebuild(servers);
    t
}

fn labels(t: &TreeState) -> Vec<String> {
    t.rows
        .iter()
        .map(|r| format!("{}{}", "  ".repeat(r.depth as usize), r.label))
        .collect()
}

#[test]
fn connections_at_the_root() {
    let t = tree(30, &servers());
    assert_eq!(
        labels(&t),
        vec![
            "srv",
            "  demo",
            "    public",
            "      Tables",
            "        customers",
            "        orders",
            "      Views",
            "      Functions",
            "    hr",
            "  other",
            "prod"
        ]
    );
    assert_eq!(t.rows[1].note.as_deref(), Some("20 MB"));
    assert_eq!(t.rows.last().unwrap().note.as_deref(), Some("PROD"));
}

#[test]
fn a_disconnected_connection_has_no_children() {
    let mut s = servers();
    let mut t = tree(30, &s);
    t.bottom(); // prod
    t.expand(&s);
    assert!(t.is_expanded(&NodeKey::Connection("prod".into())));
    assert_eq!(
        t.rows.last().unwrap().label,
        "prod",
        "nothing below until connected"
    );
    s[1].state = ConnState::Connecting(std::time::Instant::now());
    s[1].progress = Some("Ouverture du tunnel…".into());
    t.rebuild(&s);
    assert_eq!(
        t.rows.last().unwrap().note.as_deref(),
        Some("Ouverture du tunnel… · PROD")
    );
    s[1].state = ConnState::Connected;
    s[1].databases = vec![DatabaseEntry::new(info("shop"))];
    t.rebuild(&s);
    assert_eq!(t.rows.last().unwrap().label, "shop");
}

#[test]
fn expanding_an_unloaded_database() {
    let mut s = servers();
    let mut t = tree(30, &s);
    t.select(&NodeKey::Database("srv".into(), "other".into()));
    t.expand(&s);
    assert!(t.is_expanded(&NodeKey::Database("srv".into(), "other".into())));
    s[0].databases[1].loading = true;
    t.rebuild(&s);
    t.expand(&s);
    assert_eq!(
        t.selected().unwrap().label,
        "other",
        "no child while loading"
    );
    s[0].databases[1].catalog = Some(catalog("srv", "other"));
    s[0].databases[1].loading = false;
    t.rebuild(&s);
    let other = t.rows.iter().position(|r| r.label == "other").unwrap();
    assert_eq!(
        t.rows[other + 1].label,
        "public",
        "first schema opened once loaded"
    );
    assert_eq!(
        t.rows[other + 3].key,
        NodeKey::Object(ObjectRef::new(
            "srv",
            "other",
            "public",
            &obj("customers", ObjectKind::Table)
        ))
    );
}

#[test]
fn expand_collapse_and_parent() {
    let s = servers();
    let mut t = tree(30, &s);
    t.select(&NodeKey::Schema("srv".into(), "demo".into(), "hr".into()));
    t.expand(&s);
    assert_eq!(t.rows.len(), 12, "hr gains its Tables group");
    t.expand(&s); // already open -> first child
    assert_eq!(t.selected().unwrap().label, "Tables");
    t.collapse(&s); // closed group -> parent
    assert_eq!(t.selected().unwrap().label, "hr");
    t.collapse(&s);
    assert_eq!(t.rows.len(), 11);
    t.collapse(&s); // closed schema -> database
    assert_eq!(t.selected().unwrap().label, "demo");
    t.collapse(&s); // collapse the database
    t.collapse(&s); // -> connection
    assert_eq!(t.selected().unwrap().label, "srv");
    assert_eq!(t.selected_connection(), Some("srv"));
    assert_eq!(t.selected_database(), None);
}

#[test]
fn cursor_follows_node_across_rebuilds() {
    let s = servers();
    let mut t = tree(30, &s);
    t.move_by(5); // orders
    assert_eq!(t.selected().unwrap().label, "orders");
    assert_eq!(t.selected_database(), Some("demo"));
    assert_eq!(t.selected_schema(), Some("public"));
    t.rebuild(&s);
    assert_eq!(t.selected().unwrap().label, "orders");
}

#[test]
fn filter_spans_connections() {
    let mut s = servers();
    s[1].state = ConnState::Connected;
    let mut db = DatabaseEntry::new(info("shop"));
    db.catalog = Some(catalog("prod", "shop"));
    s[1].databases = vec![db];
    let mut t = tree(30, &s);
    t.filter = LineInput::with_text("ord");
    t.filter_changed(&s);
    assert_eq!(
        labels(&t),
        vec![
            "srv",
            "  demo",
            "    public",
            "      Tables",
            "        orders",
            "      Views",
            "        order_totals",
            "prod",
            "  shop",
            "    public",
            "      Tables",
            "        orders",
            "      Views",
            "        order_totals"
        ]
    );
    assert_eq!(t.selected().unwrap().label, "orders");
    assert_eq!(t.rows[4].highlights, vec![0, 1, 2]);
}

#[test]
fn typing_the_filter_follows_the_best_match() {
    let mut s = servers();
    s[0].databases[0].catalog.as_mut().unwrap().schemas[0]
        .objects
        .insert(0, obj("order_items", ObjectKind::Table));
    let mut t = tree(30, &s);
    for c in "orders".chars() {
        let mut text = t.filter.text().to_string();
        text.push(c);
        t.filter = LineInput::with_text(text);
        t.filter_changed(&s);
    }
    assert_eq!(t.selected().unwrap().label, "orders");
    t.filter.clear();
    t.filter_changed(&s);
    assert_eq!(t.selected().unwrap().label, "orders", "kept after clearing");
}

#[test]
fn qualified_filter() {
    let s = servers();
    let mut t = tree(30, &s);
    t.filter = LineInput::with_text("hr.emp");
    t.filter_changed(&s);
    assert_eq!(
        labels(&t),
        vec![
            "srv",
            "  demo",
            "    hr",
            "      Tables",
            "        employees"
        ]
    );
}

#[test]
fn reveal_object() {
    let s = servers();
    let mut t = tree(30, &s);
    let target = ObjectRef::new("srv", "demo", "hr", &obj("employees", ObjectKind::Table));
    t.reveal(&s, &target);
    assert_eq!(t.selected_object(), Some(&target));
}

#[test]
fn forget_connection_after_disconnect() {
    let mut s = servers();
    let mut t = tree(30, &s);
    t.forget_connection("srv");
    s[0].state = ConnState::Disconnected;
    s[0].databases.clear();
    t.rebuild(&s);
    assert!(
        t.is_expanded(&NodeKey::Connection("srv".into())),
        "the node itself stays"
    );
    assert!(!t.is_expanded(&NodeKey::Database("srv".into(), "demo".into())));
    assert_eq!(labels(&t), vec!["srv", "prod"]);
}

#[test]
fn collapse_all_keeps_connection_and_database() {
    let s = servers();
    let mut t = tree(30, &s);
    t.move_by(5);
    t.collapse_all(&s);
    assert_eq!(
        labels(&t),
        vec!["srv", "  demo", "    public", "    hr", "  other", "prod"]
    );
    assert_eq!(t.selected().unwrap().label, "demo");
}

#[test]
fn scrolling_keeps_cursor_visible() {
    let s = servers();
    let mut t = tree(3, &s);
    t.bottom();
    assert_eq!(t.offset, t.rows.len() - 3);
    t.top();
    assert_eq!(t.offset, 0);
}
