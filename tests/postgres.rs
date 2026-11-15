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
            connection: "test".into(),
            database: "demo".into(),
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

    commands
        .send(DbCommand::Disconnect {
            connection: "test".into(),
        })
        .unwrap();
    commands
        .send(DbCommand::LoadCatalog {
            connection: "test".into(),
            database: "demo".into(),
            include_system: false,
        })
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, DbEvent::CatalogFailed { .. }), "{ev:?}");
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
        connection: "test".into(),
        database: "demo".into(),
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

// ------------------------------------------------------------- milestone 3

use pgtui::app::data_view::DataView;
use pgtui::db::data::{Combinator, Condition, Filter, Op, RowCount, SortDir, SortKey};

async fn data_view(
    url: &str,
    schema: &str,
    name: &str,
) -> (std::sync::Arc<dyn pgtui::db::Backend>, DataView) {
    let backend = db::connect(&request(url), no_lost()).await.unwrap();
    let d = backend
        .object_details(&obj(schema, name, ObjectKind::Table))
        .await
        .unwrap();
    (backend, DataView::new(&d, 200, false))
}

async fn raw_count(url: &str, sql: &str) -> i64 {
    let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    client.query_one(sql, &[]).await.unwrap().get(0)
}

fn cond(column: &str, op: Op, value: &str) -> Condition {
    Condition {
        column: column.into(),
        op,
        value: value.into(),
    }
}

#[tokio::test]
async fn pages_in_key_order() {
    let Some(url) = test_url() else { return };
    let (b, v) = data_view(&url, "shop", "orders").await;
    let p0 = b.fetch_rows(&v.request(0, 200)).await.unwrap();
    assert_eq!(p0.rows.len(), 200);
    assert_eq!(p0.columns.last().map(String::as_str), Some("xmin"));
    assert_eq!(p0.columns.len(), 9);
    let id_col = p0.columns.iter().position(|c| c == "id").unwrap();
    assert_eq!(p0.rows[0][id_col].as_deref(), Some("1"));
    let p1 = b.fetch_rows(&v.request(200, 200)).await.unwrap();
    assert_eq!(p1.rows[0][id_col].as_deref(), Some("201"));
    assert_eq!(
        b.count_rows(&v.request(0, 0), 1_000_000).await.unwrap(),
        RowCount::Exact(100_000)
    );
    assert_eq!(
        b.count_rows(&v.request(0, 0), 1000).await.unwrap(),
        RowCount::AtLeast(1000)
    );
}

#[tokio::test]
async fn structured_filters_match_hand_written_sql() {
    let Some(url) = test_url() else { return };
    let (b, mut v) = data_view(&url, "shop", "orders").await;
    v.filter = Filter {
        conditions: vec![
            cond("status", Op::Eq, "paid"),
            cond("total", Op::Gt, "100.5"),
        ],
        combinator: Combinator::And,
        free_where: None,
    };
    let expected = raw_count(
        &url,
        "SELECT count(*) FROM shop.orders WHERE status = 'paid' AND total > 100.5",
    )
    .await;
    assert_eq!(
        b.count_rows(&v.request(0, 0), 1_000_000).await.unwrap(),
        RowCount::Exact(expected as u64)
    );

    v.filter.combinator = Combinator::Or;
    v.filter.free_where = Some("id < 50000".into());
    let expected = raw_count(
        &url,
        "SELECT count(*) FROM shop.orders WHERE (status = 'paid' OR total > 100.5) AND (id < 50000)",
    )
    .await;
    assert_eq!(
        b.count_rows(&v.request(0, 0), 1_000_000).await.unwrap(),
        RowCount::Exact(expected as u64)
    );

    // Sort: total desc then id.
    v.filter = Filter::default();
    v.sort = vec![SortKey {
        column: "total".into(),
        dir: SortDir::Desc,
    }];
    let rows = b.fetch_rows(&v.request(0, 2)).await.unwrap();
    let total_col = rows.columns.iter().position(|c| c == "total").unwrap();
    let max: String = {
        let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(conn);
        client
            .query_one("SELECT max(total)::text FROM shop.orders", &[])
            .await
            .unwrap()
            .get(0)
    };
    assert_eq!(rows.rows[0][total_col].as_deref(), Some(max.as_str()));
}

