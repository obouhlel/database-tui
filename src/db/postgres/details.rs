//! Catalog queries for the Schema tab.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use tokio_postgres::Client;
use tokio_postgres::types::{Oid, ToSql};

use super::ddl::{self, SequenceDdl, TableDdl, TableKind};
use crate::db::catalog::{ObjectKind, ObjectRef};
use crate::db::schema::{
    Column, Constraint, ConstraintKind, FkAction, ForeignKey, Generated, Identity, Index,
    ObjectDetails, Stats, Trigger,
};
use crate::sql::quote_qualified;

type Params<'a> = &'a [&'a (dyn ToSql + Sync)];

pub async fn load(client: &Client, obj: &ObjectRef) -> Result<ObjectDetails> {
    match obj.kind {
        ObjectKind::Sequence => sequence(client, obj).await,
        ObjectKind::Function | ObjectKind::Procedure => function(client, obj).await,
        _ => relation(client, obj).await,
    }
}

// ----------------------------------------------------------------- relations

const RELATION_SQL: &str = "
SELECT c.oid, c.relkind::text, pg_get_userbyid(c.relowner)::text, obj_description(c.oid, 'pg_class'),
       c.relpersistence::text,
       CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END,
       coalesce(c.reloptions, '{}'::text[]),
       ARRAY(SELECT quote_ident(pn.nspname) || '.' || quote_ident(pc.relname)
             FROM pg_inherits i
             JOIN pg_class pc ON pc.oid = i.inhparent
             JOIN pg_namespace pn ON pn.oid = pc.relnamespace
             WHERE i.inhrelid = c.oid AND NOT c.relispartition
             ORDER BY i.inhseqno),
       c.relispopulated,
       (SELECT spcname::text FROM pg_tablespace WHERE oid = c.reltablespace)
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1 AND c.relname = $2";

const COLUMNS_SQL: &str = "
SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull,
       pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text,
       col_description(a.attrelid, a.attnum),
       CASE WHEN a.attcollation <> 0 AND a.attcollation <> t.typcollation
            THEN quote_ident(cn.nspname) || '.' || quote_ident(co.collname) END
FROM pg_attribute a
JOIN pg_type t ON t.oid = a.atttypid
LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
LEFT JOIN pg_collation co ON co.oid = a.attcollation
LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace
WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum";

/// NOT NULL constraints (`contype = 'n'`, PostgreSQL 18) are shown on columns.
const CONSTRAINTS_SQL: &str = "
SELECT con.conname::text, con.contype::text, pg_get_constraintdef(con.oid, true),
       ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, i)
             JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.i),
       con.condeferrable, con.condeferred
FROM pg_constraint con
WHERE con.conrelid = $1 AND con.contype IN ('p', 'u', 'f', 'c', 'x', 't')
ORDER BY CASE con.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 WHEN 'c' THEN 3 ELSE 4 END,
         con.conname";

/// `conparentid = 0` skips the per-partition copies of a partitioned FK.
const FKS_SQL: &str = "
SELECT con.conname::text,
       sn.nspname::text, sc.relname::text, sc.relkind::text,
       ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, i)
             JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.i),
       tn.nspname::text, tc.relname::text, tc.relkind::text,
       ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, i)
             JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.i),
       con.confupdtype::text, con.confdeltype::text
FROM pg_constraint con
JOIN pg_class sc ON sc.oid = con.conrelid
JOIN pg_namespace sn ON sn.oid = sc.relnamespace
JOIN pg_class tc ON tc.oid = con.confrelid
JOIN pg_namespace tn ON tn.oid = tc.relnamespace
WHERE con.contype = 'f' AND con.conparentid = 0 AND (con.conrelid = $1 OR con.confrelid = $1)
ORDER BY con.conname";

const INDEXES_SQL: &str = "
SELECT ic.relname::text, am.amname::text, i.indisunique, i.indisprimary,
       pg_get_indexdef(i.indexrelid),
       pg_get_expr(i.indpred, i.indrelid),
       pg_relation_size(i.indexrelid),
       EXISTS (SELECT 1 FROM pg_constraint c
               WHERE c.conindid = i.indexrelid AND c.conrelid = i.indrelid AND c.contype IN ('p', 'u', 'x'))
