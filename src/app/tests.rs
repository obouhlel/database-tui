use super::*;
use crate::config::ConnectionConfig;
use crate::db::catalog::{Catalog, DatabaseInfo, DbObject, ObjectKind, Schema};
use crate::db::{BackendKind, ServerInfo};
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

fn conn(name: &str) -> ConnectionConfig {
    ConnectionConfig::named(name)
}

fn app_with(names: &[&str], default: Option<&str>) -> App {
    let mut config = Config {
        connections: names.iter().map(|n| conn(n)).collect(),
        ..Default::default()
    };
    config.general.default_connection = default.map(str::to_string);
    App::new(
        config,
        Keymap::default(),
        Theme::dark(),
        Vec::new(),
        Vec::new(),
        Start::Picker,
    )
}

/// DB commands among effects.
fn db(effects: Vec<Effect>) -> Vec<DbCommand> {
    effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::Db(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn key(app: &mut App, s: &str) -> Vec<DbCommand> {
    db(key_effects(app, s))
}

fn key_effects(app: &mut App, s: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for chord in KeyChord::parse_seq(s).unwrap() {
        effects.extend(app.update(Msg::Key(KeyEvent::new(chord.code, chord.mods))));
    }
    effects
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )));
    }
}

fn info() -> ServerInfo {
    ServerInfo {
        kind: BackendKind::Postgres,
        version: "17.0".into(),
        version_num: 170000,
        user: "u".into(),
        database: "demo".into(),
        tls: false,
    }
}

fn catalog_for(connection: &str) -> Catalog {
    let table = |n: &str| DbObject {
        name: n.into(),
        kind: ObjectKind::Table,
        comment: None,
        signature: None,
    };
    Catalog {
        connection: connection.into(),
        database: "demo".into(),
        schemas: vec![Schema {
            name: "public".into(),
            comment: None,
            is_system: false,
            objects: vec![table("customers"), table("orders")],
        }],
    }
}

fn catalog() -> Catalog {
    catalog_for("local")
}

fn connected(connection: &str) -> Msg {
    Msg::Db(DbEvent::Connected {
        connection: connection.into(),
        info: info(),
        databases: vec![],
        elapsed: Duration::from_millis(5),
    })
}

/// Tree -> Enter on the connection -> connected -> catalog loaded.
fn connected_app() -> App {
    let mut app = app_with(&["local"], None);
    let cmds = key(&mut app, "enter");
    assert!(
        matches!(cmds.as_slice(), [DbCommand::Connect { request, resolve_password: true }] if request.conn.name == "local")
    );
    let cmds = db(app.update(connected("local")));
    assert!(matches!(
        cmds.as_slice(),
        [DbCommand::LoadCatalog { connection, database, include_system: false }] if connection == "local" && database == "demo"
    ));
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: catalog(),
        elapsed: Duration::from_millis(3),
    }));
    app
}

#[test]
fn starts_on_the_tree_without_connecting() {
    let mut app = app_with(&["a", "b", "c"], Some("b"));
    assert_eq!(app.screen, Screen::Main);
    assert_eq!(
        app.tree.selected().unwrap().label,
        "b",
        "default connection selected"
    );
    let effects = db(app.init());
    assert!(
        effects
            .iter()
            .all(|c| matches!(c, DbCommand::Discover { .. })),
        "{effects:?}"
    );
}

#[test]
fn enter_on_a_connection_connects_it() {
    let mut app = app_with(&["a", "b"], None);
    key(&mut app, "j");
    let cmds = key(&mut app, "enter");
    match cmds.as_slice() {
        [
            DbCommand::Connect {
                request,
                resolve_password: true,
            },
        ] => assert_eq!(request.conn.name, "b"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        app.server("b").unwrap().state,
        server::ConnState::Connecting(_)
    ));
    assert_eq!(app.tree.rows[1].note.as_deref(), Some("connexion…"));
    // A second Enter while connecting does not connect again.
    assert!(key(&mut app, "enter").is_empty());
}

#[test]
fn connect_then_catalog() {
    let app = connected_app();
    assert!(app.server("local").unwrap().is_connected());
    // local, demo, public, Tables, customers, orders
    assert_eq!(app.tree.rows.len(), 6);
}

#[test]
fn two_connections_open_at_once() {
    let mut app = app_with(&["a", "b"], None);
    key(&mut app, "enter");
    app.update(connected("a"));
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: catalog_for("a"),
        elapsed: Duration::ZERO,
    }));
    key(&mut app, "G");
    assert_eq!(app.tree.selected().unwrap().label, "b");
    let cmds = key(&mut app, "enter");
    assert!(
        matches!(cmds.as_slice(), [DbCommand::Connect { request, .. }] if request.conn.name == "b")
    );
    app.update(connected("b"));
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: catalog_for("b"),
        elapsed: Duration::ZERO,
    }));
    assert!(app.server("a").unwrap().is_connected() && app.server("b").unwrap().is_connected());
    let roots: Vec<_> = app
        .tree
        .rows
        .iter()
        .filter(|r| r.depth == 0)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(roots, ["a", "b"]);
    assert_eq!(
        app.tree.rows.iter().filter(|r| r.label == "orders").count(),
        2
    );
}

