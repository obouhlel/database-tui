//! Render tests on ratatui's in-memory backend.

use std::time::Duration;

use crossterm::event::KeyEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, Msg, Start};
use crate::config::{Config, ConnectionConfig};
use crate::db::catalog::{Catalog, DbObject, ObjectKind, Schema};
use crate::db::worker::DbEvent;
use crate::db::{BackendKind, ServerInfo};
use crate::keymap::{KeyChord, Keymap};
use crate::ui::theme::Theme;

fn app(conn: ConnectionConfig) -> App {
    let config = Config {
        connections: vec![conn],
        ..Default::default()
    };
    App::new(
        config,
        Keymap::default(),
        Theme::dark(),
        vec!["a warning".into()],
        Vec::new(),
        Start::Picker,
    )
}

fn press(app: &mut App, keys: &str) {
    for c in KeyChord::parse_seq(keys).unwrap() {
        app.update(Msg::Key(KeyEvent::new(c.code, c.mods)));
    }
}

fn connect(app: &mut App) {
    press(app, "enter");
    app.update(Msg::Db(DbEvent::Connected {
        info: ServerInfo {
            kind: BackendKind::Postgres,
            version: "17.2".into(),
            user: "pgtui".into(),
            database: "demo".into(),
            tls: false,
        },
        elapsed: Duration::from_millis(12),
    }));
    let obj = |n: &str, kind| DbObject {
        name: n.into(),
        kind,
        comment: Some("Clients".into()),
        signature: None,
    };
    app.update(Msg::Db(DbEvent::CatalogLoaded {
        catalog: Catalog {
            schemas: vec![Schema {
                name: "shop".into(),
                comment: None,
                is_system: false,
                objects: vec![
                    obj("customers", ObjectKind::Table),
                    obj("v_orders", ObjectKind::View),
                ],
            }],
        },
        elapsed: Duration::from_millis(3),
    }));
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| super::draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn picker_lists_connections_and_warnings() {
    let mut c = ConnectionConfig::named("local");
    c.user = Some("postgres".into());
    c.read_only = true;
    let mut app = app(c);
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("local"), "{screen}");
    assert!(
        screen.contains("postgres@localhost:5432/postgres"),
        "{screen}"
    );
    assert!(screen.contains("RO"), "{screen}");
    assert!(screen.contains("a warning"), "{screen}");
}

#[test]
fn main_screen_tree_and_status() {
    let mut app = app(ConnectionConfig::named("demo"));
    connect(&mut app);
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("▾ shop"), "{screen}");
    assert!(screen.contains("▦ customers"), "{screen}");
    assert!(screen.contains("NORMAL"), "{screen}");
    assert!(screen.contains("● demo"), "{screen}");
    assert!(screen.contains("pgtui@demo"), "{screen}");
    assert!(screen.contains("1 Schema"), "{screen}");
    assert!(!screen.contains("PRODUCTION"), "{screen}");
}

#[test]
fn production_banner() {
    let mut c = ConnectionConfig::named("prod");
    c.color = Some("red".into());
    let mut app = app(c);
    connect(&mut app);
    let screen = render(&mut app, 120, 20);
    assert!(
        screen.lines().next().unwrap().contains("PRODUCTION — prod"),
        "{screen}"
    );
    assert!(screen.contains("PROD"), "{screen}");
}

#[test]
fn open_object_shows_details() {
    let mut app = app(ConnectionConfig::named("demo"));
    connect(&mut app);
    press(&mut app, "j j enter");
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("shop.customers"), "{screen}");
    assert!(screen.contains("Clients"), "{screen}");
}

#[test]
fn filter_line_and_help_overlay() {
    let mut app = app(ConnectionConfig::named("demo"));
    connect(&mut app);
    press(&mut app, "/");
    for c in "v_o".chars() {
        app.update(Msg::Key(KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )));
    }
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("/v_o"), "{screen}");
    assert!(screen.contains("FILTER"), "{screen}");
    assert!(!screen.contains("customers"), "{screen}");

    press(&mut app, "enter ?");
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("Aide — raccourcis"), "{screen}");
    assert!(screen.contains("Filtrer"), "{screen}");
}

#[test]
fn tiny_terminal_does_not_panic() {
    let mut app = app(ConnectionConfig::named("demo"));
    for (w, h) in [(1, 1), (10, 3), (30, 5)] {
        render(&mut app, w, h);
    }
    connect(&mut app);
    press(&mut app, "?");
    for (w, h) in [(1, 1), (10, 3), (30, 5)] {
        render(&mut app, w, h);
    }
}
