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

    // `ddl_round_trip` may have a temporary schema while this runs.
    let names: Vec<_> = catalog
        .schemas
        .iter()
        .map(|s| s.name.as_str())
        .filter(|n| !n.starts_with("ddl_rt_"))
        .collect();
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

// ------------------------------------------------------------- milestone 2

use pgtui::db::catalog::ObjectRef;
use pgtui::db::schema::{ConstraintKind, FkAction, Identity, ObjectDetails};

fn obj(schema: &str, name: &str, kind: ObjectKind) -> ObjectRef {
    ObjectRef {
        schema: schema.into(),
        name: name.into(),
        kind,
        signature: None,
    }
}

async fn details(url: &str, o: ObjectRef) -> ObjectDetails {
    let backend = db::connect(&request(url), no_lost()).await.unwrap();
    backend.object_details(&o).await.unwrap()
}

#[tokio::test]
async fn table_details() {
    let Some(url) = test_url() else { return };
    let d = details(&url, obj("shop", "orders", ObjectKind::Table)).await;
    assert_eq!(d.owner.as_deref(), Some("pgtui"));
    let names: Vec<_> = d.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "id",
            "customer_id",
            "status",
            "ordered_at",
            "shipped_at",
            "processing_time",
            "total",
            "notes"
        ]
    );
    let id = &d.columns[0];
    assert!(id.primary_key && !id.nullable);
    assert_eq!(id.identity, Some(Identity::ByDefault));
    let status = &d.columns[2];
    assert_eq!(status.data_type, "shop.order_status");
    assert_eq!(
        status.default.as_deref(),
        Some("'pending'::shop.order_status")
    );
    assert_eq!(
        d.columns[1].references.as_deref(),
        Some("shop.customers(id)")
    );
    assert_eq!(d.columns[6].data_type, "numeric(12,2)");
    assert!(d.columns[7].nullable);

    assert_eq!(d.foreign_keys.len(), 1);
    let fk = &d.foreign_keys[0];
    assert_eq!(
        (fk.to.schema.as_str(), fk.to.name.as_str()),
        ("shop", "customers")
    );
    assert_eq!(fk.on_delete, FkAction::Cascade);
    assert!(
        d.referenced_by
            .iter()
            .any(|f| f.from.name == "order_items" && f.from_columns == ["order_id"])
    );

    let pkey = d.indexes.iter().find(|i| i.name == "orders_pkey").unwrap();
    assert!(pkey.primary && pkey.unique && pkey.constraint_backed);
    let idx = d
        .indexes
        .iter()
        .find(|i| i.name == "orders_status_date_idx")
        .unwrap();
    assert_eq!(idx.columns, ["status", "ordered_at DESC"]);
    assert!(!idx.constraint_backed);
    assert_eq!(d.constraints[0].kind, ConstraintKind::PrimaryKey);

    let stats = d.stats.as_ref().unwrap();
    let rows = stats.estimated_rows.unwrap();
    assert!((90_000..=110_000).contains(&rows), "estimate {rows}");
    assert!(stats.total_bytes > stats.table_bytes && stats.index_bytes > 0);

    assert!(
        d.ddl.starts_with("CREATE TABLE shop.orders (\n"),
        "{}",
        d.ddl
    );
    assert!(
        d.ddl
            .contains("REFERENCES shop.customers(id) ON DELETE CASCADE"),
        "{}",
        d.ddl
    );
    assert!(d.ddl.contains(
        "CREATE INDEX orders_status_date_idx ON shop.orders USING btree (status, ordered_at DESC);"
    ));
    assert!(
        !d.ddl.contains("CREATE UNIQUE INDEX orders_pkey"),
        "constraint index not duplicated"
    );
}

#[tokio::test]
async fn triggers_checks_and_gin() {
    let Some(url) = test_url() else { return };
    let d = details(&url, obj("shop", "products", ObjectKind::Table)).await;
    let t = &d.triggers[0];
    assert_eq!(
        (
            t.name.as_str(),
            t.timing.as_str(),
            t.events.as_str(),
            t.level.as_str()
        ),
        ("products_touch", "BEFORE", "UPDATE", "ROW")
    );
    assert_eq!(t.function, "shop.touch_updated_at");
    assert!(t.enabled);
    assert!(
        d.ddl
            .contains("CREATE TRIGGER products_touch BEFORE UPDATE ON shop.products"),
        "{}",
        d.ddl
    );
    let checks: Vec<_> = d
        .constraints
        .iter()
        .filter(|c| c.kind == ConstraintKind::Check)
        .collect();
    assert_eq!(checks.len(), 2);
    assert!(
        checks
            .iter()
            .any(|c| c.definition == "CHECK (price > 0::numeric)"),
        "{checks:?}"
    );
    let gin = d.indexes.iter().find(|i| i.method == "gin").unwrap();
    assert_eq!(gin.columns, ["attributes jsonb_path_ops"]);
}