#[test]
fn password_prompt_flow() {
    let mut app = app_with(&["local"], None);
    key(&mut app, "enter");
    app.update(Msg::Db(DbEvent::ConnectFailed {
        connection: "local".into(),
        error: "password missing".into(),
        password_required: true,
    }));
    assert!(matches!(app.overlay, Some(Overlay::Password { .. })));
    typed(&mut app, "s3cret");
    let cmds = key(&mut app, "enter");
    match cmds.as_slice() {
        [
            DbCommand::Connect {
                request,
                resolve_password: false,
            },
        ] => {
            assert_eq!(request.password.as_deref(), Some("s3cret"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(app.overlay.is_none());
}

#[test]
fn connect_error_shows_popup_and_enter_retries() {
    let mut app = app_with(&["local"], None);
    key(&mut app, "enter");
    app.update(Msg::Db(DbEvent::ConnectFailed {
        connection: "local".into(),
        error: "refused".into(),
        password_required: false,
    }));
    assert!(matches!(app.overlay, Some(Overlay::Error { .. })));
    assert_eq!(app.tree.rows[0].note.as_deref(), Some("erreur"));
    key(&mut app, "esc");
    assert!(app.overlay.is_none());
    assert!(matches!(
        key(&mut app, "enter").as_slice(),
        [DbCommand::Connect { .. }]
    ));
}

#[test]
fn disconnect_and_delete() {
    let mut app = connected_app();
    key(&mut app, "G enter"); // open orders
    key(&mut app, "ctrl+h gg");
    let cmds = key(&mut app, "X");
    assert!(
        matches!(cmds.as_slice(), [DbCommand::Disconnect { connection }] if connection == "local")
    );
    assert!(
        app.current.is_none(),
        "objects of the closed connection are closed"
    );
    assert_eq!(app.tree.rows.len(), 1);

    let effects = key_effects(&mut app, "d d");
    assert!(effects.is_empty());
    assert!(matches!(app.overlay, Some(Overlay::ConfirmDelete(ref n)) if n == "local"));
    let effects = key_effects(&mut app, "o");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteConnection(n) if n == "local")),
        "{effects:?}"
    );
    assert!(app.servers.is_empty() && app.tree.rows.is_empty());
}

#[test]
fn connections_page_opens_with_ctrl_n() {
    let mut app = app_with(&["a"], None);
    key(&mut app, "ctrl+n");
    assert_eq!(app.screen, Screen::Picker);
    key(&mut app, "esc");
    assert_eq!(app.screen, Screen::Main);
    key(&mut app, "ctrl+n");
    let cmds = key(&mut app, "enter");
    assert_eq!(app.screen, Screen::Main);
    assert!(
        matches!(cmds.as_slice(), [DbCommand::Connect { request, .. }] if request.conn.name == "a")
    );
}

#[test]
fn open_object_focuses_center() {
    let mut app = connected_app();
    key(&mut app, "G");
    key(&mut app, "enter");
    assert_eq!(
        app.current.as_ref().map(|o| o.name.as_str()),
        Some("orders")
    );
    assert_eq!(
        app.current.as_ref().map(|o| o.connection.as_str()),
        Some("local")
    );
    assert_eq!(app.focus, Focus::Center);
    key(&mut app, "ctrl+h");
    assert_eq!(app.focus, Focus::Tree);
}

#[test]
fn tree_filter() {
    let mut app = connected_app();
    key(&mut app, "/");
    assert_eq!(app.mode(), Mode::Filter);
    typed(&mut app, "cus");
    assert_eq!(app.tree.rows.len(), 5); // local, demo, public, Tables, customers
    assert_eq!(app.tree.selected().unwrap().label, "customers");
    // `q` is text while filtering, not quit.
    typed(&mut app, "q");
    assert!(!app.should_quit);
    key(&mut app, "esc");
    assert_eq!(app.mode(), Mode::Normal);
    assert_eq!(app.tree.rows.len(), 6);
}

#[test]
fn tabs() {
    let mut app = connected_app();
    key(&mut app, "2");
    assert_eq!(app.tab, Tab::Data);
    key(&mut app, "tab");
    assert_eq!(app.tab, Tab::Sql);
    // In the SQL editor Tab indents: Alt+1/2/3 switch tabs.
    key(&mut app, "tab");
    assert_eq!(app.tab, Tab::Sql);
    key(&mut app, "alt+1");
    assert_eq!(app.tab, Tab::Schema);
    key(&mut app, "shift+tab");
    assert_eq!(app.tab, Tab::Sql);
}

#[test]
fn command_line() {
    let mut app = connected_app();
    key(&mut app, ":");
    assert_eq!(app.mode(), Mode::Command);
    typed(&mut app, "nope");
    key(&mut app, "enter");
    assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Error));
    key(&mut app, ":");
    typed(&mut app, "reload");
    let cmds = key(&mut app, "enter");
    assert!(matches!(
        cmds.as_slice(),
        [DbCommand::ListDatabases { connection }, DbCommand::LoadCatalog { .. }] if connection == "local"
    ));
    key(&mut app, "ctrl+p");
    typed(&mut app, "q");
    key(&mut app, "enter");
    assert!(app.should_quit);
}

#[test]
fn palette_runs_a_chosen_action() {
    let mut app = connected_app();
    key(&mut app, "ctrl+p");
    typed(&mut app, "help");
    let m = app.palette_matches("help");
    assert_eq!(m[0].label, "help");
    key(&mut app, "down");
    key(&mut app, "enter");
    assert!(matches!(app.overlay, Some(Overlay::Help { .. })));
    key(&mut app, "esc");
    // Tab completes a command that takes an argument.
    key(&mut app, ":");
    typed(&mut app, "conne");
    key(&mut app, "down tab");
    match &app.overlay {
        Some(Overlay::Command(i)) => assert!(
            i.text() == "connect " || i.text() == "connections",
            "{}",
            i.text()
        ),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ctrl_c_does_not_quit() {
    let mut app = connected_app();
    key(&mut app, "ctrl+c");
    assert!(!app.should_quit);
    assert!(app.status.is_some());
}

#[test]
fn connection_lost_then_reconnect() {
    let mut app = connected_app();
    app.update(Msg::Db(DbEvent::ConnectionLost {
        connection: "local".into(),
        message: "eof".into(),
    }));
    assert!(matches!(
        app.server("local").unwrap().state,
        server::ConnState::Lost(_)
    ));
    assert!(
        key(&mut app, "R").is_empty(),
        "nothing to reload on a lost connection"
    );
    key(&mut app, "gg");
    assert!(matches!(
        key(&mut app, "enter").as_slice(),
        [DbCommand::Connect { .. }]
    ));
}

fn db_info(name: &str, size: i64) -> DatabaseInfo {
    DatabaseInfo {
        name: name.into(),
        comment: None,
        size_bytes: Some(size),
        can_connect: true,
    }
}

fn database_names(app: &App) -> Vec<String> {
    app.server("local")
        .unwrap()
        .databases
        .iter()
        .map(|d| d.name().to_string())
        .collect()
}

/// `R` re-reads the databases: new ones appear, dropped ones go, the
/// others keep their catalog and expansion.
#[test]
fn reload_refreshes_the_list_of_databases() {
    let mut app = connected_app();
    app.update(Msg::Db(DbEvent::DatabasesListed {
        connection: "local".into(),
        databases: vec![db_info("demo", 10), db_info("old", 1)],
    }));
    assert_eq!(database_names(&app), ["demo", "old"]);
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: catalog_for("old"),
        elapsed: Duration::from_millis(3),
    }));
    // Open a table of `old`, then the server drops it and creates `mnemo`.
    app.current = Some(ObjectRef {
        connection: "local".into(),
        database: "old".into(),
        schema: "public".into(),
        name: "orders".into(),
        kind: ObjectKind::Table,
        signature: None,
    });
    let cmds = key(&mut app, "R");
    assert!(matches!(
        cmds.first(),
        Some(DbCommand::ListDatabases { .. })
    ));
    app.update(Msg::Db(DbEvent::DatabasesListed {
        connection: "local".into(),
        databases: vec![db_info("mnemo", 5), db_info("demo", 20)],
    }));
    assert_eq!(
        database_names(&app),
        ["demo", "mnemo"],
        "connected one first"
    );
    let s = app.server("local").unwrap();
    let demo = s.database("demo").unwrap();
    assert!(demo.catalog.is_some(), "catalog kept");
    assert_eq!(demo.info.size_bytes, Some(20), "size refreshed");
    assert!(app.current.is_none(), "view of the dropped database closed");
    assert!(app.tree.rows.iter().any(|r| r.label == "mnemo"));
    assert!(!app.tree.rows.iter().any(|r| r.label == "old"));
    assert!(
        app.tree.rows.iter().any(|r| r.label == "customers"),
        "demo still expanded"
    );
    let msg = &app.status.as_ref().unwrap().text;
    assert!(msg.contains("mnemo") && msg.contains("old"), "{msg}");
    // The connected database stays even if the list misses it.
    app.update(Msg::Db(DbEvent::DatabasesListed {
        connection: "local".into(),
        databases: vec![],
    }));
    assert_eq!(database_names(&app), ["demo"]);
}

#[test]
fn reconnect_reloads_catalogs() {
    let mut app = connected_app();
    key(&mut app, ":");
    typed(&mut app, "reconnect");
    let cmds = key(&mut app, "enter");
    assert!(matches!(cmds.as_slice(), [DbCommand::Connect { .. }]));
    let cmds = db(app.update(connected("local")));
    assert!(matches!(cmds.as_slice(), [DbCommand::LoadCatalog { .. }]));
}

// ------------------------------------------------------------ Data tab