FROM pg_index i
JOIN pg_class ic ON ic.oid = i.indexrelid
JOIN pg_am am ON am.oid = ic.relam
WHERE i.indrelid = $1
ORDER BY i.indisprimary DESC, ic.relname";

const TRIGGERS_SQL: &str = "
SELECT t.tgname::text, pg_get_triggerdef(t.oid, true), t.tgenabled::text <> 'D',
       CASE WHEN t.tgtype & 2 = 2 THEN 'BEFORE' WHEN t.tgtype & 64 = 64 THEN 'INSTEAD OF' ELSE 'AFTER' END,
       array_to_string(array_remove(ARRAY[
           CASE WHEN t.tgtype & 4 = 4 THEN 'INSERT' END,
           CASE WHEN t.tgtype & 16 = 16 THEN 'UPDATE' END,
           CASE WHEN t.tgtype & 8 = 8 THEN 'DELETE' END,
           CASE WHEN t.tgtype & 32 = 32 THEN 'TRUNCATE' END], NULL), ' OR '),
       CASE WHEN t.tgtype & 1 = 1 THEN 'ROW' ELSE 'STATEMENT' END,
       t.tgfoid::regproc::text
FROM pg_trigger t
WHERE t.tgrelid = $1 AND NOT t.tgisinternal
ORDER BY t.tgname";

const STATS_SQL: &str = "
SELECT c.reltuples::float8,
       pg_total_relation_size(c.oid), pg_relation_size(c.oid), pg_indexes_size(c.oid),
       coalesce(pg_total_relation_size(nullif(c.reltoastrelid, 0)), 0),
       s.n_live_tup, s.n_dead_tup, s.seq_scan, s.idx_scan,
       s.last_vacuum, s.last_autovacuum, s.last_analyze, s.last_autoanalyze
FROM pg_class c
LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
WHERE c.oid = $1";

/// For partitioned tables: totals over the leaf partitions.
const PARTITIONED_STATS_SQL: &str = "
SELECT sum(greatest(c.reltuples, 0))::float8, bool_and(c.reltuples < 0),
       coalesce(sum(pg_total_relation_size(c.oid)), 0)::int8,
       coalesce(sum(pg_relation_size(c.oid)), 0)::int8,
       coalesce(sum(pg_indexes_size(c.oid)), 0)::int8,
       coalesce(sum(coalesce(pg_total_relation_size(nullif(c.reltoastrelid, 0)), 0)), 0)::int8
FROM pg_partition_tree($1::oid::regclass) t
JOIN pg_class c ON c.oid = t.relid
WHERE t.isleaf";

const PARTITIONS_SQL: &str = "
SELECT n.nspname::text, c.relname::text, pg_get_expr(c.relpartbound, c.oid)
FROM pg_inherits i
JOIN pg_class c ON c.oid = i.inhrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE i.inhparent = $1
ORDER BY c.relname";

const FOREIGN_SQL: &str = "
SELECT s.srvname::text, coalesce(ft.ftoptions, '{}'::text[])
FROM pg_foreign_table ft JOIN pg_foreign_server s ON s.oid = ft.ftserver
WHERE ft.ftrelid = $1";