#[tokio::test]
async fn text_patterns_nulls_and_empty_strings() {
    let Some(url) = test_url() else { return };
    let (b, mut v) = data_view(&url, "shop", "customers").await;
    let count = |v: &DataView| v.request(0, 0);

    v.filter.conditions = vec![cond("full_name", Op::ILike, "%ZOÉ%")];
    let expected = raw_count(
        &url,
        "SELECT count(*) FROM shop.customers WHERE full_name ILIKE '%zoé%'",
    )
    .await;
    assert!(expected > 0);
    assert_eq!(
        b.count_rows(&count(&v), 1_000_000).await.unwrap(),
        RowCount::Exact(expected as u64)
    );

    v.filter.conditions = vec![cond("email", Op::Regex, "^user1[0-9]@")];
    assert_eq!(
        b.count_rows(&count(&v), 1_000_000).await.unwrap(),
        RowCount::Exact(10)
    );

    v.filter.conditions = vec![cond("notes", Op::IsNull, "")];
    let nulls = raw_count(
        &url,
        "SELECT count(*) FROM shop.customers WHERE notes IS NULL",
    )
    .await;
    assert_eq!(
        b.count_rows(&count(&v), 1_000_000).await.unwrap(),
        RowCount::Exact(nulls as u64)
    );

    // NULL and the empty string stay distinct.
    v.filter.conditions = vec![cond("id", Op::Le, "3")];
    let rows = b.fetch_rows(&v.request(0, 10)).await.unwrap();
    let notes = rows.columns.iter().position(|c| c == "notes").unwrap();
    let values: Vec<_> = rows.rows.iter().map(|r| r[notes].clone()).collect();
    assert!(values.contains(&None), "{values:?}");
    assert!(values.contains(&Some(String::new())), "{values:?}");

    // Array, jsonb, inet, bytea come back as their text representation.
    let tags = rows.columns.iter().position(|c| c == "tags").unwrap();
    assert!(
        rows.rows
            .iter()
            .any(|r| r[tags].as_deref().is_some_and(|t| t.starts_with('{')))
    );
}

#[tokio::test]
async fn values_cannot_inject_sql() {
    let Some(url) = test_url() else { return };
    let (b, mut v) = data_view(&url, "public", "settings").await;
    v.filter.conditions = vec![cond("key", Op::Eq, "x'; DROP TABLE public.settings; --")];
    let rows = b.fetch_rows(&v.request(0, 10)).await.unwrap();
    assert!(rows.rows.is_empty());
    assert_eq!(
        raw_count(&url, "SELECT count(*) FROM public.settings").await,
        4
    );

    // A value that is not valid for the column type is a clean error.
    let (b, mut v) = data_view(&url, "shop", "orders").await;
    v.filter.conditions = vec![cond("id", Op::Eq, "abc")];
    let err = b
        .fetch_rows(&v.request(0, 10))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("invalid input syntax for type bigint"),
        "{err}"
    );
}

#[tokio::test]
async fn free_where_errors_are_reported() {
    let Some(url) = test_url() else { return };
    let (b, mut v) = data_view(&url, "shop", "orders").await;
    v.filter.free_where = Some("id <".into());
    let err = b
        .fetch_rows(&v.request(0, 10))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("syntax error"), "{err}");
}

#[tokio::test]
async fn read_only_session_refuses_writes() {
    let Some(url) = test_url() else { return };
    let mut req = request(&url);
    req.conn.read_only = true;
    let b = db::connect(&req, no_lost()).await.unwrap();
    let d = b
        .object_details(&obj("shop", "orders", ObjectKind::Table))
        .await
        .unwrap();
    let mut v = DataView::new(&d, 10, true);
    assert!(
        v.read_only_reason
            .as_deref()
            .unwrap()
            .contains("lecture seule")
    );
    // nextval() writes: refused by default_transaction_read_only.
    v.filter.free_where = Some("nextval('shop.invoice_number_seq') > 0".into());
    let err = b
        .fetch_rows(&v.request(0, 1))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only transaction"), "{err}");
}

