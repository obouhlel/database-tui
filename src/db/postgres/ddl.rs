//! DDL reconstruction from catalog data. Pure functions: the catalog queries
//! fetch the pieces (`pg_get_constraintdef`, `pg_get_indexdef`, ...), this
//! module assembles them, so the layout is unit-testable.

use std::fmt::Write;

use crate::db::schema::{Column, Constraint, Generated, Identity, Index, Trigger};
use crate::sql::{quote_ident, quote_literal, quote_qualified};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Table,
    Unlogged,
    Partitioned,
    Foreign,
}

/// Everything needed to rebuild a table's DDL.
#[derive(Debug, Clone)]
pub struct TableDdl<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    pub kind: TableKind,
    pub columns: &'a [Column],
    pub constraints: &'a [Constraint],
    pub indexes: &'a [Index],
    pub triggers: &'a [Trigger],
    /// `RANGE (taken_on)`
    pub partition_key: Option<&'a str>,
    /// `(schema, name, bound)` of each partition.
    pub partitions: &'a [(String, String, String)],
    /// Parent tables, already quoted.
    pub inherits: &'a [String],
    /// `fillfactor=70`
    pub reloptions: &'a [String],
    /// `(server, ["option=value", ...])` for foreign tables.
    pub foreign_server: Option<(&'a str, &'a [String])>,
    pub comment: Option<&'a str>,
}

pub fn table(d: &TableDdl) -> String {
    let qname = quote_qualified(d.schema, d.name);
    let mut out = String::new();
    let keyword = match d.kind {
        TableKind::Table | TableKind::Partitioned => "TABLE",
        TableKind::Unlogged => "UNLOGGED TABLE",
        TableKind::Foreign => "FOREIGN TABLE",
    };
    let _ = writeln!(out, "CREATE {keyword} {qname} (");

    let width = d
        .columns
        .iter()
        .map(|c| quote_ident(&c.name).chars().count())
        .max()
        .unwrap_or(0);
    let mut items: Vec<String> = d
        .columns
        .iter()
        .map(|c| format!("    {}", column_def(c, width)))
        .collect();
    items.extend(
        d.constraints
            .iter()
            // Constraint triggers are emitted as CREATE CONSTRAINT TRIGGER (in `triggers`).
            .filter(|c| c.kind != crate::db::schema::ConstraintKind::Trigger)
            .map(|c| format!("    CONSTRAINT {} {}", quote_ident(&c.name), c.definition)),
    );
    out.push_str(&items.join(",\n"));
    out.push_str("\n)");
    if !d.inherits.is_empty() {
        let _ = write!(out, "\nINHERITS ({})", d.inherits.join(", "));
    }
    if let Some(key) = d.partition_key {
        let _ = write!(out, "\nPARTITION BY {key}");
    }
    if let Some((server, options)) = d.foreign_server {
        let _ = write!(out, "\nSERVER {}", quote_ident(server));
        if !options.is_empty() {
            let _ = write!(out, "\nOPTIONS ({})", fdw_options(options));
        }
    }
    if !d.reloptions.is_empty() {
        let _ = write!(out, "\nWITH ({})", d.reloptions.join(", "));
    }
    out.push_str(";\n");

    for (schema, name, bound) in d.partitions {
        let _ = write!(
            out,
            "\nCREATE TABLE {} PARTITION OF {qname}\n    {bound};\n",
            quote_qualified(schema, name)
        );
    }
    append_indexes_triggers(&mut out, d.indexes, d.triggers);
    let target_kind = if d.kind == TableKind::Foreign {
        "FOREIGN TABLE"
    } else {
        "TABLE"
    };
    append_comments(&mut out, target_kind, &qname, d.comment, d.columns);
    out
}

