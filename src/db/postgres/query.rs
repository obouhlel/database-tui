//! SQL generation for Data tab requests.
//!
//! Identifiers are quoted with [`quote_ident`]; every user value becomes a
//! `$n` parameter sent as text and cast server-side to the column type
//! (`CAST($1::text AS numeric(12,2))`), so PostgreSQL parses it — values
//! are never concatenated into the statement. The free `WHERE` clause is
//! raw SQL by design (the user writes SQL there).
//!
//! The table is aliased `t` and every column reference is qualified
//! (`t.id`): the select list returns `id::text` under the name `id`, and an
//! unqualified `ORDER BY id` would sort on that *text* output column.

use crate::db::data::{Condition, DataRequest, Filter, Op, SortDir};
use crate::sql::{quote_ident, quote_qualified};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sql {
    pub text: String,
    pub params: Vec<String>,
}

/// `SELECT col::text, ... FROM t WHERE ... ORDER BY ... LIMIT n OFFSET m`.
pub fn select(req: &DataRequest) -> Sql {
    let mut params = Vec::new();
    let mut cols: Vec<String> = req
        .columns
        .iter()
        .map(|c| format!("t.{}::text", quote_ident(&c.name)))
        .collect();
    if req.with_row_version {
        cols.push("t.xmin::text".into());
    }
    let mut text = format!(
        "SELECT {} FROM {} AS t",
        cols.join(", "),
        quote_qualified(&req.object.schema, &req.object.name)
    );
    push_where(&mut text, &mut params, req);
    let order = order_by(req);
    if !order.is_empty() {
        text.push_str(" ORDER BY ");
        text.push_str(&order.join(", "));
    }
    text.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
    Sql { text, params }
}

/// Bounded count: stops after `bound + 1` rows so that huge tables stay cheap.
pub fn count(req: &DataRequest, bound: u64) -> Sql {
    let mut params = Vec::new();
    let mut inner = format!(
        "SELECT 1 FROM {} AS t",
        quote_qualified(&req.object.schema, &req.object.name)
    );
    push_where(&mut inner, &mut params, req);
    Sql {
        text: format!("SELECT count(*) FROM ({inner} LIMIT {}) AS c", bound + 1),
        params,
    }
}

fn push_where(text: &mut String, params: &mut Vec<String>, req: &DataRequest) {
    if let Some(clause) = where_clause(&req.filter, req, params) {
        text.push_str(" WHERE ");
        text.push_str(&clause);
    }
}

fn where_clause(filter: &Filter, req: &DataRequest, params: &mut Vec<String>) -> Option<String> {
    let mut parts = Vec::new();
    let conds: Vec<String> = filter
        .conditions
        .iter()
        .map(|c| condition(c, req, params))
        .collect();
    if !conds.is_empty() {
        let joined = conds.join(&format!(" {} ", filter.combinator.keyword()));
        parts.push(if conds.len() > 1 && filter.free_where.is_some() {
            format!("({joined})")
        } else {
            joined
        });
    }
    if let Some(free) = filter
        .free_where
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        parts.push(format!("({free})"));
    }
    (!parts.is_empty()).then(|| parts.join(" AND "))
}

fn condition(c: &Condition, req: &DataRequest, params: &mut Vec<String>) -> String {
    let col = format!("t.{}", quote_ident(&c.column));
    if !c.op.has_value() {
        return format!("{col} {}", c.op.label());
    }
    params.push(c.value.clone());
    let p = format!("${}", params.len());
    if c.op.is_textual() {
        // Patterns apply to the text representation, whatever the type.
        return format!("{col}::text {} {p}", c.op.label());
    }
    let ty = req
        .columns
        .iter()
        .find(|col| col.name == c.column)
        .map_or("text", |col| col.data_type.as_str());
    let op = if c.op == Op::Ne { "<>" } else { c.op.label() };
    format!("{col} {op} CAST({p}::text AS {ty})")
}