async fn relation(client: &Client, obj: &ObjectRef) -> Result<ObjectDetails> {
    let row = client
        .query_opt(RELATION_SQL, &[&obj.schema, &obj.name])
        .await
        .context("loading relation")?
        .ok_or_else(|| anyhow!("{} no longer exists", obj.qualified()))?;
    let oid: Oid = row.get(0);
    let relkind: String = row.get(1);
    let owner: Option<String> = row.get(2);
    let comment: Option<String> = row.get(3);
    let persistence: String = row.get(4);
    let partition_key: Option<String> = row.get(5);
    let reloptions: Vec<String> = row.get(6);
    let inherits: Vec<String> = row.get(7);
    let populated: bool = row.get(8);
    let tablespace: Option<String> = row.get(9);

    let p: Params = &[&oid];
    let (col_rows, con_rows, fk_rows, idx_rows, trg_rows, stat_rows) = tokio::try_join!(
        client.query(COLUMNS_SQL, p),
        client.query(CONSTRAINTS_SQL, p),
        client.query(FKS_SQL, p),
        client.query(INDEXES_SQL, p),
        client.query(TRIGGERS_SQL, p),
        client.query(STATS_SQL, p),
    )
    .context("loading relation details")?;

    let constraints: Vec<Constraint> = con_rows
        .iter()
        .filter_map(|r| {
            Some(Constraint {
                name: r.get(0),
                kind: ConstraintKind::from_pg(&r.get::<_, String>(1))?,
                definition: r.get(2),
                columns: r.get(3),
                deferrable: r.get(4),
                initially_deferred: r.get(5),
            })
        })
        .collect();

    let mut foreign_keys = Vec::new();
    let mut referenced_by = Vec::new();
    for r in &fk_rows {
        let fk = ForeignKey {
            name: r.get(0),
            from: relation_ref(obj, r.get(1), r.get(2), &r.get::<_, String>(3)),
            from_columns: r.get(4),
            to: relation_ref(obj, r.get(5), r.get(6), &r.get::<_, String>(7)),
            to_columns: r.get(8),
            on_update: FkAction::from_pg(&r.get::<_, String>(9)),
            on_delete: FkAction::from_pg(&r.get::<_, String>(10)),
        };
        let outgoing = fk.from.schema == obj.schema && fk.from.name == obj.name;
        let incoming = fk.to.schema == obj.schema && fk.to.name == obj.name;
        // A self-reference is both.
        if incoming {
            referenced_by.push(fk.clone());
        }
        if outgoing {
            foreign_keys.push(fk);
        }
    }

    let pk_columns: Vec<String> = constraints
        .iter()
        .filter(|c| c.kind == ConstraintKind::PrimaryKey)
        .flat_map(|c| c.columns.clone())
        .collect();
    let columns: Vec<Column> = col_rows
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let generated = match r.get::<_, String>(5).as_str() {
                "s" => Some(Generated::Stored),
                "v" => Some(Generated::Virtual),
                _ => None,
            };
            let references = foreign_keys
                .iter()
                .find(|fk| fk.from_columns.len() == 1 && fk.from_columns[0] == name)
                .map(|fk| format!("{}({})", fk.to.qualified(), fk.to_columns.join(", ")));
            Column {
                primary_key: pk_columns.contains(&name),
                name,
                data_type: r.get(1),
                nullable: r.get(2),
                default: r.get(3),
                identity: match r.get::<_, String>(4).as_str() {
                    "a" => Some(Identity::Always),
                    "d" => Some(Identity::ByDefault),
                    _ => None,
                },
                generated,
                comment: r.get(6),
                collation: r.get(7),
                references,
            }
        })
        .collect();

    let indexes: Vec<Index> = idx_rows
        .iter()
        .map(|r| {
            let definition: String = r.get(4);
            let (columns, include) = index_columns(&definition);
            Index {
                name: r.get(0),
                method: r.get(1),
                unique: r.get(2),
                primary: r.get(3),
                predicate: r.get(5),
                size_bytes: Some(r.get(6)),
                constraint_backed: r.get(7),
                columns,
                include,
                definition,
            }
        })
        .collect();

    let triggers: Vec<Trigger> = trg_rows
        .iter()
        .map(|r| Trigger {
            name: r.get(0),
            definition: r.get(1),
            enabled: r.get(2),
            timing: r.get(3),
            events: r.get(4),
            level: r.get(5),
            function: r.get(6),
        })
        .collect();

    let kind = obj.kind;
    let mut stats = stat_rows.first().map(|r| {
        let reltuples: f64 = r.get(0);
        Stats {
            estimated_rows: (reltuples >= 0.0).then_some(reltuples as i64),
            total_bytes: r.get(1),
            table_bytes: r.get(2),
            index_bytes: r.get(3),
            toast_bytes: r.get(4),
            live_tuples: r.get(5),
            dead_tuples: r.get(6),
            seq_scans: r.get(7),
            index_scans: r.get(8),
            last_vacuum: r.get::<_, Option<DateTime<Utc>>>(9),
            last_autovacuum: r.get(10),
            last_analyze: r.get(11),
            last_autoanalyze: r.get(12),
        }
    });

    let mut properties = Vec::new();
    if let Some(ts) = &tablespace {
        properties.push(("Tablespace".into(), ts.clone()));
    }
    if !reloptions.is_empty() {
        properties.push(("Options".into(), reloptions.join(", ")));
    }
    if !inherits.is_empty() {
        properties.push(("Hérite de".into(), inherits.join(", ")));
    }

    let ddl = match kind {
        ObjectKind::View | ObjectKind::MaterializedView => {
            let def: String = client
                .query_one("SELECT pg_get_viewdef($1::oid, true)", &[&oid])
                .await
                .context("loading view definition")?
                .get(0);
            let materialized = kind == ObjectKind::MaterializedView;
            if materialized {
                properties.push((
                    "Peuplée".into(),
                    if populated { "oui" } else { "non" }.into(),
                ));
            } else {
                // Views have no storage.
                stats = None;
            }
            ddl::view(
                &obj.schema,
                &obj.name,
                materialized,
                &def,
                &columns,
                &indexes,
                comment.as_deref(),
            )
        }
        _ => {
            let mut partitions = Vec::new();
            let mut foreign = None;
            let table_kind = match relkind.as_str() {
                "p" => {
                    partitions = client
                        .query(PARTITIONS_SQL, p)
                        .await
                        .context("loading partitions")?
                        .iter()
                        .map(|r| (r.get(0), r.get(1), r.get(2)))
                        .collect::<Vec<(String, String, String)>>();
                    let r = client
                        .query_one(PARTITIONED_STATS_SQL, p)
                        .await
                        .context("partition sizes")?;
                    let rows: Option<f64> = r.get(0);
                    let never_analyzed: Option<bool> = r.get(1);
                    stats = Some(Stats {
                        estimated_rows: match (rows, never_analyzed) {
                            (Some(n), Some(false)) => Some(n as i64),
                            _ => None,
                        },
                        total_bytes: r.get(2),
                        table_bytes: r.get(3),
                        index_bytes: r.get(4),
                        toast_bytes: r.get(5),
                        ..Stats::default()
                    });
                    if let Some(key) = &partition_key {
                        properties.push(("Partitionnement".into(), key.clone()));
                    }
                    properties.push(("Partitions".into(), partitions.len().to_string()));
                    TableKind::Partitioned
                }
                "f" => {
                    if let Some(r) = client
                        .query_opt(FOREIGN_SQL, p)
                        .await
                        .context("foreign table")?
                    {
                        let server: String = r.get(0);
                        properties.push(("Serveur".into(), server.clone()));
                        foreign = Some((server, r.get::<_, Vec<String>>(1)));
                    }
                    TableKind::Foreign
                }
                _ if persistence == "u" => TableKind::Unlogged,
                _ => TableKind::Table,
            };
            ddl::table(&TableDdl {
                schema: &obj.schema,
                name: &obj.name,
                kind: table_kind,
                columns: &columns,
                constraints: &constraints,
                indexes: &indexes,
                triggers: &triggers,
                partition_key: partition_key.as_deref(),
                partitions: &partitions,
                inherits: &inherits,
                reloptions: &reloptions,
                foreign_server: foreign.as_ref().map(|(s, o)| (s.as_str(), o.as_slice())),
                comment: comment.as_deref(),
            })
        }
    };
    if persistence == "u" {
        properties.push(("Persistance".into(), "UNLOGGED (non journalisée)".into()));
    }

    Ok(ObjectDetails {
        object: obj.clone(),
        owner,
        comment,
        columns,
        indexes,
        constraints,
        triggers,
        foreign_keys,
        referenced_by,
        stats,
        properties,
        ddl,
    })
}