#[tokio::test]
async fn long_query_can_be_cancelled() {
    let Some(url) = test_url() else { return };
    let (b, mut v) = data_view(&url, "public", "settings").await;
    v.filter.free_where = Some("(SELECT pg_sleep(10)) IS NOT NULL".into());
    let started = std::time::Instant::now();
    let fetch = {
        let b = b.clone();
        let req = v.request(0, 10);
        tokio::spawn(async move { b.fetch_rows(&req).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    b.cancel().await.unwrap();
    let err = fetch.await.unwrap().unwrap_err().to_string();
    assert!(err.contains("canceling statement"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

// ------------------------------------------- discovery and several databases

#[tokio::test]
async fn discovers_the_demo_server() {
    let Some(_) = test_url() else { return };
    let found = pgtui::db::discover::scan(&[5433]).await;
    assert!(
        found
            .iter()
            .any(|s| s.host == "127.0.0.1" && s.port == 5433),
        "{found:?}"
    );
}

#[tokio::test]
async fn lists_databases() {
    let Some(url) = test_url() else { return };
    let b = db::connect(&request(&url), no_lost()).await.unwrap();
    let dbs = b.list_databases().await.unwrap();
    let names: Vec<_> = dbs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"demo") && names.contains(&"postgres"),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.starts_with("template")));
    let demo = dbs.iter().find(|d| d.name == "demo").unwrap();
    assert!(demo.can_connect && demo.size_bytes.unwrap() > 1_000_000);
}

/// Connected to `demo`, the worker opens `postgres` on demand with the same
/// credentials, and routes object requests to the right database.
#[tokio::test]
async fn worker_opens_other_databases_lazily() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    commands
        .send(DbCommand::Connect {
            request: Box::new(request(&url)),
            resolve_password: false,
        })
        .unwrap();
    match next_event(&mut rx).await {
        DbEvent::Connected {
            info, databases, ..
        } => {
            assert_eq!(info.database, "demo");
            assert!(databases.iter().any(|d| d.name == "postgres"));
        }
        other => panic!("unexpected {other:?}"),
    }
    commands
        .send(DbCommand::LoadCatalog {
            connection: "test".into(),
            database: "postgres".into(),
            include_system: false,
        })
        .unwrap();
    match next_event(&mut rx).await {
        DbEvent::CatalogLoaded { catalog, .. } => {
            assert_eq!(catalog.database, "postgres");
            assert!(
                !catalog.schemas.iter().any(|s| s.name == "shop"),
                "shop lives in demo only"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    // An object of `demo` still goes to `demo`.
    commands
        .send(DbCommand::LoadDetails(obj(
            "shop",
            "orders",
            ObjectKind::Table,
        )))
        .unwrap();
    match next_event(&mut rx).await {
        DbEvent::DetailsLoaded { details, .. } => assert_eq!(details.object.database, "demo"),
        other => panic!("unexpected {other:?}"),
    }
    // An unknown database is a clean error.
    commands
        .send(DbCommand::LoadCatalog {
            connection: "test".into(),
            database: "nope".into(),
            include_system: false,
        })
        .unwrap();
    match next_event(&mut rx).await {
        DbEvent::CatalogFailed {
            database, error, ..
        } => {
            assert_eq!(database, "nope");
            assert!(error.contains("does not exist"), "{error}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

async fn next_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<DbEvent>) -> DbEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

// ---------------------------------------------------------------- tunnels

/// `tunnel_cmd` forwarding to the demo database (stands in for ssh -L).
fn forward_cmd(url: &str, prefix: &str) -> String {
    let (conn, _) = db::connection_from_url("t", url).unwrap();
    format!(
        "{prefix}python3 -I {}/tests/fixtures/forward.py {{port}} {} {}",
        env!("CARGO_MANIFEST_DIR"),
        conn.host,
        conn.port
    )
}

#[tokio::test]
async fn connects_through_a_tunnel_command() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    let mut req = request(&url);
    // An unreachable host: only the tunnel leads to the server.
    req.conn.host = "db.unreachable.invalid".into();
    req.conn.tunnel_cmd = Some(forward_cmd(&url, ""));
    commands
        .send(DbCommand::Connect {
            request: Box::new(req),
            resolve_password: false,
        })
        .unwrap();
    let mut progress = Vec::new();
    loop {
        match next_event(&mut rx).await {
            DbEvent::Progress { message, .. } => progress.push(message),
            DbEvent::Connected {
                info, databases, ..
            } => {
                assert_eq!(info.database, "demo");
                assert!(databases.iter().any(|d| d.name == "postgres"));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        progress,
        ["Ouverture du tunnel…", "Tunnel ouvert, connexion…"]
    );
    // Other databases go through the same tunnel.
    commands
        .send(DbCommand::LoadCatalog {
            connection: "test".into(),
            database: "postgres".into(),
            include_system: false,
        })
        .unwrap();
    assert!(matches!(
        next_event(&mut rx).await,
        DbEvent::CatalogLoaded { .. }
    ));
}

#[tokio::test]
async fn tunnel_failure_and_death_are_reported() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);

    let mut req = request(&url);
    req.conn.tunnel_cmd =
        Some("echo 'ssh: Could not resolve hostname bastion' >&2; exit 255 # {port}".into());
    commands
        .send(DbCommand::Connect {
            request: Box::new(req),
            resolve_password: false,
        })
        .unwrap();
    loop {
        match next_event(&mut rx).await {
            DbEvent::Progress { .. } => {}
            DbEvent::ConnectFailed {
                error,
                password_required,
                ..
            } => {
                assert!(
                    error.contains("Could not resolve hostname bastion"),
                    "{error}"
                );
                assert!(!password_required);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // A tunnel that dies after 3 s: the session is reported lost.
    let mut req = request(&url);
    req.conn.tunnel_cmd = Some(forward_cmd(&url, "timeout 3 "));
    commands
        .send(DbCommand::Connect {
            request: Box::new(req),
            resolve_password: false,
        })
        .unwrap();
    let lost = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await.unwrap() {
                DbEvent::ConnectionLost { message, .. } => return message,
                DbEvent::Progress { .. } | DbEvent::Connected { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    })
    .await
    .expect("tunnel death reported");
    assert!(!lost.is_empty());
}

/// Two connections open at once; closing one leaves the other usable.
#[tokio::test]
async fn worker_keeps_connections_apart() {
    let Some(url) = test_url() else { return };
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    for name in ["a", "b"] {
        let mut req = request(&url);
        req.conn.name = name.into();
        commands
            .send(DbCommand::Connect {
                request: Box::new(req),
                resolve_password: true,
            })
            .unwrap();
    }
    let mut connected = Vec::new();
    while connected.len() < 2 {
        match next_event(&mut rx).await {
            DbEvent::Connected { connection, .. } => connected.push(connection),
            DbEvent::Progress { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    connected.sort();
    assert_eq!(connected, ["a", "b"]);

    commands
        .send(DbCommand::Disconnect {
            connection: "a".into(),
        })
        .unwrap();
    for (name, ok) in [("b", true), ("a", false)] {
        commands
            .send(DbCommand::LoadCatalog {
                connection: name.into(),
                database: "demo".into(),
                include_system: false,
            })
            .unwrap();
        match next_event(&mut rx).await {
            DbEvent::CatalogLoaded { catalog, .. } if ok => assert_eq!(catalog.connection, "b"),
            DbEvent::CatalogFailed { connection, .. } if !ok => assert_eq!(connection, "a"),
            other => panic!("unexpected {other:?}"),
        }
    }
}

// ------------------------------------------------------------ staging

mod staging {
    use super::*;
    use pgtui::db::catalog::ObjectRef;
    use pgtui::db::changes::{ApplyError, Change, ChangeSet, NewValue};
    use pgtui::db::data::{ColumnSpec, DataRequest, Filter};

    /// A scratch table `edit_<pid>_<tag>` in its own schema, with 3 rows.
    async fn scratch(url: &str, tag: &str) -> (tokio_postgres::Client, ObjectRef) {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(conn);
        let schema = format!("edit_{}_{tag}", std::process::id());
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};
                 CREATE TABLE {schema}.items (id integer PRIMARY KEY, name text, price numeric(8,2) DEFAULT 1.50);
                 INSERT INTO {schema}.items VALUES (1, 'one', 10), (2, 'two', 20), (3, NULL, 30);"
            ))
            .await
            .unwrap();
        let object = ObjectRef {
            connection: "test".into(),
            database: "demo".into(),
            schema,
            name: "items".into(),
            kind: ObjectKind::Table,
            signature: None,
        };
        (client, object)
    }

    fn columns() -> Vec<ColumnSpec> {
        [
            ("id", "integer"),
            ("name", "text"),
            ("price", "numeric(8,2)"),
        ]
        .iter()
        .map(|(n, t)| ColumnSpec {
            name: n.to_string(),
            data_type: t.to_string(),
        })
        .collect()
    }

    fn key(id: &str) -> Vec<(String, String)> {
        vec![("id".into(), id.into())]
    }

    async fn rows(client: &tokio_postgres::Client, object: &ObjectRef) -> Vec<String> {
        client
            .query(
                &format!(
                    "SELECT format('%s|%s|%s', id, coalesce(name, 'NULL'), price) FROM {}.items ORDER BY id",
                    object.schema
                ),
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }

    async fn drop(client: &tokio_postgres::Client, object: &ObjectRef) {
        client
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", object.schema))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn applies_updates_inserts_and_deletes_in_one_transaction() {
        let Some(url) = test_url() else { return };
        let (client, object) = scratch(&url, "ok").await;
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        // Row versions as the Data tab reads them.
        let read = backend
            .fetch_rows(&DataRequest {
                object: object.clone(),
                columns: columns(),
                filter: Filter::default(),
                sort: vec![],
                tiebreak: vec!["id".into()],
                order_by_ctid: false,
                with_row_version: true,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        let xmin_of_2 = read.rows[1][3].clone();
        let set = ChangeSet {
            object: object.clone(),
            columns: columns(),
            changes: vec![
                Change::Delete {
                    key: key("2"),
                    expect_version: xmin_of_2,
                },
                Change::Update {
                    key: key("1"),
                    set: vec![
                        ("name".into(), NewValue::Text("l'un".into())),
                        ("price".into(), NewValue::Text("12.5".into())),
                    ],
                    expect: vec![
                        ("name".into(), Some("one".into())),
                        ("price".into(), Some("10.00".into())),
                    ],
                },
                Change::Update {
                    key: key("3"),
                    set: vec![("name".into(), NewValue::Text("trois".into()))],
                    expect: vec![("name".into(), None)],
                },
                Change::Insert {
                    values: vec![
                        ("id".into(), NewValue::Text("4".into())),
                        ("name".into(), NewValue::Null),
                        ("price".into(), NewValue::Default),
                    ],
                },
            ],
        };
        let counts = backend.apply_changes(&set).await.unwrap();
        assert_eq!((counts.updated, counts.inserted, counts.deleted), (2, 1, 1));
        assert_eq!(
            rows(&client, &object).await,
            ["1|l'un|12.50", "3|trois|30.00", "4|NULL|1.50"]
        );
        drop(&client, &object).await;
    }

    #[tokio::test]
    async fn a_conflict_rolls_everything_back() {
        let Some(url) = test_url() else { return };
        let (client, object) = scratch(&url, "conflict").await;
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        // Someone else changes row 2 after it was read.
        client
            .batch_execute(&format!(
                "UPDATE {}.items SET name = 'deux' WHERE id = 2",
                object.schema
            ))
            .await
            .unwrap();
        let set = ChangeSet {
            object: object.clone(),
            columns: columns(),
            changes: vec![
                Change::Update {
                    key: key("1"),
                    set: vec![("name".into(), NewValue::Text("changed".into()))],
                    expect: vec![("name".into(), Some("one".into()))],
                },
                Change::Update {
                    key: key("2"),
                    set: vec![("name".into(), NewValue::Text("mine".into()))],
                    expect: vec![("name".into(), Some("two".into()))],
                },
            ],
        };
        assert_eq!(
            backend.apply_changes(&set).await.unwrap_err(),
            ApplyError::Conflict { index: 1 }
        );
        assert_eq!(
            rows(&client, &object).await,
            ["1|one|10.00", "2|deux|20.00", "3|NULL|30.00"],
            "the first update was rolled back too"
        );
        // A deleted row is a conflict as well.
        let set = ChangeSet {
            object: object.clone(),
            columns: columns(),
            changes: vec![Change::Delete {
                key: key("99"),
                expect_version: None,
            }],
        };
        assert_eq!(
            backend.apply_changes(&set).await.unwrap_err(),
            ApplyError::Conflict { index: 0 }
        );
        drop(&client, &object).await;
    }

    #[tokio::test]
    async fn server_errors_roll_back_and_name_the_change() {
        let Some(url) = test_url() else { return };
        let (client, object) = scratch(&url, "error").await;
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let set = ChangeSet {
            object: object.clone(),
            columns: columns(),
            changes: vec![
                Change::Insert {
                    values: vec![("id".into(), NewValue::Text("5".into()))],
                },
                Change::Insert {
                    values: vec![("id".into(), NewValue::Text("1".into()))],
                },
            ],
        };
        match backend.apply_changes(&set).await.unwrap_err() {
            ApplyError::Failed {
                index: Some(1),
                message,
            } => assert!(message.contains("duplicate key"), "{message}"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(rows(&client, &object).await.len(), 3, "nothing inserted");
        // The session is usable afterwards (no transaction left open).
        backend
            .run_script(&format!(
                "UPDATE {0}.items SET price = 0 WHERE id = 3; DELETE FROM {0}.items WHERE id = 2;",
                object.schema
            ))
            .await
            .unwrap();
        assert_eq!(rows(&client, &object).await, ["1|one|10.00", "3|NULL|0.00"]);
        // A failing script changes nothing.
        let err = backend
            .run_script(&format!(
                "DELETE FROM {0}.items; SELECT 1/0;",
                object.schema
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("division by zero"), "{err}");
        assert_eq!(rows(&client, &object).await.len(), 2);
        drop(&client, &object).await;
    }

    #[tokio::test]
    async fn worker_reports_apply_results() {
        let Some(url) = test_url() else { return };
        let (client, object) = scratch(&url, "worker").await;
        let (tx, mut rx) = unbounded_channel();
        let commands = worker::spawn(tx);
        commands
            .send(DbCommand::Connect {
                request: Box::new(request(&url)),
                resolve_password: true,
            })
            .unwrap();
        loop {
            match next_event(&mut rx).await {
                DbEvent::Connected { .. } => break,
                DbEvent::Progress { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        commands
            .send(DbCommand::ApplyChanges {
                id: 7,
                set: Box::new(ChangeSet {
                    object: object.clone(),
                    columns: columns(),
                    changes: vec![Change::Delete {
                        key: key("3"),
                        expect_version: None,
                    }],
                }),
            })
            .unwrap();
        match next_event(&mut rx).await {
            DbEvent::ChangesApplied {
                id: 7,
                counts: Some(c),
                ..
            } => assert_eq!(c.deleted, 1),
            other => panic!("unexpected {other:?}"),
        }
        commands
            .send(DbCommand::RunScript {
                id: 8,
                object: object.clone(),
                sql: "SELECT nope".into(),
            })
            .unwrap();
        assert!(matches!(
            next_event(&mut rx).await,
            DbEvent::ChangesFailed { id: 8, .. }
        ));
        drop(&client, &object).await;
    }
}

// ------------------------------------------------------------ SQL tab

mod sql_tab {
    use super::*;

    #[tokio::test]
    async fn select_returns_typed_text_rows() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let r = backend
            .execute(
                "select id, email, null::jsonb as j from shop.customers order by id limit 3",
                100,
            )
            .await
            .unwrap();
        let names: Vec<_> = r
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str()))
            .collect();
        assert_eq!(names, [("id", "int8"), ("email", "text"), ("j", "jsonb")]);
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[0][0].as_deref(), Some("1"));
        assert_eq!(r.rows[0][2], None);
        assert!(r.returns_rows && !r.truncated);
        assert_eq!(r.row_count, 3);

        let r = backend
            .execute("select generate_series(1, 50)", 10)
            .await
            .unwrap();
        assert_eq!((r.rows.len(), r.truncated, r.row_count), (10, true, 50));
        let r = backend.execute("select 1 where false", 10).await.unwrap();
        assert!(r.returns_rows && r.rows.is_empty());
    }

    #[tokio::test]
    async fn editable_results_are_detected() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let source = |sql: &'static str| {
            let b = &backend;
            async move { b.execute(sql, 5).await.unwrap().source }
        };
        let s = source("select id, email from shop.customers where id < 3")
            .await
            .unwrap();
        assert_eq!((s.schema.as_str(), s.table.as_str()), ("shop", "customers"));
        assert_eq!(s.key, ["id"]);
        assert!(
            source("select * from shop.customers limit 1")
                .await
                .is_some()
        );
        assert!(
            source("select email from shop.customers limit 1")
                .await
                .is_none(),
            "no key"
        );
        assert!(
            source("select id as x, email from shop.customers limit 1")
                .await
                .is_none(),
            "renamed"
        );
        assert!(
            source("select id, upper(email) from shop.customers limit 1")
                .await
                .is_none()
        );
        assert!(
            source("select c.id, o.id from shop.customers c join shop.orders o on o.customer_id = c.id limit 1")
                .await
                .is_none()
        );
        assert!(
            source("select * from shop.v_order_totals limit 1")
                .await
                .is_none(),
            "a view"
        );
    }

    #[tokio::test]
    async fn errors_carry_the_position() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let e = backend
            .execute("select nope from shop.customers", 10)
            .await
            .unwrap_err();
        assert!(e.message.contains("nope"), "{e}");
        assert_eq!(e.position, Some(8));
    }

    #[tokio::test]
    async fn explain_and_explain_analyze_leave_no_trace() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let json = backend
            .explain("select * from shop.orders where id = 3", false, false)
            .await
            .unwrap();
        let plan = pgtui::app::plan::parse(&json).unwrap();
        assert!(!plan.analyze);
        assert!(plan.lines[0].title.contains("orders"), "{:?}", plan.lines);

        let count = |b: std::sync::Arc<dyn db::Backend>| async move {
            let r = b
                .execute("select count(*) from shop.order_items", 1)
                .await
                .unwrap();
            r.rows[0][0].clone().unwrap()
        };
        let backend: std::sync::Arc<dyn db::Backend> = backend;
        let before = count(backend.clone()).await;
        let json = backend
            .explain(
                "delete from shop.order_items where order_id > 0",
                true,
                false,
            )
            .await
            .unwrap();
        let plan = pgtui::app::plan::parse(&json).unwrap();
        assert!(plan.analyze);
        assert!(plan.summary.iter().any(|s| s.starts_with("Exécution")));
        assert_eq!(count(backend.clone()).await, before, "rolled back");

        // Inside a transaction: a savepoint, the transaction stays usable.
        backend.execute("begin", 1).await.unwrap();
        backend
            .explain(
                "delete from shop.order_items where order_id > 0",
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(count(backend.clone()).await, before);
        // An error keeps the outer transaction alive too.
        let e = backend
            .explain("select nope", true, true)
            .await
            .unwrap_err();
        assert_eq!(e.position, Some(8), "relative to the statement: {e}");
        assert_eq!(count(backend.clone()).await, before);
        backend.execute("rollback", 1).await.unwrap();
    }

    #[tokio::test]
    async fn completion_columns_and_search_path() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        let (cols, path) = backend.completion_columns(false).await.unwrap();
        assert_eq!(path, ["public"]);
        assert!(cols.iter().any(|c| c.schema == "shop"
            && c.table == "orders"
            && c.name == "total"
            && c.data_type == "numeric(12,2)"));
        assert!(
            cols.iter().any(|c| c.table == "v_order_totals"),
            "views too"
        );
        assert!(!cols.iter().any(|c| c.schema == "pg_catalog"));
    }

    #[tokio::test]
    async fn manual_transaction_spans_statements() {
        let Some(url) = test_url() else { return };
        let backend = db::connect(&request(&url), no_lost()).await.unwrap();
        backend.execute("begin", 10).await.unwrap();
        backend
            .execute("create temp table tx_probe (a int) on commit drop", 10)
            .await
            .unwrap();
        let r = backend
            .execute("insert into tx_probe values (1), (2)", 10)
            .await
            .unwrap();
        assert!(!r.returns_rows);
        assert_eq!(r.row_count, 2);
        let r = backend
            .execute("update tx_probe set a = a + 1 returning a", 10)
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 2);
        // An error aborts the transaction until ROLLBACK.
        assert!(backend.execute("select 1/0", 10).await.is_err());
        let e = backend.execute("select 1", 10).await.unwrap_err();
        assert!(e.message.contains("aborted"), "{e}");
        backend.execute("rollback", 10).await.unwrap();
        backend.execute("select 1", 10).await.unwrap();
        assert!(backend.execute("select * from tx_probe", 10).await.is_err());
    }
}

#[tokio::test]
async fn worker_exports_a_whole_table() {
    use pgtui::app::export::{CopyFormat, ExportSpec};
    use pgtui::app::value::ValueKind;
    use pgtui::db::data::{ColumnSpec, DataRequest, Filter};
    let Some(url) = test_url() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = unbounded_channel();
    let commands = worker::spawn(tx);
    commands
        .send(DbCommand::Connect {
            request: Box::new(request(&url)),
            resolve_password: true,
        })
        .unwrap();
    loop {
        match next_event(&mut rx).await {
            DbEvent::Connected { .. } => break,
            DbEvent::Progress { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    let req = DataRequest {
        object: obj("shop", "orders", ObjectKind::Table),
        columns: vec![
            ColumnSpec {
                name: "id".into(),
                data_type: "bigint".into(),
            },
            ColumnSpec {
                name: "status".into(),
                data_type: "text".into(),
            },
        ],
        filter: Filter::default(),
        sort: vec![],
        tiebreak: vec!["id".into()],
        order_by_ctid: false,
        with_row_version: true,
        offset: 0,
        limit: 0,
    };
    for (fmt, id) in [(CopyFormat::Csv, 1), (CopyFormat::Json, 2)] {
        let path = dir.path().join(format!("c.{}", fmt.extension()));
        commands
            .send(DbCommand::Export {
                id,
                request: Box::new(req.clone()),
                spec: Box::new(ExportSpec {
                    format: fmt,
                    path: path.clone(),
                    columns: vec![
                        (1, "status".into(), ValueKind::Text),
                        (0, "id".into(), ValueKind::Integer),
                    ],
                    schema: "shop".into(),
                    table: "orders".into(),
                }),
            })
            .unwrap();
        let rows = loop {
            match next_event(&mut rx).await {
                DbEvent::ExportProgress { .. } => {}
                DbEvent::ExportDone { rows, .. } => break rows,
                other => panic!("unexpected {other:?}"),
            }
        };
        assert!(rows > 5_000, "several pages: {rows}");
        let text = std::fs::read_to_string(&path).unwrap();
        match fmt {
            CopyFormat::Csv => {
                assert_eq!(text.lines().count() as u64, rows + 1);
                assert_eq!(text.lines().next(), Some("status,id"));
                assert!(text.lines().nth(1).unwrap().ends_with(",1"));
            }
            _ => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v.as_array().unwrap().len() as u64, rows);
                assert_eq!(v[0]["id"], serde_json::json!(1));
            }
        }
    }
}