#[tokio::test]
async fn partitioned_view_matview() {
    let Some(url) = test_url() else { return };
    let m = details(
        &url,
        obj("shop", "measurements", ObjectKind::PartitionedTable),
    )
    .await;
    assert!(
        m.properties
            .contains(&("Partitionnement".into(), "RANGE (taken_on)".into())),
        "{:?}",
        m.properties
    );
    assert!(m.ddl.contains("PARTITION BY RANGE (taken_on)"), "{}", m.ddl);
    assert!(
        m.ddl
            .contains("CREATE TABLE shop.measurements_2025 PARTITION OF shop.measurements"),
        "{}",
        m.ddl
    );
    let st = m.stats.unwrap();
    assert_eq!(st.estimated_rows, Some(5000));
    assert!(st.total_bytes > 0, "sizes summed over partitions");

    let v = details(&url, obj("shop", "v_order_totals", ObjectKind::View)).await;
    assert_eq!(v.columns.len(), 5);
    assert!(v.stats.is_none());
    assert!(
        v.ddl
            .starts_with("CREATE OR REPLACE VIEW shop.v_order_totals AS\n"),
        "{}",
        v.ddl
    );
    assert!(
        v.ddl
            .contains("COMMENT ON VIEW shop.v_order_totals IS 'Total recalculé par commande';")
    );

    let mv = details(
        &url,
        obj("shop", "mv_daily_sales", ObjectKind::MaterializedView),
    )
    .await;
    assert!(
        mv.ddl
            .starts_with("CREATE MATERIALIZED VIEW shop.mv_daily_sales AS\n"),
        "{}",
        mv.ddl
    );
    assert!(mv.ddl.contains("WITH DATA;"));
    assert!(mv.stats.is_some());
}

#[tokio::test]
async fn sequence_and_function_details() {
    let Some(url) = test_url() else { return };
    let s = details(
        &url,
        obj("shop", "invoice_number_seq", ObjectKind::Sequence),
    )
    .await;
    assert!(
        s.properties.contains(&("Début".into(), "1000".into())),
        "{:?}",
        s.properties
    );
    assert!(s.ddl.contains("START WITH 1000"), "{}", s.ddl);
    let owned = details(&url, obj("shop", "products_id_seq", ObjectKind::Sequence)).await;
    assert!(
        owned.ddl.contains("OWNED BY shop.products.id;"),
        "{}",
        owned.ddl
    );

    let mut f = obj("shop", "order_total", ObjectKind::Function);
    f.signature = Some("p_order_id bigint".into());
    let d = details(&url, f).await;
    assert!(
        d.ddl
            .starts_with("CREATE OR REPLACE FUNCTION shop.order_total(p_order_id bigint)"),
        "{}",
        d.ddl
    );
    assert!(d.ddl.contains("COMMENT ON FUNCTION shop.order_total(p_order_id bigint) IS 'Somme des lignes d''une commande';"));
    assert!(
        d.properties
            .contains(&("Volatilité".into(), "STABLE".into()))
    );

    let mut p = obj("shop", "recompute_totals", ObjectKind::Procedure);
    p.signature = Some(String::new());
    let d = details(&url, p).await;
    assert!(
        d.ddl
            .starts_with("CREATE OR REPLACE PROCEDURE shop.recompute_totals()"),
        "{}",
        d.ddl
    );
}

#[tokio::test]
async fn missing_object_is_an_error() {
    let Some(url) = test_url() else { return };
    let backend = db::connect(&request(&url), no_lost()).await.unwrap();
    let err = backend
        .object_details(&obj("shop", "nope", ObjectKind::Table))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no longer exists"), "{err}");
}

/// The generated DDL recreates an identical table in another schema.
#[tokio::test]
async fn ddl_round_trip() {
    let Some(url) = test_url() else { return };
    let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let target = format!("ddl_rt_{}", std::process::id());
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {target} CASCADE; CREATE SCHEMA {target}"
        ))
        .await
        .unwrap();

    let backend = db::connect(&request(&url), no_lost()).await.unwrap();
    let originals = [
        ("hr", "departments"),
        ("hr", "employees"),
        ("shop", "customers"),
    ];
    let mut result = Ok(());
    for (schema, name) in originals {
        let original = backend
            .object_details(&obj(schema, name, ObjectKind::Table))
            .await
            .unwrap();
        let ddl = original
            .ddl
            .replace(&format!("{schema}."), &format!("{target}."));
        if let Err(e) = client.batch_execute(&ddl).await {
            result = Err(format!("{schema}.{name}: {e:?}\n{ddl}"));
            break;
        }
        let copy = backend
            .object_details(&obj(&target, name, ObjectKind::Table))
            .await
            .unwrap();
        let shape = |d: &ObjectDetails, from: &str| {
            let cols: Vec<_> = d
                .columns
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        c.data_type.clone(),
                        c.nullable,
                        c.default.clone(),
                        c.identity,
                        c.comment.clone(),
                    )
                })
                .collect();
            let cons: Vec<_> = d
                .constraints
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        c.definition.replace(&format!("{from}."), "S."),
                    )
                })
                .collect();
            let idx: Vec<_> = d
                .indexes
                .iter()
                .map(|i| (i.name.clone(), i.columns.clone(), i.unique))
                .collect();
            (cols, cons, idx, d.comment.clone())
        };
        if shape(&original, schema) != shape(&copy, &target) {
            result = Err(format!(
                "{schema}.{name} differs:\n{:#?}\n---\n{:#?}",
                shape(&original, schema),
                shape(&copy, &target)
            ));
            break;
        }
    }
    client
        .batch_execute(&format!("DROP SCHEMA {target} CASCADE"))
        .await
        .unwrap();
    result.unwrap();
}