/// Key and INCLUDE columns from a `pg_get_indexdef` statement, keeping
/// ordering and operator classes (`ordered_at DESC`, `attributes jsonb_path_ops`).
/// `pg_get_indexdef(oid, column, ...)` returns the bare expression only.
fn index_columns(def: &str) -> (Vec<String>, Vec<String>) {
    let Some(using) = def.find(" USING ") else {
        return (Vec::new(), Vec::new());
    };
    let rest = &def[using..];
    let Some((keys, after)) = rest.find('(').and_then(|i| parenthesized(&rest[i..])) else {
        return (Vec::new(), Vec::new());
    };
    let include = after
        .trim_start()
        .strip_prefix("INCLUDE ")
        .and_then(|s| parenthesized(s.trim_start()))
        .map(|(inner, _)| split_top_level(inner))
        .unwrap_or_default();
    (split_top_level(keys), include)
}

/// For `s` starting with `(`: the content up to the matching `)`, and the rest.
fn parenthesized(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits on commas that are not nested in parentheses or quotes.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        parts.push(last.to_string());
    }
    parts
}

fn relation_ref(origin: &ObjectRef, schema: String, name: String, relkind: &str) -> ObjectRef {
    let kind = match relkind {
        "p" => ObjectKind::PartitionedTable,
        "f" => ObjectKind::ForeignTable,
        "v" => ObjectKind::View,
        "m" => ObjectKind::MaterializedView,
        _ => ObjectKind::Table,
    };
    ObjectRef {
        connection: origin.connection.clone(),
        database: origin.database.clone(),
        schema,
        name,
        kind,
        signature: None,
    }
}