/// `CREATE [MATERIALIZED] VIEW`.
pub fn view(
    schema: &str,
    name: &str,
    materialized: bool,
    definition: &str,
    columns: &[Column],
    indexes: &[Index],
    comment: Option<&str>,
) -> String {
    let qname = quote_qualified(schema, name);
    let body = definition.trim().trim_end_matches(';').trim_end();
    let mut out = if materialized {
        format!("CREATE MATERIALIZED VIEW {qname} AS\n{body}\nWITH DATA;\n")
    } else {
        format!("CREATE OR REPLACE VIEW {qname} AS\n{body};\n")
    };
    append_indexes_triggers(&mut out, indexes, &[]);
    let kind = if materialized {
        "MATERIALIZED VIEW"
    } else {
        "VIEW"
    };
    append_comments(&mut out, kind, &qname, comment, columns);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceDdl<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    pub data_type: &'a str,
    pub start: i64,
    pub increment: i64,
    pub min: i64,
    pub max: i64,
    pub cache: i64,
    pub cycle: bool,
    /// `schema.table.column`, already quoted.
    pub owned_by: Option<&'a str>,
    pub comment: Option<&'a str>,
}

pub fn sequence(s: &SequenceDdl) -> String {
    let qname = quote_qualified(s.schema, s.name);
    let mut out = format!(
        "CREATE SEQUENCE {qname}\n    AS {}\n    INCREMENT BY {}\n    MINVALUE {}\n    MAXVALUE {}\n    START WITH {}\n    CACHE {}\n    {};\n",
        s.data_type,
        s.increment,
        s.min,
        s.max,
        s.start,
        s.cache,
        if s.cycle { "CYCLE" } else { "NO CYCLE" }
    );
    if let Some(owner) = s.owned_by {
        let _ = write!(out, "\nALTER SEQUENCE {qname} OWNED BY {owner};\n");
    }
    append_comments(&mut out, "SEQUENCE", &qname, s.comment, &[]);
    out
}

/// `pg_get_functiondef` output plus its comment.
pub fn function(
    definition: &str,
    signature: &str,
    procedure: bool,
    comment: Option<&str>,
) -> String {
    let mut out = definition.trim_end().to_string();
    if !out.ends_with(';') {
        out.push(';');
    }
    out.push('\n');
    if let Some(c) = comment {
        let kind = if procedure { "PROCEDURE" } else { "FUNCTION" };
        let _ = write!(
            out,
            "\nCOMMENT ON {kind} {signature} IS {};\n",
            quote_literal(c)
        );
    }
    out
}

fn column_def(c: &Column, width: usize) -> String {
    let name = quote_ident(&c.name);
    let mut s = format!("{name:<width$} {}", c.data_type);
    if let Some(coll) = &c.collation {
        let _ = write!(s, " COLLATE {coll}");
    }
    match c.identity {
        Some(Identity::Always) => s.push_str(" GENERATED ALWAYS AS IDENTITY"),
        Some(Identity::ByDefault) => s.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
        None => {}
    }
    match (c.generated, &c.default) {
        (Some(g), Some(expr)) => {
            let kind = if g == Generated::Stored {
                "STORED"
            } else {
                "VIRTUAL"
            };
            let _ = write!(s, " GENERATED ALWAYS AS ({expr}) {kind}");
        }
        (None, Some(expr)) => {
            let _ = write!(s, " DEFAULT {expr}");
        }
        _ => {}
    }
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    s
}

fn append_indexes_triggers(out: &mut String, indexes: &[Index], triggers: &[Trigger]) {
    let standalone: Vec<_> = indexes.iter().filter(|i| !i.constraint_backed).collect();
    if !standalone.is_empty() {
        out.push('\n');
        for i in standalone {
            let _ = writeln!(out, "{};", i.definition);
        }
    }
    if !triggers.is_empty() {
        out.push('\n');
        for t in triggers {
            let _ = writeln!(out, "{};", t.definition);
        }
    }
}

