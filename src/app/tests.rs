use super::*;
use crate::config::ConnectionConfig;
use crate::db::catalog::{Catalog, DbObject, ObjectKind, Schema};
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
    key(&mut app, "tab");
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
    assert!(matches!(cmds.as_slice(), [DbCommand::LoadCatalog { .. }]));
    key(&mut app, "ctrl+p");
    typed(&mut app, "q");
    key(&mut app, "enter");
    assert!(app.should_quit);
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
        key(&mut app, "y");
        app.update(Msg::Tick); // `y` waits for a possible `yy`
        std::thread::sleep(crate::keymap::SEQUENCE_TIMEOUT);
        let effects = app.update(Msg::Tick);
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