// ----------------------------------------------------------------- sequences

const SEQUENCE_SQL: &str = "
SELECT format_type(s.seqtypid, NULL), s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle,
       (SELECT ps.last_value FROM pg_sequences ps WHERE ps.schemaname = n.nspname AND ps.sequencename = c.relname),
       pg_get_userbyid(c.relowner)::text, obj_description(c.oid, 'pg_class'),
       (SELECT quote_ident(tn.nspname) || '.' || quote_ident(tc.relname) || '.' || quote_ident(a.attname)
        FROM pg_depend d
        JOIN pg_class tc ON tc.oid = d.refobjid
        JOIN pg_namespace tn ON tn.oid = tc.relnamespace
        JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
        WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype IN ('a', 'i')
        LIMIT 1)
FROM pg_sequence s
JOIN pg_class c ON c.oid = s.seqrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1 AND c.relname = $2";

async fn sequence(client: &Client, obj: &ObjectRef) -> Result<ObjectDetails> {
    let r = client
        .query_opt(SEQUENCE_SQL, &[&obj.schema, &obj.name])
        .await
        .context("loading sequence")?
        .ok_or_else(|| anyhow!("{} no longer exists", obj.qualified()))?;
    let data_type: String = r.get(0);
    let (start, increment, min, max, cache): (i64, i64, i64, i64, i64) =
        (r.get(1), r.get(2), r.get(3), r.get(4), r.get(5));
    let cycle: bool = r.get(6);
    let last_value: Option<i64> = r.get(7);
    let owner: Option<String> = r.get(8);
    let comment: Option<String> = r.get(9);
    let owned_by: Option<String> = r.get(10);

    let mut properties = vec![
        ("Type".to_string(), data_type.clone()),
        (
            "Valeur courante".into(),
            last_value.map_or_else(|| "jamais utilisée".into(), |v| v.to_string()),
        ),
        ("Début".into(), start.to_string()),
        ("Incrément".into(), increment.to_string()),
        ("Minimum".into(), min.to_string()),
        ("Maximum".into(), max.to_string()),
        ("Cache".into(), cache.to_string()),
        ("Cyclique".into(), if cycle { "oui" } else { "non" }.into()),
    ];
    if let Some(o) = &owned_by {
        properties.push(("Appartient à".into(), o.clone()));
    }
    let ddl = ddl::sequence(&SequenceDdl {
        schema: &obj.schema,
        name: &obj.name,
        data_type: &data_type,
        start,
        increment,
        min,
        max,
        cache,
        cycle,
        owned_by: owned_by.as_deref(),
        comment: comment.as_deref(),
    });
    Ok(ObjectDetails {
        object: obj.clone(),
        owner,
        comment,
        columns: Vec::new(),
        indexes: Vec::new(),
        constraints: Vec::new(),
        triggers: Vec::new(),
        foreign_keys: Vec::new(),
        referenced_by: Vec::new(),
        stats: None,
        properties,
        ddl,
    })
}

