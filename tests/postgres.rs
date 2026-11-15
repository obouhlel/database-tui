//! Integration tests against the demo database from `docker-compose.yml`.
//!
//! ```sh
//! docker compose up -d
//! PGTUI_TEST_DATABASE_URL=postgres://pgtui:pgtui@localhost:5433/demo cargo test --test postgres
//! ```
//! Without `PGTUI_TEST_DATABASE_URL` the tests are skipped (they pass with a notice).

use std::sync::Arc;
use std::time::Duration;

use pgtui::config::SslMode;
use pgtui::db::catalog::ObjectKind;
use pgtui::db::worker::{self, DbCommand, DbEvent};
use pgtui::db::{self, ConnectError, ConnectRequest};
use tokio::sync::mpsc::unbounded_channel;

fn test_url() -> Option<String> {
    let url = std::env::var("PGTUI_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("PGTUI_TEST_DATABASE_URL not set: skipping PostgreSQL integration test");
    }
    url
}

fn request(url: &str) -> ConnectRequest {
    let (conn, password) = db::connection_from_url("test", url).unwrap();
    ConnectRequest {
        conn,
        password,
        statement_timeout_secs: 0,
    }
}

fn no_lost() -> db::postgres::OnConnectionLost {
    Arc::new(|_| {})
}

#[tokio::test]
async fn connects_and_reports_server_info() {
    let Some(url) = test_url() else { return };
    let backend = db::connect(&request(&url), no_lost()).await.unwrap();
    let info = backend.info();
    assert_eq!(info.database, "demo");
    assert_eq!(info.user, "pgtui");
    assert!(
        info.version
            .split('.')
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            >= 13,
        "{}",
        info.version
    );
}

#[tokio::test]
async fn loads_catalog() {
    let Some(url) = test_url() else { return };
    let backend = db::connect(&request(&url), no_lost()).await.unwrap();
    let catalog = backend.load_catalog(false).await.unwrap();

    let names: Vec<_> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["public", "audit", "hr", "shop"],
        "public first, then alphabetical"
    );
    assert!(catalog.schemas.iter().all(|s| !s.is_system));

    let shop = catalog.schemas.iter().find(|s| s.name == "shop").unwrap();
    assert_eq!(
        shop.comment.as_deref(),
        Some("Boutique en ligne de démonstration")
    );
    let find = |name: &str| shop.objects.iter().find(|o| o.name == name);

    assert_eq!(find("customers").unwrap().kind, ObjectKind::Table);
    assert_eq!(
        find("customers").unwrap().comment.as_deref(),
        Some("Clients de la boutique")
    );
    assert_eq!(
        find("measurements").unwrap().kind,
        ObjectKind::PartitionedTable
    );
    assert!(find("measurements_2025").is_none(), "partitions are hidden");
    assert_eq!(find("v_order_totals").unwrap().kind, ObjectKind::View);
    assert_eq!(
        find("mv_daily_sales").unwrap().kind,
        ObjectKind::MaterializedView
    );
    assert_eq!(
        find("invoice_number_seq").unwrap().kind,
        ObjectKind::Sequence
    );
    assert!(
        find("customers_id_seq").is_none(),
        "identity sequences are hidden"
    );
    assert!(
        find("products_id_seq").is_some(),
        "serial sequences are shown"
    );
    assert_eq!(
        find("recompute_totals").unwrap().kind,
        ObjectKind::Procedure
    );
    let f = find("order_total").unwrap();
    assert_eq!(f.kind, ObjectKind::Function);
    assert_eq!(f.signature.as_deref(), Some("p_order_id bigint"));
    assert_eq!(f.display_name(), "order_total(p_order_id bigint)");
    // Sorted by group: tables before views before functions.
    let kinds: Vec<_> = shop.objects.iter().map(|o| o.kind.group()).collect();
    let mut sorted = kinds.clone();
    sorted.sort();
    assert_eq!(kinds, sorted);
}

#[tokio::test]
async fn catalog_with_system_schemas() {
    let Some(url) = test_url() else { return };
    let backend = db::connect(&request(&url), no_lost()).await.unwrap();
    let catalog = backend.load_catalog(true).await.unwrap();
    let pg_catalog = catalog
        .schemas
        .iter()
        .find(|s| s.name == "pg_catalog")
        .unwrap();
    assert!(pg_catalog.is_system);
    assert!(pg_catalog.objects.iter().any(|o| o.name == "pg_class"));
    assert!(
        catalog.schemas.last().unwrap().is_system,
        "system schemas come last"
    );
    assert!(
        !catalog
            .schemas
            .iter()
            .any(|s| s.name.starts_with("pg_toast"))
    );
}

#[tokio::test]
async fn wrong_or_missing_password_asks_for_one() {
    let Some(url) = test_url() else { return };
    let mut req = request(&url);
    req.password = Some("definitely-wrong".into());
    match db::connect(&req, no_lost()).await {
        Err(ConnectError::PasswordRequired(_)) => {}
        Err(e) => panic!("expected PasswordRequired, got {e}"),
        Ok(_) => panic!("connection with a wrong password succeeded"),
    }
    req.password = None;
    match db::connect(&req, no_lost()).await {
        Err(ConnectError::PasswordRequired(_)) => {}
        Err(e) => panic!("expected PasswordRequired, got {e:?}"),
        Ok(_) => panic!("connected without password"),
    }
}

#[tokio::test]
async fn tls_required_on_plain_server_fails_cleanly() {
    let Some(url) = test_url() else { return };
    let mut req = request(&url);
    req.conn.sslmode = SslMode::Require;
    let err = match db::connect(&req, no_lost()).await {
        Err(e) => e.to_string(),
        Ok(b) => {
            // A server with TLS enabled is fine too.
            assert!(b.info().tls);
            return;
        }
    };
    assert!(!err.is_empty());
}

#[tokio::test]
async fn unreachable_server_is_an_error() {
    let Some(_) = test_url() else { return };
    let req = request("postgres://pgtui:pgtui@127.0.0.1:1/demo");
    assert!(matches!(
        db::connect(&req, no_lost()).await,
        Err(ConnectError::Other(_))
    ));
}

#[tokio::test]
async fn worker_connect_and_catalog() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    let mut req = request(&url);
    // Password resolution: the URL password wins, no prompt needed.
    req.statement_timeout_secs = 5;
    commands
        .send(DbCommand::Connect {
            request: Box::new(req),
            resolve_password: true,
        })
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, DbEvent::Connected { .. }), "{ev:?}");

    commands
        .send(DbCommand::LoadCatalog {
            include_system: false,
        })
        .unwrap();
    match tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        DbEvent::CatalogLoaded { catalog, .. } => assert!(catalog.object_count() > 10),
        other => panic!("unexpected {other:?}"),
    }

    commands.send(DbCommand::Disconnect).unwrap();
    commands
        .send(DbCommand::LoadCatalog {
            include_system: false,
        })
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, DbEvent::CatalogFailed(_)), "{ev:?}");
}

#[tokio::test]
async fn worker_resolves_password_cmd() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    let mut req = request(&url);
    let pw = req
        .password
        .take()
        .expect("test URL must contain the password");
    req.conn.password_cmd = Some(format!("printf '%s\\n' '{pw}'"));
    commands
        .send(DbCommand::Connect {
            request: Box::new(req),
            resolve_password: true,
        })
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, DbEvent::Connected { .. }), "{ev:?}");
}
