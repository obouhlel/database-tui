//! Catalog queries for the schema tree.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use tokio_postgres::Client;

use crate::db::catalog::{Catalog, DbObject, ObjectKind, Schema};

const SYSTEM_SCHEMAS: &[&str] = &["pg_catalog", "information_schema"];

/// Hidden in every case: toast and temporary schemas.
const SCHEMAS_SQL: &str = "
SELECT n.nspname::text, obj_description(n.oid, 'pg_namespace')
FROM pg_namespace n
WHERE n.nspname !~ '^pg_(toast|temp_)'
  AND ($1 OR n.nspname NOT IN ('pg_catalog', 'information_schema'))
ORDER BY n.nspname";

/// Partitions are hidden (their parent is listed instead), and so are the
/// sequences backing identity columns (an implementation detail).
const RELATIONS_SQL: &str = "
SELECT n.nspname::text, c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class')
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind IN ('r', 'p', 'f', 'v', 'm', 'S')
  AND NOT c.relispartition
  AND NOT (c.relkind = 'S' AND EXISTS (
      SELECT 1 FROM pg_depend d
      WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'i'))
  AND n.nspname !~ '^pg_(toast|temp_)'
  AND ($1 OR n.nspname NOT IN ('pg_catalog', 'information_schema'))
ORDER BY 1, 2";

/// Functions and procedures, without those owned by extensions.
const FUNCTIONS_SQL: &str = "
SELECT n.nspname::text, p.proname::text, p.prokind::text,
       pg_get_function_identity_arguments(p.oid), obj_description(p.oid, 'pg_proc')
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE p.prokind IN ('f', 'p')
  AND n.nspname !~ '^pg_(toast|temp_)'
  AND ($1 OR n.nspname NOT IN ('pg_catalog', 'information_schema'))
  AND NOT EXISTS (
      SELECT 1 FROM pg_depend d
      WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
ORDER BY 1, 2, 4";

pub async fn load(client: &Client, database: &str, include_system: bool) -> Result<Catalog> {
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[&include_system];
    let (schemas, relations, functions) = tokio::try_join!(
        client.query(SCHEMAS_SQL, params),
        client.query(RELATIONS_SQL, params),
        client.query(FUNCTIONS_SQL, params),
    )
    .context("loading catalog")?;

    let mut by_schema: BTreeMap<String, Schema> = schemas
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let schema = Schema {
                is_system: SYSTEM_SCHEMAS.contains(&name.as_str()),
                name: name.clone(),
                comment: r.get(1),
                objects: Vec::new(),
            };
            (name, schema)
        })
        .collect();

    for r in &relations {
        let kind = match r.get::<_, String>(2).as_str() {
            "r" => ObjectKind::Table,
            "p" => ObjectKind::PartitionedTable,
            "f" => ObjectKind::ForeignTable,
            "v" => ObjectKind::View,
            "m" => ObjectKind::MaterializedView,
            "S" => ObjectKind::Sequence,
            _ => continue,
        };
        if let Some(s) = by_schema.get_mut(&r.get::<_, String>(0)) {
            s.objects.push(DbObject {
                name: r.get(1),
                kind,
                comment: r.get(3),
                signature: None,
            });
        }
    }
    for r in &functions {
        let kind = if r.get::<_, String>(2) == "p" {
            ObjectKind::Procedure
        } else {
            ObjectKind::Function
        };
        if let Some(s) = by_schema.get_mut(&r.get::<_, String>(0)) {
            s.objects.push(DbObject {
                name: r.get(1),
                kind,
                signature: Some(r.get(3)),
                comment: r.get(4),
            });
        }
    }

    let mut schemas: Vec<Schema> = by_schema.into_values().collect();
    for s in &mut schemas {
        s.objects.sort_by(|a, b| {
            (a.kind.group(), &a.name, &a.signature).cmp(&(b.kind.group(), &b.name, &b.signature))
        });
    }
    // User schemas first, `public` leading them; system schemas last.
    schemas.sort_by_key(|s| (s.is_system, s.name != "public", s.name.clone()));
    Ok(Catalog {
        database: database.to_string(),
        schemas,
    })
}