// ----------------------------------------------------------------- functions

const FUNCTION_SQL: &str = "
SELECT pg_get_functiondef(p.oid), l.lanname::text, pg_get_function_result(p.oid),
       p.provolatile::text, p.prosecdef, p.proisstrict, p.proparallel::text,
       pg_get_userbyid(p.proowner)::text, obj_description(p.oid, 'pg_proc'),
       pg_get_function_arguments(p.oid)
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
JOIN pg_language l ON l.oid = p.prolang
WHERE n.nspname = $1 AND p.proname = $2 AND pg_get_function_identity_arguments(p.oid) = $3";

async fn function(client: &Client, obj: &ObjectRef) -> Result<ObjectDetails> {
    let signature = obj.signature.clone().unwrap_or_default();
    let r = client
        .query_opt(FUNCTION_SQL, &[&obj.schema, &obj.name, &signature])
        .await
        .context("loading function")?
        .ok_or_else(|| anyhow!("{}({signature}) no longer exists", obj.qualified()))?;
    let definition: String = r.get(0);
    let language: String = r.get(1);
    let result: Option<String> = r.get(2);
    let volatility = match r.get::<_, String>(3).as_str() {
        "i" => "IMMUTABLE",
        "s" => "STABLE",
        _ => "VOLATILE",
    };
    let security_definer: bool = r.get(4);
    let strict: bool = r.get(5);
    let parallel = match r.get::<_, String>(6).as_str() {
        "s" => "SAFE",
        "r" => "RESTRICTED",
        _ => "UNSAFE",
    };
    let owner: Option<String> = r.get(7);
    let comment: Option<String> = r.get(8);
    let arguments: String = r.get(9);

    let procedure = obj.kind == ObjectKind::Procedure;
    let mut properties = vec![
        ("Langage".to_string(), language),
        (
            "Arguments".into(),
            if arguments.is_empty() {
                "—".into()
            } else {
                arguments
            },
        ),
    ];
    if let Some(res) = result.filter(|_| !procedure) {
        properties.push(("Retourne".into(), res));
    }
    if !procedure {
        properties.push(("Volatilité".into(), volatility.into()));
        properties.push(("Parallélisme".into(), parallel.into()));
        properties.push(("Strict".into(), if strict { "oui" } else { "non" }.into()));
    }
    properties.push((
        "Sécurité".into(),
        if security_definer {
            "DEFINER"
        } else {
            "INVOKER"
        }
        .into(),
    ));

    let sig = format!("{}({signature})", quote_qualified(&obj.schema, &obj.name));
    let ddl = ddl::function(&definition, &sig, procedure, comment.as_deref());
    Ok(ObjectDetails {
        object: obj.clone(),
        owner,
        comment,
        columns: Vec::new(),
        indexes: Vec::new(),
        constraints: Vec::new(),
        triggers: Vec::new(),
        foreign_keys: Vec::new(),
        referenced_by: Vec::new(),
        stats: None,
        properties,
        ddl,
    })
}

#[cfg(test)]
mod tests {
    use super::index_columns;

    #[test]
    fn index_column_parsing() {
        assert_eq!(
            index_columns("CREATE INDEX i ON shop.orders USING btree (status, ordered_at DESC)").0,
            ["status", "ordered_at DESC"]
        );
        assert_eq!(
            index_columns("CREATE INDEX i ON t USING gin (attributes jsonb_path_ops)").0,
            ["attributes jsonb_path_ops"]
        );
        assert_eq!(
            index_columns(
                "CREATE UNIQUE INDEX i ON t USING btree (lower(email), coalesce(a, 'x,y')) INCLUDE (b, \"C,d\") WHERE (a > 0)"
            ),
            (
                vec!["lower(email)".to_string(), "coalesce(a, 'x,y')".to_string()],
                vec!["b".to_string(), "\"C,d\"".to_string()]
            )
        );
        assert_eq!(index_columns("garbage"), (vec![], vec![]));
    }
}