mod data_tab {
    use super::*;
    use crate::db::data::{Op, RowCount, TextRows};
    use crate::db::schema::{Column, ObjectDetails};
    use pretty_assertions::assert_eq;

    fn column(name: &str, ty: &str, pk: bool) -> Column {
        Column {
            name: name.into(),
            data_type: ty.into(),
            nullable: !pk,
            default: None,
            identity: None,
            generated: None,
            collation: None,
            comment: None,
            primary_key: pk,
            references: None,
        }
    }

    fn fetches(effects: &[Effect]) -> Vec<(u64, u64, u64)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Db(DbCommand::FetchRows { id, request }) => {
                    Some((*id, request.offset, request.limit))
                }
                _ => None,
            })
            .collect()
    }

    fn count_id(effects: &[Effect]) -> Option<u64> {
        effects.iter().find_map(|e| match e {
            Effect::Db(DbCommand::CountRows { id, .. }) => Some(*id),
            _ => None,
        })
    }

    fn rows(offset: u64, n: u64) -> TextRows {
        TextRows {
            columns: vec!["id".into(), "status".into(), "xmin".into()],
            rows: (offset..offset + n)
                .map(|i| {
                    vec![
                        Some(i.to_string()),
                        (i % 3 != 0).then(|| format!("s{i}")),
                        Some("1".into()),
                    ]
                })
                .collect(),
        }
    }

    /// Opens `orders` on the Data tab and answers the first fetch with
    /// `total` rows in pages of 200.
    fn data_app(total: u64) -> App {
        let mut app = connected_app();
        key(&mut app, "2");
        let effects = key_effects(&mut app, "G enter");
        let obj = match db(effects).as_slice() {
            [DbCommand::LoadDetails(o)] => o.clone(),
            other => panic!("expected details request, got {other:?}"),
        };
        let effects = app.update(Msg::Db(DbEvent::DetailsLoaded {
            details: Box::new(ObjectDetails {
                object: obj,
                owner: None,
                comment: None,
                columns: vec![
                    column("id", "bigint", true),
                    column("status", "text", false),
                ],
                indexes: vec![],
                constraints: vec![],
                triggers: vec![],
                foreign_keys: vec![],
                referenced_by: vec![],
                stats: None,
                properties: vec![],
                ddl: String::new(),
            }),
            elapsed: Duration::ZERO,
        }));
        let f = fetches(&effects);
        assert_eq!(f.len(), 1, "{effects:?}");
        let (id, offset, limit) = f[0];
        assert_eq!(offset, 0);
        let cid = count_id(&effects).expect("count requested");
        app.update(Msg::Db(DbEvent::RowsLoaded {
            id,
            offset,
            rows: rows(0, limit.min(total)),
            elapsed: Duration::ZERO,
        }));
        app.update(Msg::Db(DbEvent::RowsCounted {
            id: cid,
            count: RowCount::Exact(total),
        }));
        app.data.grid.viewport_rows = 20;
        app
    }

    #[test]
    fn opening_loads_first_page_and_count() {
        let app = data_app(1000);
        assert_eq!(app.data.rows.len(), 200);
        assert_eq!(app.data_position().as_deref(), Some("ligne 1 / 1 000"));
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn scrolling_near_the_end_loads_next_page() {
        let mut app = data_app(1000);
        let mut effects = Vec::new();
        // 15 half pages: the window end (row 200) comes within the
        // look-ahead (a quarter page) of the visible rows.
        for _ in 0..15 {
            effects.extend(key_effects(&mut app, "ctrl+d"));
        }
        assert_eq!(
            fetches(&effects).iter().map(|f| f.1).collect::<Vec<_>>(),
            [200]
        );
    }

    #[test]
    fn jump_to_end_and_cancel() {
        let mut app = data_app(100_000);
        let effects = key_effects(&mut app, "G");
        let f = fetches(&effects);
        assert_eq!(f.len(), 1);
        assert!(f[0].1 > 99_000, "loads the tail: {f:?}");
        assert_eq!(app.data.grid.row, 99_999);
        let effects = key_effects(&mut app, "ctrl+c");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Db(DbCommand::Cancel { .. })))
        );
        assert!(app.data.error.is_some());
    }

    #[test]
    fn sort_and_filter_requery() {
        let mut app = data_app(1000);
        key(&mut app, "l");
        let effects = key_effects(&mut app, "s");
        let f = fetches(&effects);
        assert_eq!(f.len(), 1);
        assert_eq!(app.data.sort[0].column, "status");
        assert!(count_id(&effects).is_some(), "recount after query change");

        key(&mut app, "f");
        assert_eq!(app.mode(), Mode::Filter);
        key(&mut app, "tab tab");
        typed(&mut app, "s4");
        let effects = key_effects(&mut app, "enter");
        assert_eq!(fetches(&effects).len(), 1);
        assert_eq!(app.data.filter.conditions[0].column, "status");
        assert_eq!(app.data.filter.conditions[0].op, Op::Eq);
        assert_eq!(app.data.filter.conditions[0].value, "s4");

        key(&mut app, "F");
        typed(&mut app, "id > 5");
        key(&mut app, "enter");
        assert_eq!(app.data.filter.free_where.as_deref(), Some("id > 5"));
        key(&mut app, "C");
        assert!(app.data.filter.is_empty());
    }

    #[test]
    fn search_copy_and_detail() {
        let mut app = data_app(1000);
        key(&mut app, "/");
        assert_eq!(app.mode(), Mode::Search);
        typed(&mut app, "s14");
        key(&mut app, "enter");
        assert_eq!((app.data.grid.row, app.data.grid.col), (14, 1));
        key(&mut app, "n");
        assert_eq!(app.data.grid.row, 140);

        // yy: whole row; Y + j: JSON of the current row.
        let effects = key_effects(&mut app, "y y");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t == "140\ts140")),
            "{effects:?}"
        );
        key(&mut app, "Y");
        let effects = key_effects(&mut app, "j");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t.contains("\"status\": \"s140\"")))
        );

        // Visual line selection over 3 rows, copied as TSV.
        key(&mut app, "V j j");
        assert_eq!(app.mode(), Mode::VisualLine);
        // `y` copies at once, without waiting for a possible `yy`.
        let effects = key_effects(&mut app, "y");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t.lines().count() == 3)),
            "{effects:?}"
        );
        assert_eq!(app.mode(), Mode::Normal, "selection ends after copy");

        key(&mut app, "enter");
        assert!(matches!(app.overlay, Some(Overlay::RowDetail { .. })));
        key(&mut app, "]");
        assert_eq!(app.data.grid.row, 143);
        key(&mut app, "esc");
        assert!(app.overlay.is_none());
    }

    #[test]
    fn stale_pages_are_ignored_after_requery() {
        let mut app = data_app(1000);
        let first = fetches(&key_effects(&mut app, "s"))[0].0;
        let second = fetches(&key_effects(&mut app, "s"))[0].0;
        app.update(Msg::Db(DbEvent::RowsLoaded {
            id: first,
            offset: 0,
            rows: rows(500, 5),
            elapsed: Duration::ZERO,
        }));
        assert!(app.data.rows.is_empty(), "stale page dropped");
        app.update(Msg::Db(DbEvent::RowsLoaded {
            id: second,
            offset: 0,
            rows: rows(0, 200),
            elapsed: Duration::ZERO,
        }));
        assert_eq!(app.data.rows.len(), 200);
    }

    // ------------------------------------------------------------ editing

    use crate::db::changes::{ApplyError, Change, NewValue};

    fn applied(effects: &[Effect]) -> Option<(u64, crate::db::changes::ChangeSet)> {
        effects.iter().find_map(|e| match e {
            Effect::Db(DbCommand::ApplyChanges { id, set }) => Some((*id, (**set).clone())),
            _ => None,
        })
    }

    /// Row id=1, column status: "s1" -> "paid".
    fn edited_app() -> App {
        let mut app = data_app(1000);
        key(&mut app, "j l e");
        assert_eq!(app.mode(), Mode::Edit);
        key(&mut app, "ctrl+u");
        typed(&mut app, "paid");
        key(&mut app, "enter");
        assert!(app.overlay.is_none());
        app
    }

    #[test]
    fn edit_a_cell_then_apply() {
        let mut app = edited_app();
        assert_eq!(app.pending_changes().map(|c| c.updated), Some(1));
        assert_eq!(app.cursor_original(), Some(Some("s1")));
        let (text, style) = crate::app::grid::GridSource::cell(&app.data, 1, 1);
        assert_eq!(
            (text.as_ref(), style),
            ("paid", crate::app::grid::CellStyle::Modified)
        );

        key(&mut app, "ctrl+s");
        assert!(matches!(
            app.overlay,
            Some(Overlay::ConfirmApply {
                production: false,
                ..
            })
        ));
        let effects = key_effects(&mut app, "o");
        let (id, set) = applied(&effects).expect("apply requested");
        assert_eq!(
            set.changes,
            vec![Change::Update {
                key: vec![("id".into(), "1".into())],
                set: vec![("status".into(), NewValue::Text("paid".into()))],
                expect: vec![("status".into(), Some("s1".into()))],
            }]
        );
        assert!(key(&mut app, "e").is_empty());
        assert!(app.overlay.is_none(), "no edition while applying");

        let effects = app.update(Msg::Db(DbEvent::ChangesApplied {
            id,
            counts: Some(set.counts()),
            elapsed: Duration::from_millis(4),
        }));
        assert!(app.pending_changes().is_none());
        assert!(!fetches(&effects).is_empty(), "rows reloaded");
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Success));
    }

    #[test]
    fn invalid_value_keeps_the_editor() {
        let mut app = data_app(10);
        key(&mut app, "e");
        key(&mut app, "ctrl+u");
        typed(&mut app, "abc");
        key(&mut app, "enter");
        match &app.overlay {
            Some(Overlay::CellEditor(ed)) => {
                assert!(ed.error.as_deref().unwrap().contains("entier"))
            }
            other => panic!("unexpected {other:?}"),
        }
        // Ctrl+n stages NULL whatever the text.
        key(&mut app, "esc");
        key(&mut app, "l e ctrl+n");
        assert!(app.overlay.is_none());
        assert_eq!(
            crate::app::grid::GridSource::cell(&app.data, 0, 1)
                .0
                .as_ref(),
            "NULL"
        );
    }

    #[test]
    fn insert_delete_undo_redo() {
        let mut app = data_app(1000);
        key(&mut app, "o");
        assert_eq!(app.mode(), Mode::Insert);
        key(&mut app, "esc");
        assert_eq!(app.data_position().as_deref(), Some("nouvelle ligne 1 / 1"));
        assert_eq!(app.data.row_count(), 1001);
        key(&mut app, "j");
        assert_eq!(app.data_position().as_deref(), Some("ligne 1 / 1 000"));
        key(&mut app, "d d");
        let c = app.pending_changes().unwrap();
        assert_eq!((c.inserted, c.deleted), (1, 1));
        key(&mut app, "u");
        assert_eq!(app.pending_changes().unwrap().deleted, 0);
        key(&mut app, "ctrl+r");
        assert_eq!(app.pending_changes().unwrap().deleted, 1);
        // dd on the new row removes it.
        key(&mut app, "gg d d");
        assert_eq!(app.pending_changes().unwrap().inserted, 0);
        // Discard everything, with confirmation.
        key(&mut app, "X");
        assert!(matches!(app.overlay, Some(Overlay::ConfirmDiscard)));
        key(&mut app, "o");
        assert!(app.pending_changes().is_none());
    }

    #[test]
    fn duplicate_row_opens_the_copied_key() {
        let mut app = data_app(1000);
        key(&mut app, "j p");
        // The bigint key is copied as is: the editor opens on it.
        match &app.overlay {
            Some(Overlay::CellEditor(ed)) => {
                assert_eq!(ed.column, "id");
                assert_eq!(ed.input.text(), "1");
            }
            other => panic!("unexpected {other:?}"),
        }
        key(&mut app, "ctrl+u");
        typed(&mut app, "5000");
        key(&mut app, "enter");
        assert_eq!(app.pending_changes().map(|c| c.inserted), Some(1));
        assert_eq!(
            app.data.staging.state.inserts[0],
            vec![NewValue::Text("5000".into()), NewValue::Text("s1".into()),]
        );
        // `u` removes the duplicate and the key edit.
        key(&mut app, "u u");
        assert!(app.pending_changes().is_none());
    }

    #[test]
    fn ctrl_g_inserts_a_uuid() {
        let mut app = data_app(10);
        key(&mut app, "l e ctrl+u ctrl+g");
        let text = match &app.overlay {
            Some(Overlay::CellEditor(ed)) => ed.input.text(),
            other => panic!("unexpected {other:?}"),
        };
        assert!(uuid::Uuid::parse_str(&text).is_ok(), "{text}");
        key(&mut app, "enter");
        assert_eq!(app.pending_changes().map(|c| c.updated), Some(1));
    }

    #[test]
    fn peek_the_referenced_row() {
        let mut app = data_app(10);
        let effects = key_effects(&mut app, "j l K");
        let id = match db(effects).as_slice() {
            [
                DbCommand::FetchReference {
                    id, column, row, ..
                },
            ] => {
                assert_eq!(column, "status");
                assert_eq!(row[1], ("status".into(), Some("s1".into())));
                *id
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(matches!(&app.overlay, Some(Overlay::Reference(p)) if p.result.is_none()));
        let object = app.data.object.clone().unwrap();
        app.update(Msg::Db(DbEvent::ReferenceLoaded {
            id,
            result: Ok(Box::new(crate::db::worker::Reference {
                object,
                constraint: "fk_status".into(),
                columns: vec![("code".into(), "text".into())],
                row: Some(vec![Some("s1".into())]),
            })),
        }));
        assert!(matches!(&app.overlay, Some(Overlay::Reference(p)) if p.result.is_some()));
        let effects = key_effects(&mut app, "y");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t.contains("\"code\": \"s1\""))),
            "{effects:?}"
        );
        key(&mut app, "esc");
        assert!(app.overlay.is_none());
        // A NULL value has nothing to point to.
        key(&mut app, "g g j j j");
        assert!(db(key_effects(&mut app, "K")).is_empty());
    }

    #[test]
    fn y_copies_the_cell_at_once() {
        let mut app = data_app(10);
        let effects = key_effects(&mut app, "j l y");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t == "s1")),
            "{effects:?}"
        );
    }

    #[test]
    fn conflict_keeps_changes_and_marks_the_cell() {
        let mut app = edited_app();
        key(&mut app, "ctrl+s");
        let (id, _) = applied(&key_effects(&mut app, "o")).unwrap();
        let effects = app.update(Msg::Db(DbEvent::ChangesFailed {
            id,
            error: ApplyError::Conflict { index: 0 },
        }));
        match &app.overlay {
            Some(Overlay::Error { message, .. }) => {
                assert!(
                    message.contains("Conflit") && message.contains("id = 1"),
                    "{message}"
                )
            }
            other => panic!("unexpected {other:?}"),
        }
        let f = fetches(&effects);
        assert!(!f.is_empty(), "current values reloaded");
        app.update(Msg::Db(DbEvent::RowsLoaded {
            id: f[0].0,
            offset: f[0].1,
            rows: rows(f[0].1, f[0].2),
            elapsed: Duration::ZERO,
        }));
        assert_eq!(app.data.grid.row, 1, "cursor kept on the edited row");
        assert_eq!(app.pending_changes().map(|c| c.updated), Some(1));
        let row = app.data.row_at(1).cloned();
        if let Some(row) = row {
            assert!(matches!(
                app.data.staging.cell(&row, 1),
                Some(crate::app::staging::CellChange::Conflict(_))
            ));
        }
    }

    #[test]
    fn leaving_the_table_asks_first() {
        let mut app = edited_app();
        key(&mut app, "ctrl+h k enter");
        assert!(matches!(app.overlay, Some(Overlay::PendingChanges(_))));
        assert_eq!(app.current.as_ref().unwrap().name, "orders");
        key(&mut app, "esc");
        assert_eq!(app.current.as_ref().unwrap().name, "orders", "stays");
        key(&mut app, "enter");
        key(&mut app, "x");
        assert_eq!(app.current.as_ref().unwrap().name, "customers");
        assert!(app.pending_changes().is_none());
    }

    #[test]
    fn quitting_asks_then_apply_and_quit() {
        let mut app = edited_app();
        key(&mut app, "q");
        assert!(!app.should_quit);
        key(&mut app, "a");
        assert!(matches!(app.overlay, Some(Overlay::ConfirmApply { .. })));
        let (id, set) = applied(&key_effects(&mut app, "enter")).unwrap();
        app.update(Msg::Db(DbEvent::ChangesApplied {
            id,
            counts: Some(set.counts()),
            elapsed: Duration::ZERO,
        }));
        assert!(app.should_quit, "quits once applied");
    }

    #[test]
    fn production_requires_the_connection_name() {
        let mut app = edited_app();
        app.servers[0].conn.production = Some(true);
        key(&mut app, "ctrl+s");
        assert!(matches!(
            app.overlay,
            Some(Overlay::ConfirmApply {
                production: true,
                ..
            })
        ));
        // `o` is text here, not a confirmation.
        assert!(applied(&key_effects(&mut app, "o")).is_none());
        let effects = key_effects(&mut app, "enter");
        assert!(applied(&effects).is_none());
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Warning));
        key(&mut app, "ctrl+s");
        typed(&mut app, "local");
        assert!(applied(&key_effects(&mut app, "enter")).is_some());
    }

    #[test]
    fn changes_view_and_generated_sql() {
        let mut app = edited_app();
        key(&mut app, "j d d");
        key(&mut app, "D");
        let Some(Overlay::Changes(v)) = &app.overlay else {
            panic!("changes view expected");
        };
        let ops: Vec<&str> = v.grid.rows.iter().map(|r| r[0].text.as_str()).collect();
        assert_eq!(ops, ["UPDATE", "DELETE"]);
        assert!(
            v.sql
                .starts_with("DELETE FROM public.orders AS t WHERE t.id = '2'::bigint"),
            "{}",
            v.sql
        );
        assert!(v.sql.contains("SET status = 'paid'::text"), "{}", v.sql);
        key(&mut app, "tab e");
        let Some(Overlay::Changes(v)) = &app.overlay else {
            panic!("changes view expected");
        };
        assert!(v.editing.is_some());
        key(&mut app, "ctrl+s");
        let effects = key_effects(&mut app, "o");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Db(DbCommand::RunScript { .. })))
        );
    }

    #[test]
    fn export_the_whole_query() {
        let mut app = data_app(1000);
        app.export_dir = Some("/tmp/x".into());
        key(&mut app, "E");
        let Some(Overlay::Export(d)) = &app.overlay else {
            panic!("export dialog expected");
        };
        assert!(d.path.text().starts_with("/tmp/x/orders-") && d.path.text().ends_with(".csv"));
        key(&mut app, "down");
        let Some(Overlay::Export(d)) = &app.overlay else {
            panic!("export dialog expected");
        };
        assert!(
            d.path.text().ends_with(".json"),
            "the extension follows the format"
        );
        let effects = key_effects(&mut app, "enter");
        match db(effects).as_slice() {
            [DbCommand::Export { request, spec, .. }] => {
                assert_eq!(request.object.name, "orders");
                assert_eq!(spec.format, crate::app::export::CopyFormat::Json);
                let cols: Vec<&str> = spec.columns.iter().map(|c| c.1.as_str()).collect();
                assert_eq!(cols, ["id", "status"]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(app.busy.is_some());
    }

    #[test]
    fn read_only_tables_refuse_edits() {
        let mut app = data_app(10);
        app.data.read_only_reason = Some("connexion en lecture seule".into());
        key(&mut app, "e");
        key(&mut app, "o");
        assert!(app.overlay.is_none() && app.pending_changes().is_none());
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Warning));
    }
}