fn order_by(req: &DataRequest) -> Vec<String> {
    let mut items: Vec<String> = req
        .sort
        .iter()
        .map(|k| {
            let dir = match k.dir {
                SortDir::Asc => "ASC",
                SortDir::Desc => "DESC",
            };
            format!("t.{} {dir}", quote_ident(&k.column))
        })
        .collect();
    for t in &req.tiebreak {
        if !req.sort.iter().any(|k| &k.column == t) {
            items.push(format!("t.{} ASC", quote_ident(t)));
        }
    }
    if items.is_empty() && req.order_by_ctid {
        items.push("t.ctid".into());
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::catalog::{ObjectKind, ObjectRef};
    use crate::db::data::{ColumnSpec, Combinator, SortKey};
    use pretty_assertions::assert_eq;

    fn req() -> DataRequest {
        DataRequest {
            object: ObjectRef {
                database: "demo".into(),
                schema: "shop".into(),
                name: "Order Items".into(),
                kind: ObjectKind::Table,
                signature: None,
            },
            columns: vec![
                ColumnSpec {
                    name: "id".into(),
                    data_type: "bigint".into(),
                },
                ColumnSpec {
                    name: "user".into(),
                    data_type: "text".into(),
                },
                ColumnSpec {
                    name: "total".into(),
                    data_type: "numeric(12,2)".into(),
                },
            ],
            filter: Filter::default(),
            sort: vec![],
            tiebreak: vec!["id".into()],
            order_by_ctid: false,
            with_row_version: true,
            offset: 400,
            limit: 200,
        }
    }

    fn cond(column: &str, op: Op, value: &str) -> Condition {
        Condition {
            column: column.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn plain_select_quotes_identifiers() {
        let sql = select(&req());
        assert_eq!(
            sql.text,
            "SELECT t.id::text, t.\"user\"::text, t.total::text, t.xmin::text FROM shop.\"Order Items\" AS t \
             ORDER BY t.id ASC LIMIT 200 OFFSET 400"
        );
        assert!(sql.params.is_empty());
    }

    #[test]
    fn conditions_are_parameters_cast_to_the_column_type() {
        let mut r = req();
        r.filter.conditions = vec![
            cond("total", Op::Gt, "100.5"),
            cond("user", Op::ILike, "%zoé%"),
            cond("id", Op::Ne, "3"),
            cond("user", Op::IsNull, "ignored"),
        ];
        let sql = select(&r);
        assert!(
            sql.text.contains(
                "WHERE t.total > CAST($1::text AS numeric(12,2)) AND t.\"user\"::text ILIKE $2 \
                 AND t.id <> CAST($3::text AS bigint) AND t.\"user\" IS NULL ORDER BY"
            ),
            "{}",
            sql.text
        );
        assert_eq!(sql.params, vec!["100.5", "%zoé%", "3"]);
    }

    #[test]
    fn values_are_never_concatenated() {
        let mut r = req();
        let evil = "x'; DROP TABLE shop.orders; --";
        r.filter.conditions = vec![cond("user", Op::Eq, evil), cond("user", Op::Regex, evil)];
        let sql = select(&r);
        assert!(!sql.text.contains("DROP"), "{}", sql.text);
        assert_eq!(sql.params, vec![evil, evil]);
    }

    #[test]
    fn or_combinator_and_free_where() {
        let mut r = req();
        r.filter = Filter {
            conditions: vec![cond("id", Op::Lt, "10"), cond("id", Op::Gt, "90")],
            combinator: Combinator::Or,
            free_where: Some(" total > 0 ".into()),
        };
        let sql = select(&r);
        assert!(
            sql.text.contains(
                "WHERE (t.id < CAST($1::text AS bigint) OR t.id > CAST($2::text AS bigint)) AND (total > 0)"
            ),
            "{}",
            sql.text
        );
        r.filter.conditions.clear();
        assert!(select(&r).text.contains("WHERE (total > 0) ORDER BY"));
        r.filter.free_where = Some("   ".into());
        assert!(!select(&r).text.contains("WHERE"));
    }

    #[test]
    fn multi_sort_with_tiebreak() {
        let mut r = req();
        r.sort = vec![
            SortKey {
                column: "total".into(),
                dir: SortDir::Desc,
            },
            SortKey {
                column: "user".into(),
                dir: SortDir::Asc,
            },
        ];
        assert!(
            select(&r)
                .text
                .contains("ORDER BY t.total DESC, t.\"user\" ASC, t.id ASC LIMIT")
        );
        // The tiebreak column is not repeated.
        r.sort = vec![SortKey {
            column: "id".into(),
            dir: SortDir::Desc,
        }];
        assert!(select(&r).text.contains("ORDER BY t.id DESC LIMIT"));
    }

    #[test]
    fn ctid_order_without_key() {
        let mut r = req();
        r.tiebreak.clear();
        r.with_row_version = false;
        assert!(
            select(&r)
                .text
                .ends_with("FROM shop.\"Order Items\" AS t LIMIT 200 OFFSET 400")
        );
        r.order_by_ctid = true;
        assert!(select(&r).text.contains("ORDER BY t.ctid LIMIT"));
    }

    /// Regression: `SELECT id::text ... ORDER BY id` sorted on the text
    /// output column (1, 10, 100, ...). Every reference must be qualified.
    #[test]
    fn order_by_targets_the_table_column() {
        let mut r = req();
        r.sort = vec![SortKey {
            column: "total".into(),
            dir: SortDir::Desc,
        }];
        let text = select(&r).text;
        let order = &text[text.find("ORDER BY").unwrap()..];
        assert!(
            !order.contains(" total ") && order.contains("t.total DESC"),
            "{order}"
        );
    }

    #[test]
    fn bounded_count() {
        let mut r = req();
        r.filter.conditions = vec![cond("total", Op::Ge, "5")];
        let sql = count(&r, 1_000_000);
        assert_eq!(
            sql.text,
            "SELECT count(*) FROM (SELECT 1 FROM shop.\"Order Items\" AS t \
             WHERE t.total >= CAST($1::text AS numeric(12,2)) LIMIT 1000001) AS c"
        );
        assert_eq!(sql.params, vec!["5"]);
    }
}
