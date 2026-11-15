use super::*;
use crate::db::BackendKind;
use crate::db::catalog::{DbObject, ObjectKind, Schema};
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
            Effect::Copy(_) => None,
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
        database: "d".into(),
        tls: false,
    }
}

fn catalog() -> Catalog {
    let table = |n: &str| DbObject {
        name: n.into(),
        kind: ObjectKind::Table,
        comment: None,
        signature: None,
    };
    Catalog {
        schemas: vec![Schema {
            name: "public".into(),
            comment: None,
            is_system: false,
            objects: vec![table("customers"), table("orders")],
        }],
    }
}

/// Picker -> connected -> catalog loaded.
fn connected_app() -> App {
    let mut app = app_with(&["local"], None);
    key(&mut app, "enter");
    let cmds = db(app.update(Msg::Db(DbEvent::Connected {
        info: info(),
        elapsed: Duration::from_millis(5),
    })));
    assert!(matches!(
        cmds.as_slice(),
        [DbCommand::LoadCatalog {
            include_system: false
        }]
    ));
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: catalog(),
        elapsed: Duration::from_millis(3),
    }));
    app
}

#[test]
fn picker_preselects_default_connection() {
    let app = app_with(&["a", "b", "c"], Some("b"));
    assert_eq!(app.picker_cursor, 1);
    assert_eq!(app.screen, Screen::Picker);
}

#[test]
fn picker_navigation_and_connect() {
    let mut app = app_with(&["a", "b"], None);
    key(&mut app, "j");
    assert_eq!(app.picker_cursor, 1);
    key(&mut app, "j");
    assert_eq!(app.picker_cursor, 1, "clamped");
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
    assert!(app.connecting.is_some());
    // A second Enter while connecting does nothing.
    assert!(key(&mut app, "enter").is_empty());
}

#[test]
fn connect_then_catalog() {
    let app = connected_app();
    assert_eq!(app.screen, Screen::Main);
    assert!(app.connecting.is_none());
    assert_eq!(app.tree.rows.len(), 4); // public, Tables, customers, orders
}

#[test]
fn password_prompt_flow() {
    let mut app = app_with(&["local"], None);
    key(&mut app, "enter");
    app.update(Msg::Db(DbEvent::ConnectFailed {
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
fn connect_error_shows_popup_and_stays_on_picker() {
    let mut app = app_with(&["local"], None);
    key(&mut app, "enter");
    app.update(Msg::Db(DbEvent::ConnectFailed {
        error: "refused".into(),
        password_required: false,
    }));
    assert!(matches!(app.overlay, Some(Overlay::Error { .. })));
    assert_eq!(app.screen, Screen::Picker);
    key(&mut app, "esc");
    assert!(app.overlay.is_none());
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
    assert_eq!(app.tree.rows.len(), 3);
    assert_eq!(app.tree.selected().unwrap().label, "customers");
    // `q` is text while filtering, not quit.
    typed(&mut app, "q");
    assert!(!app.should_quit);
    key(&mut app, "esc");
    assert_eq!(app.mode(), Mode::Normal);
    assert_eq!(app.tree.rows.len(), 4);
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
fn connection_lost_blocks_reload() {
    let mut app = connected_app();
    app.update(Msg::Db(DbEvent::ConnectionLost("eof".into())));
    assert!(app.session.as_ref().unwrap().lost.is_some());
    assert!(key(&mut app, "R").is_empty());
}

#[test]
fn reconnect_keeps_tree_state() {
    let mut app = connected_app();
    key(&mut app, "G");
    key(&mut app, ":");
    typed(&mut app, "reconnect");
    let cmds = key(&mut app, "enter");
    assert!(matches!(cmds.as_slice(), [DbCommand::Connect { .. }]));
    app.update(Msg::Db(DbEvent::Connected {
        info: info(),
        elapsed: Duration::ZERO,
    }));
    assert!(
        app.session.as_ref().unwrap().catalog.is_some(),
        "catalog kept while reloading"
    );
    assert_eq!(app.tree.selected().unwrap().label, "orders");
}