// ------------------------------------------- discovery and several databases

mod discovery {
    use super::*;
    use crate::db::catalog::DatabaseInfo;
    use crate::db::discover::DiscoveredServer;
    use pretty_assertions::assert_eq;

    fn dbinfo(name: &str) -> DatabaseInfo {
        DatabaseInfo {
            name: name.into(),
            comment: None,
            size_bytes: Some(8192 * 1024),
            can_connect: true,
        }
    }

    fn picker_with_server() -> App {
        let mut app = app_with(&["local"], None);
        app.config_path = Some("/tmp/does-not-matter.toml".into());
        let effects = db(app.init());
        assert!(matches!(effects.as_slice(), [DbCommand::Discover { ports }] if ports == &[5432]));
        assert!(app.discovering);
        key(&mut app, "ctrl+n");
        assert_eq!(app.screen, Screen::Picker);
        app.update(Msg::Db(DbEvent::Discovered(vec![DiscoveredServer {
            host: "127.0.0.1".into(),
            port: 5432,
            tls: false,
        }])));
        assert_eq!(app.picker_len(), 2);
        app
    }

    #[test]
    fn login_on_a_discovered_server_then_save_it() {
        let mut app = picker_with_server();
        key(&mut app, "j enter");
        assert!(matches!(app.overlay, Some(Overlay::Login(_))));
        typed(&mut app, "secret");
        let cmds = key(&mut app, "enter");
        let request = match cmds.as_slice() {
            [
                DbCommand::Connect {
                    request,
                    resolve_password: false,
                },
            ] => request.clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(request.conn.host, "127.0.0.1");
        assert_eq!(request.conn.database.as_deref(), Some("postgres"));
        assert_eq!(request.password.as_deref(), Some("secret"));
        assert_eq!(request.conn.name, "localhost");

        // Wrong password: the form comes back with the error, password cleared.
        app.update(Msg::Db(DbEvent::ConnectFailed {
            connection: "localhost".into(),
            error: "password authentication failed".into(),
            password_required: true,
        }));
        match &app.overlay {
            Some(Overlay::Login(f)) => {
                assert_eq!(f.error.as_deref(), Some("password authentication failed"));
                assert!(f.password().is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        typed(&mut app, "right");
        key(&mut app, "enter");
        let mut i = info();
        i.database = "postgres".into();
        app.update(Msg::Db(DbEvent::Connected {
            connection: "localhost".into(),
            info: i,
            databases: vec![dbinfo("demo"), dbinfo("postgres"), dbinfo("shop")],
            elapsed: Duration::ZERO,
        }));
        assert!(matches!(app.overlay, Some(Overlay::SaveServer(_))));
        let effects = key_effects(&mut app, "o");
        assert!(
            matches!(effects.as_slice(), [Effect::SaveConnection(c)] if c.password.is_none() && c.name == "localhost")
        );
        assert_eq!(app.servers.len(), 2);
        assert!(app.server("localhost").unwrap().saved);

        let roots: Vec<_> = app
            .tree
            .rows
            .iter()
            .filter(|r| r.depth == 0)
            .map(|r| r.label.clone())
            .collect();
        assert_eq!(roots, ["local", "localhost"]);
        // The connected database comes first, then the others by name.
        let dbs: Vec<_> = app
            .tree
            .rows
            .iter()
            .filter(|r| r.depth == 1 && r.key.connection() == "localhost")
            .map(|r| r.label.clone())
            .collect();
        assert_eq!(dbs, ["postgres", "demo", "shop"]);
    }

    #[test]
    fn expanding_another_database_loads_its_catalog() {
        let mut app = app_with(&["local"], None);
        key(&mut app, "enter");
        let effects = db(app.update(Msg::Db(DbEvent::Connected {
            connection: "local".into(),
            info: info(),
            databases: vec![dbinfo("demo"), dbinfo("other")],
            elapsed: Duration::ZERO,
        })));
        assert!(
            matches!(effects.as_slice(), [DbCommand::LoadCatalog { database, .. }] if database == "demo")
        );
        app.update(Msg::Db(DbEvent::CatalogLoaded {
            catalog: catalog(),
            elapsed: Duration::ZERO,
        }));
        let effects = key(&mut app, "G l");
        assert!(
            matches!(effects.as_slice(), [DbCommand::LoadCatalog { database, .. }] if database == "other")
        );
        assert_eq!(
            app.tree.rows.last().unwrap().note.as_deref(),
            Some("chargement…")
        );
        // No second request while loading.
        assert!(key(&mut app, "h l").is_empty());
    }

    #[test]
    fn rescan_from_the_picker() {
        let mut app = picker_with_server();
        app.discovering = false;
        assert!(matches!(
            key(&mut app, "R").as_slice(),
            [DbCommand::Discover { .. }]
        ));
    }
}

// ------------------------------------------------------------- SQL tab

mod sql {
    use super::*;
    use crate::app::sql_tab::{SqlFocus, TxnState};
    use crate::db::exec::{ExecError, ResultColumn, StatementResult};
    use pretty_assertions::assert_eq;

    /// Types text in the editor (`\n` = Enter).
    fn write(app: &mut App, text: &str) {
        for c in text.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
    }

    fn executed(effects: &[Effect]) -> Vec<(u64, String)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Db(DbCommand::Execute {
                    id,
                    sql,
                    connection,
                    database,
                    ..
                }) => {
                    assert_eq!((connection.as_str(), database.as_str()), ("local", "demo"));
                    Some((*id, sql.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn rows_result(n: usize) -> StatementResult {
        StatementResult {
            columns: vec![ResultColumn {
                name: "n".into(),
                data_type: "int4".into(),
            }],
            rows: (0..n).map(|i| vec![Some(i.to_string())]).collect(),
            truncated: false,
            row_count: n as u64,
            returns_rows: true,
            source: None,
        }
    }

    fn done(app: &mut App, id: u64, result: StatementResult) -> Vec<Effect> {
        app.update(Msg::Db(DbEvent::StatementDone {
            id,
            result: Box::new(result),
            elapsed: Duration::from_millis(2),
        }))
    }

    fn sql_app() -> App {
        let mut app = connected_app();
        key(&mut app, "3");
        assert_eq!((app.tab, app.focus), (Tab::Sql, Focus::Center));
        assert_eq!(app.mode(), Mode::Sql);
        app
    }

    #[test]
    fn runs_the_statement_under_the_cursor() {
        let mut app = sql_app();
        write(&mut app, "select 1;\nselect 2");
        // `q`, `1`, `?` are text here.
        assert!(!app.should_quit);
        let effects = key_effects(&mut app, "ctrl+e");
        let ex = executed(&effects);
        assert_eq!(
            ex.iter().map(|e| e.1.as_str()).collect::<Vec<_>>(),
            ["select 2"]
        );
        assert!(app.busy.is_some());
        let effects = done(&mut app, ex[0].0, rows_result(3));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::AppendHistory(h) if h.sql == "select 2" && h.ok))
        );
        assert!(app.sql.running.is_none() && app.busy.is_none());
        assert_eq!(app.sql.results.len(), 1);
        assert_eq!(app.sql.results[0].label, "SELECT 3");
        assert_eq!(app.sql.history.len(), 1);
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Success));
    }

    #[test]
    fn a_script_runs_one_statement_at_a_time_and_stops_on_error() {
        let mut app = sql_app();
        write(&mut app, "select 1;\nselect nope;\nselect 3;");
        let ex = executed(&key_effects(&mut app, "f5"));
        assert_eq!(ex[0].1, "select 1");
        let ex = executed(&done(&mut app, ex[0].0, rows_result(1)));
        assert_eq!(ex[0].1, "select nope");
        let effects = app.update(Msg::Db(DbEvent::StatementFailed {
            id: ex[0].0,
            error: ExecError {
                message: "column \"nope\" does not exist".into(),
                position: Some(8),
            },
            elapsed: Duration::ZERO,
        }));
        assert!(executed(&effects).is_empty(), "stops at the first error");
        assert_eq!(app.sql.results.len(), 2);
        assert!(app.sql.results[1].error);
        assert_eq!(app.sql.active, 1, "the error is shown");
        // The cursor is on the error: line 2, column 8.
        assert_eq!(
            (app.sql.editor.cursor.row, app.sql.editor.cursor.col),
            (1, 7)
        );
        let msg = &app.status.as_ref().unwrap().text;
        assert!(msg.contains("1 instruction(s) non exécutée(s)"), "{msg}");
    }

    #[test]
    fn selection_runs_its_statements() {
        let mut app = sql_app();
        write(&mut app, "select 1;\nselect 2;\nselect 3;");
        key(&mut app, "ctrl+home shift+down shift+down shift+end");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        assert_eq!(ex[0].1, "select 1");
        let ex = executed(&done(&mut app, ex[0].0, rows_result(1)));
        assert_eq!(ex[0].1, "select 2");
        let ex = executed(&done(&mut app, ex[0].0, rows_result(1)));
        assert_eq!(ex[0].1, "select 3");
    }

    #[test]
    fn manual_transaction_and_quit_guard() {
        let mut app = sql_app();
        write(&mut app, "begin");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        done(&mut app, ex[0].0, StatementResult::default());
        assert_eq!(app.sql.txn, TxnState::Open);
        // A failing statement aborts it.
        key(&mut app, "ctrl+a");
        write(&mut app, "select 1/0");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        app.update(Msg::Db(DbEvent::StatementFailed {
            id: ex[0].0,
            error: ExecError {
                message: "division by zero".into(),
                position: None,
            },
            elapsed: Duration::ZERO,
        }));
        assert_eq!(app.sql.txn, TxnState::Aborted);
        // Quitting asks first.
        key(&mut app, "ctrl+h q");
        assert!(matches!(app.overlay, Some(Overlay::ConfirmTxnExit(_))));
        key(&mut app, "n");
        assert!(!app.should_quit);
        // ROLLBACK closes it.
        key(&mut app, "3 ctrl+a");
        write(&mut app, "rollback");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        done(&mut app, ex[0].0, StatementResult::default());
        assert_eq!(app.sql.txn, TxnState::Idle);
        key(&mut app, "ctrl+h q");
        assert!(app.should_quit);
    }

    #[test]
    fn writes_without_where_ask_first() {
        let mut app = sql_app();
        write(&mut app, "delete from t");
        assert!(executed(&key_effects(&mut app, "ctrl+e")).is_empty());
        assert!(matches!(app.overlay, Some(Overlay::ConfirmWrites { .. })));
        key(&mut app, "esc");
        assert!(app.overlay.is_none() && app.sql.running.is_none());
        key_effects(&mut app, "ctrl+e");
        let ex = executed(&key_effects(&mut app, "o"));
        assert_eq!(ex[0].1, "delete from t");
    }

    #[test]
    fn results_use_the_data_grid() {
        let mut app = sql_app();
        write(&mut app, "select n");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        done(&mut app, ex[0].0, rows_result(5));
        key(&mut app, "esc");
        assert_eq!(app.sql.focus, SqlFocus::Results);
        key(&mut app, "j j");
        assert_eq!(app.sql.active_view().unwrap().grid.row, 2);
        let effects = key_effects(&mut app, "y y");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Copy(t) if t == "2")),
            "{effects:?}"
        );
        // Sorting is the query's job.
        key(&mut app, "s");
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Info));
        key(&mut app, "enter");
        assert!(matches!(app.overlay, Some(Overlay::RowDetail { .. })));
        key(&mut app, "esc i");
        assert_eq!(app.sql.focus, SqlFocus::Editor);
        assert!(
            app.data.object.is_none() || app.data.object.is_some(),
            "data untouched"
        );
    }

    #[test]
    fn export_a_result_from_memory() {
        let mut app = sql_app();
        write(&mut app, "select n");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        done(&mut app, ex[0].0, rows_result(3));
        key(&mut app, "ctrl+j E");
        assert!(matches!(app.overlay, Some(Overlay::Export(_))));
        let effects = key_effects(&mut app, "enter");
        match effects.as_slice() {
            [Effect::WriteFile { path, text, .. }] => {
                assert!(path.to_string_lossy().ends_with(".csv"));
                assert_eq!(text, "n\n0\n1\n2\n");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn history_search_inserts_a_statement() {
        let mut app = sql_app();
        for q in [
            "select 1",
            "update t set a = 1 where id = 2",
            "select now()",
        ] {
            key(&mut app, "ctrl+a");
            write(&mut app, q);
            let ex = executed(&key_effects(&mut app, "ctrl+e"));
            done(&mut app, ex[0].0, StatementResult::default());
        }
        key(&mut app, "ctrl+a backspace ctrl+r");
        let Some(Overlay::HistorySearch(s)) = &app.overlay else {
            panic!("history expected");
        };
        assert_eq!(s.matches, [2, 1, 0], "most recent first");
        write(&mut app, "upd");
        key(&mut app, "enter");
        assert_eq!(app.sql.editor.text(), "update t set a = 1 where id = 2");
    }

    #[test]
    fn saved_queries() {
        let mut app = sql_app();
        write(&mut app, "select 42");
        key(&mut app, "ctrl+p");
        write(&mut app, "save\n");
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Warning));
        key(&mut app, "ctrl+p");
        write(&mut app, "save");
        let effects = {
            let mut e = Vec::new();
            for c in " rapport\n".chars() {
                let code = if c == '\n' {
                    KeyCode::Enter
                } else {
                    KeyCode::Char(c)
                };
                e.extend(app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))));
            }
            e
        };
        assert!(effects.iter().any(|e| matches!(e, Effect::SaveQuery { name, text } if name == "rapport" && text == "select 42")));
        key(&mut app, "ctrl+p");
        write(&mut app, "save ../x\n");
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Error));
        app.update(Msg::QueryLoaded {
            name: "autre".into(),
            text: "select 1;\nselect 2;".into(),
        });
        assert_eq!(app.sql.editor.lines().len(), 2);
        assert_eq!(app.sql.file.as_deref(), Some("autre"));
    }

    #[test]
    fn completion_popup() {
        let mut app = sql_app();
        write(&mut app, "select * from cu");
        let effects = key_effects(&mut app, "ctrl+space");
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Db(DbCommand::LoadCompletion { connection, database, .. })
                if connection == "local" && database == "demo"
        )));
        let labels: Vec<_> = app
            .sql
            .popup
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|c| c.insert.clone())
            .collect();
        assert_eq!(labels, ["customers"]);
        key(&mut app, "enter");
        assert_eq!(app.sql.editor.text(), "select * from customers");
        assert!(app.sql.popup.is_none());

        app.update(Msg::Db(DbEvent::CompletionLoaded {
            connection: "local".into(),
            database: "demo".into(),
            columns: ["id", "email", "name"]
                .iter()
                .map(|n| crate::sql::complete::Column {
                    schema: "public".into(),
                    table: "customers".into(),
                    name: n.to_string(),
                    data_type: "text".into(),
                })
                .collect(),
            search_path: vec!["public".into()],
        }));
        write(&mut app, " c where c.");
        // Columns load only once.
        assert!(db(key_effects(&mut app, "ctrl+space")).is_empty());
        assert_eq!(app.sql.popup.as_ref().unwrap().items.len(), 3);
        // Typing narrows the list, Esc closes it, Tab accepts.
        write(&mut app, "e");
        assert_eq!(app.sql.popup.as_ref().unwrap().items.len(), 1);
        key(&mut app, "tab");
        assert!(app.sql.editor.text().ends_with("c.email"));
        write(&mut app, " = ");
        assert!(app.sql.popup.is_none());
    }

    #[test]
    fn editable_result_applies_then_reruns() {
        let mut app = sql_app();
        write(&mut app, "select id, name from shop.items");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        let mut r = StatementResult {
            columns: ["id", "name"]
                .iter()
                .map(|n| ResultColumn {
                    name: n.to_string(),
                    data_type: if *n == "id" { "int4" } else { "text" }.into(),
                })
                .collect(),
            rows: vec![
                vec![Some("1".into()), Some("a".into())],
                vec![Some("2".into()), None],
            ],
            row_count: 2,
            returns_rows: true,
            ..Default::default()
        };
        r.source = Some(crate::db::exec::ResultSource {
            schema: "shop".into(),
            table: "items".into(),
            key: vec!["id".into()],
        });
        done(&mut app, ex[0].0, r);
        key(&mut app, "ctrl+j l e ctrl+u");
        assert_eq!(app.mode(), Mode::Edit);
        write(&mut app, "b\n");
        assert_eq!(app.pending_changes().map(|c| c.updated), Some(1));
        // No new run while edits are pending.
        key(&mut app, "i");
        assert!(executed(&key_effects(&mut app, "ctrl+e")).is_empty());
        key(&mut app, "ctrl+j ctrl+s");
        assert!(matches!(app.overlay, Some(Overlay::ConfirmApply { .. })));
        let effects = key_effects(&mut app, "o");
        let (id, set) = effects
            .iter()
            .find_map(|e| match e {
                Effect::Db(DbCommand::ApplyChanges { id, set }) => Some((*id, set.clone())),
                _ => None,
            })
            .expect("apply");
        assert_eq!(
            (set.object.schema.as_str(), set.object.name.as_str()),
            ("shop", "items")
        );
        assert_eq!(set.columns[0].data_type, "int4");
        let effects = app.update(Msg::Db(DbEvent::ChangesApplied {
            id,
            counts: Some(set.counts()),
            elapsed: Duration::ZERO,
        }));
        let ex = executed(&effects);
        assert_eq!(ex.len(), 1, "the statement runs again: {effects:?}");
        assert_eq!(ex[0].1, "select id, name from shop.items");
        assert!(app.pending_changes().is_none());
        // The Data tab is untouched.
        assert!(app.data.object.is_none());
    }

    #[test]
    fn vim_mode() {
        let mut app = connected_app();
        app.sql = crate::app::sql_tab::SqlState::new(crate::config::EditorMode::Vim, 4);
        key(&mut app, "3");
        assert_eq!(app.mode(), Mode::Normal);
        write(&mut app, "iselect 1");
        assert_eq!(app.mode(), Mode::Insert);
        key(&mut app, "esc");
        let ex = executed(&key_effects(&mut app, "ctrl+e"));
        assert_eq!(ex[0].1, "select 1");
        done(&mut app, ex[0].0, rows_result(1));
        // Keys vim does not use reach the keymap.
        write(&mut app, "dd");
        assert!(app.sql.editor.is_empty());
        key(&mut app, "q");
        assert!(app.should_quit);
    }

    #[test]
    fn no_connection_no_run() {
        let mut app = app_with(&["a"], None);
        key(&mut app, "3");
        write(&mut app, "select 1");
        assert!(executed(&key_effects(&mut app, "ctrl+e")).is_empty());
        assert_eq!(app.status.as_ref().map(|s| s.level), Some(Level::Warning));
    }
}