fn append_comments(
    out: &mut String,
    kind: &str,
    qname: &str,
    comment: Option<&str>,
    columns: &[Column],
) {
    let column_comments: Vec<_> = columns.iter().filter(|c| c.comment.is_some()).collect();
    if comment.is_none() && column_comments.is_empty() {
        return;
    }
    out.push('\n');
    if let Some(c) = comment {
        let _ = writeln!(out, "COMMENT ON {kind} {qname} IS {};", quote_literal(c));
    }
    for c in column_comments {
        let _ = writeln!(
            out,
            "COMMENT ON COLUMN {qname}.{} IS {};",
            quote_ident(&c.name),
            quote_literal(c.comment.as_deref().unwrap_or_default())
        );
    }
}

/// `["schema_name=public", "table_name=x"]` -> `schema_name 'public', table_name 'x'`.
fn fdw_options(options: &[String]) -> String {
    options
        .iter()
        .map(|o| {
            let (k, v) = o.split_once('=').unwrap_or((o, ""));
            format!("{} {}", quote_ident(k), quote_literal(v))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::ConstraintKind;
    use pretty_assertions::assert_eq;

    fn col(name: &str, ty: &str, nullable: bool) -> Column {
        Column {
            name: name.into(),
            data_type: ty.into(),
            nullable,
            default: None,
            identity: None,
            generated: None,
            collation: None,
            comment: None,
            primary_key: false,
            references: None,
        }
    }

    fn base<'a>(
        columns: &'a [Column],
        constraints: &'a [Constraint],
        indexes: &'a [Index],
    ) -> TableDdl<'a> {
        TableDdl {
            schema: "shop",
            name: "orders",
            kind: TableKind::Table,
            columns,
            constraints,
            indexes,
            triggers: &[],
            partition_key: None,
            partitions: &[],
            inherits: &[],
            reloptions: &[],
            foreign_server: None,
            comment: None,
        }
    }

    #[test]
    fn full_table() {
        let mut id = col("id", "bigint", false);
        id.identity = Some(Identity::Always);
        let mut status = col("status", "shop.order_status", false);
        status.default = Some("'pending'::shop.order_status".into());
        let mut total = col("total", "numeric(12,2)", true);
        total.generated = Some(Generated::Stored);
        total.default = Some("(qty * price)".into());
        let mut user = col("user", "text", true);
        user.collation = Some("\"C\"".into());
        user.comment = Some("l'utilisateur".into());
        let columns = vec![id, status, total, user];
        let constraints = vec![
            Constraint {
                name: "orders_pkey".into(),
                kind: ConstraintKind::PrimaryKey,
                definition: "PRIMARY KEY (id)".into(),
                columns: vec!["id".into()],
                deferrable: false,
                initially_deferred: false,
            },
            Constraint {
                name: "orders_customer_id_fkey".into(),
                kind: ConstraintKind::ForeignKey,
                definition:
                    "FOREIGN KEY (customer_id) REFERENCES shop.customers(id) ON DELETE CASCADE"
                        .into(),
                columns: vec!["customer_id".into()],
                deferrable: false,
                initially_deferred: false,
            },
        ];
        let indexes = vec![
            Index {
                name: "orders_pkey".into(),
                method: "btree".into(),
                columns: vec!["id".into()],
                include: vec![],
                unique: true,
                primary: true,
                predicate: None,
                definition: "CREATE UNIQUE INDEX orders_pkey ON shop.orders USING btree (id)"
                    .into(),
                size_bytes: None,
                constraint_backed: true,
            },
            Index {
                name: "orders_status_idx".into(),
                method: "btree".into(),
                columns: vec!["status".into()],
                include: vec![],
                unique: false,
                primary: false,
                predicate: None,
                definition: "CREATE INDEX orders_status_idx ON shop.orders USING btree (status)"
                    .into(),
                size_bytes: None,
                constraint_backed: false,
            },
        ];
        let mut d = base(&columns, &constraints, &indexes);
        d.comment = Some("Commandes");
        assert_eq!(
            table(&d),
            "CREATE TABLE shop.orders (
    id     bigint GENERATED ALWAYS AS IDENTITY NOT NULL,
    status shop.order_status DEFAULT 'pending'::shop.order_status NOT NULL,
    total  numeric(12,2) GENERATED ALWAYS AS ((qty * price)) STORED,
    \"user\" text COLLATE \"C\",
    CONSTRAINT orders_pkey PRIMARY KEY (id),
    CONSTRAINT orders_customer_id_fkey FOREIGN KEY (customer_id) REFERENCES shop.customers(id) ON DELETE CASCADE
);

CREATE INDEX orders_status_idx ON shop.orders USING btree (status);

COMMENT ON TABLE shop.orders IS 'Commandes';
COMMENT ON COLUMN shop.orders.\"user\" IS 'l''utilisateur';
"
        );
    }

    #[test]
    fn partitioned_table() {
        let columns = vec![col("taken_on", "date", false)];
        let partitions = vec![(
            "shop".to_string(),
            "m_2025".to_string(),
            "FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')".to_string(),
        )];
        let mut d = base(&columns, &[], &[]);
        d.name = "Measurements";
        d.kind = TableKind::Partitioned;
        d.partition_key = Some("RANGE (taken_on)");
        d.partitions = &partitions;
        let reloptions = ["fillfactor=70".to_string()];
        d.reloptions = &reloptions;
        let ddl = table(&d);
        assert!(
            ddl.starts_with("CREATE TABLE shop.\"Measurements\" (\n"),
            "{ddl}"
        );
        assert!(
            ddl.contains(")\nPARTITION BY RANGE (taken_on)\nWITH (fillfactor=70);\n"),
            "{ddl}"
        );
        assert!(ddl.contains(
            "CREATE TABLE shop.m_2025 PARTITION OF shop.\"Measurements\"\n    FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');"
        ));
    }

    #[test]
    fn foreign_and_unlogged() {
        let columns = vec![col("a", "int", true)];
        let options = vec![
            "schema_name=public".to_string(),
            "table_name=it's".to_string(),
        ];
        let mut d = base(&columns, &[], &[]);
        d.kind = TableKind::Foreign;
        d.foreign_server = Some(("remote", &options));
        let ddl = table(&d);
        assert!(
            ddl.starts_with("CREATE FOREIGN TABLE shop.orders ("),
            "{ddl}"
        );
        assert!(
            ddl.contains("SERVER remote\nOPTIONS (schema_name 'public', table_name 'it''s');"),
            "{ddl}"
        );

        d.kind = TableKind::Unlogged;
        d.foreign_server = None;
        assert!(table(&d).starts_with("CREATE UNLOGGED TABLE"));
    }

    #[test]
    fn views() {
        assert_eq!(
            view("shop", "v", false, " SELECT 1 AS x;", &[], &[], Some("vue")),
            "CREATE OR REPLACE VIEW shop.v AS\nSELECT 1 AS x;\n\nCOMMENT ON VIEW shop.v IS 'vue';\n"
        );
        assert_eq!(
            view("shop", "mv", true, "SELECT 1", &[], &[], None),
            "CREATE MATERIALIZED VIEW shop.mv AS\nSELECT 1\nWITH DATA;\n"
        );
    }

    #[test]
    fn sequences_and_functions() {
        let s = SequenceDdl {
            schema: "shop",
            name: "seq",
            data_type: "bigint",
            start: 1000,
            increment: 1,
            min: 1,
            max: i64::MAX,
            cache: 1,
            cycle: false,
            owned_by: Some("shop.t.id"),
            comment: None,
        };
        let ddl = sequence(&s);
        assert!(ddl.contains("START WITH 1000\n"), "{ddl}");
        assert!(ddl.contains("NO CYCLE;"), "{ddl}");
        assert!(
            ddl.contains("ALTER SEQUENCE shop.seq OWNED BY shop.t.id;"),
            "{ddl}"
        );

        assert_eq!(
            function(
                "CREATE FUNCTION f() ...\n$$\n",
                "shop.f()",
                false,
                Some("x")
            ),
            "CREATE FUNCTION f() ...\n$$;\n\nCOMMENT ON FUNCTION shop.f() IS 'x';\n"
        );
    }
}