#[test]
fn virtual_relations_merge_into_details_and_flag_missing_databases() {
    let mut app = connected_app();
    app.relations = crate::config::relations::Relations::parse(
        r#"
[[relation]]
from = { database = "demo", table = "orders", columns = ["customer_id"] }
to = { database = "demo", table = "customers", columns = ["id"] }
kind = "souple"

[[relation]]
from = { database = "demo", table = "orders", columns = ["user_id"] }
to = { database = "auth", table = "users", columns = ["id"] }
kind = "cross_base"
"#,
    )
    .unwrap();
    key(&mut app, "2");
    let effects = key_effects(&mut app, "G enter");
    let obj = match db(effects).as_slice() {
        [DbCommand::LoadDetails(o)] => o.clone(),
        other => panic!("expected details request, got {other:?}"),
    };
    assert_eq!(obj.name, "orders");
    app.update(Msg::Db(DbEvent::DetailsLoaded {
        details: Box::new(crate::db::schema::ObjectDetails {
            object: obj.clone(),
            owner: None,
            comment: None,
            columns: vec![],
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            referenced_by: vec![],
            stats: None,
            properties: vec![],
            ddl: String::new(),
        }),
        elapsed: Duration::ZERO,
    }));
    let Some(DetailsState::Loaded(d)) = app.server("local").unwrap().details.get(&obj) else {
        panic!("details not cached");
    };
    let fks = d.foreign_keys.clone();
    assert_eq!(fks.len(), 2);
    assert!(fks.iter().all(|fk| fk.is_virtual()));
    assert_eq!(fks[1].to.database, "auth");
    // `demo` is on the server, `auth` is not: following the second one warns.
    assert!(app.has_database(&fks[0].to));
    assert!(!app.has_database(&fks[1].to));
}

#[test]
fn gd_follows_a_reference_filtered_and_ctrl_o_restores_the_previous_filter() {
    use crate::db::data::{Op, TextRows};
    use crate::db::schema::{Column, FkAction, FkOrigin, ForeignKey, ObjectDetails};

    fn details(obj: &ObjectRef, cols: &[&str], fks: Vec<ForeignKey>) -> ObjectDetails {
        ObjectDetails {
            object: obj.clone(),
            owner: None,
            comment: None,
            columns: cols
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    data_type: "bigint".into(),
                    nullable: *c != "id",
                    default: None,
                    identity: None,
                    generated: None,
                    collation: None,
                    comment: None,
                    primary_key: *c == "id",
                    references: None,
                })
                .collect(),
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            foreign_keys: fks,
            referenced_by: vec![],
            stats: None,
            properties: vec![],
            ddl: String::new(),
        }
    }
    fn fetch(effects: &[Effect]) -> (u64, crate::db::data::Filter) {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Db(DbCommand::FetchRows { id, request }) => {
                    Some((*id, request.filter.clone()))
                }
                _ => None,
            })
            .expect("a fetch")
    }

    let mut app = connected_app();
    key(&mut app, "2");
    let orders = match db(key_effects(&mut app, "G enter")).as_slice() {
        [DbCommand::LoadDetails(o)] => o.clone(),
        other => panic!("expected details request, got {other:?}"),
    };
    let customers = ObjectRef {
        name: "customers".into(),
        ..orders.clone()
    };
    let fk = ForeignKey {
        name: "orders_customer_id_fkey".into(),
        from: orders.clone(),
        from_columns: vec!["customer_id".into()],
        to: customers.clone(),
        to_columns: vec!["id".into()],
        on_update: FkAction::NoAction,
        on_delete: FkAction::NoAction,
        origin: FkOrigin::Declared,
    };
    let effects = app.update(Msg::Db(DbEvent::DetailsLoaded {
        details: Box::new(details(&orders, &["id", "customer_id"], vec![fk])),
        elapsed: Duration::ZERO,
    }));
    let (id, _) = fetch(&effects);
    app.update(Msg::Db(DbEvent::RowsLoaded {
        id,
        offset: 0,
        rows: TextRows {
            columns: vec!["id".into(), "customer_id".into()],
            rows: vec![vec![Some("1".into()), Some("42".into())]],
        },
        elapsed: Duration::ZERO,
    }));
    app.data.filter.free_where = Some("id > 0".into());

    // `gd` on `customer_id` opens `customers` filtered on id = 42.
    key(&mut app, "l");
    let effects = key_effects(&mut app, "g d");
    assert_eq!(app.current.as_ref(), Some(&customers));
    assert!(
        db(effects)
            .iter()
            .any(|c| matches!(c, DbCommand::LoadDetails(o) if o == &customers))
    );
    let effects = app.update(Msg::Db(DbEvent::DetailsLoaded {
        details: Box::new(details(&customers, &["id"], vec![])),
        elapsed: Duration::ZERO,
    }));
    let (_, filter) = fetch(&effects);
    assert_eq!(filter.conditions.len(), 1);
    assert_eq!(filter.conditions[0].column, "id");
    assert_eq!(filter.conditions[0].op, Op::Eq);
    assert_eq!(filter.conditions[0].value, "42");

    // `Ctrl+o` goes back to `orders` with its own filter.
    let effects = key_effects(&mut app, "ctrl+o");
    assert_eq!(app.current.as_ref(), Some(&orders));
    let (_, filter) = fetch(&effects);
    assert_eq!(filter.free_where.as_deref(), Some("id > 0"));
    assert!(filter.conditions.is_empty());

    // NULL / no reference: a message, no jump.
    key(&mut app, "0");
    key(&mut app, "g d");
    assert_eq!(app.current.as_ref(), Some(&orders));
}
