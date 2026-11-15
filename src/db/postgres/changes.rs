//! SQL for staged changes.
//!
//! Executed statements use `$n` parameters sent as text and cast to the
//! column type (`CAST($1::text AS integer)`): values are never concatenated.
//! [`preview`] renders the same statements with quoted literals, for the
//! "SQL généré" view only.
//!
//! Conflict detection: an `UPDATE` repeats the old text value of every
//! changed column in its `WHERE` (`t.col::text = $n`, or `IS NULL`), a
//! `DELETE` the row version (`xmin`) read with the row. Zero affected rows
//! means the row changed in between.

use crate::db::changes::{Change, ChangeSet, NewValue};
use crate::sql::{quote_ident, quote_literal, quote_qualified};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub text: String,
    pub params: Vec<Option<String>>,
}

/// How values are written: parameters (execution) or literals (preview).
enum Values {
    Params(Vec<Option<String>>),
    Literals,
}

impl Values {
    /// A value of type `ty`.
    fn typed(&mut self, value: &str, ty: &str) -> String {
        match self {
            Values::Params(p) => {
                p.push(Some(value.to_string()));
                format!("CAST(${}::text AS {ty})", p.len())
            }
            Values::Literals => format!("{}::{ty}", quote_literal(value)),
        }
    }

    /// A text value (compared to `col::text`).
    fn text(&mut self, value: &str) -> String {
        match self {
            Values::Params(p) => {
                p.push(Some(value.to_string()));
                format!("${}", p.len())
            }
            Values::Literals => quote_literal(value),
        }
    }

    fn new_value(&mut self, v: &NewValue, ty: &str) -> String {
        match v {
            NewValue::Null => "NULL".into(),
            NewValue::Default => "DEFAULT".into(),
            NewValue::Text(s) => self.typed(s, ty),
        }
    }
}

fn build(set: &ChangeSet, change: &Change, values: &mut Values) -> String {
    let table = quote_qualified(&set.object.schema, &set.object.name);
    let key_cond = |values: &mut Values, key: &[(String, String)]| -> Vec<String> {
        key.iter()
            .map(|(c, v)| {
                format!(
                    "t.{} = {}",
                    quote_ident(c),
                    values.typed(v, set.data_type(c))
                )
            })
            .collect()
    };
    match change {
        Change::Update {
            key,
            set: cols,
            expect,
        } => {
            let assignments: Vec<String> = cols
                .iter()
                .map(|(c, v)| {
                    format!(
                        "{} = {}",
                        quote_ident(c),
                        values.new_value(v, set.data_type(c))
                    )
                })
                .collect();
            let mut conds = key_cond(values, key);
            for (c, old) in expect {
                conds.push(match old {
                    None => format!("t.{} IS NULL", quote_ident(c)),
                    Some(v) => format!("t.{}::text = {}", quote_ident(c), values.text(v)),
                });
            }
            format!(
                "UPDATE {table} AS t SET {} WHERE {}",
                assignments.join(", "),
                conds.join(" AND ")
            )
        }
        Change::Insert { values: cols } => {
            let cols: Vec<&(String, NewValue)> = cols
                .iter()
                .filter(|(_, v)| *v != NewValue::Default)
                .collect();
            if cols.is_empty() {
                return format!("INSERT INTO {table} DEFAULT VALUES");
            }
            let names: Vec<String> = cols.iter().map(|(c, _)| quote_ident(c)).collect();
            let vals: Vec<String> = cols
                .iter()
                .map(|(c, v)| values.new_value(v, set.data_type(c)))
                .collect();
            format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                names.join(", "),
                vals.join(", ")
            )
        }
        Change::Delete {
            key,
            expect_version,
        } => {
            let mut conds = key_cond(values, key);
            if let Some(v) = expect_version {
                conds.push(format!("t.xmin::text = {}", values.text(v)));
            }
            format!("DELETE FROM {table} AS t WHERE {}", conds.join(" AND "))
        }
    }
}

/// Parameterised statements, in the change set's order.
pub fn statements(set: &ChangeSet) -> Vec<Statement> {
    set.changes
        .iter()
        .map(|c| {
            let mut values = Values::Params(Vec::new());
            let text = build(set, c, &mut values);
            let Values::Params(params) = values else {
                unreachable!()
            };
            Statement { text, params }
        })
        .collect()
}

/// Readable statements with literal values, one per line (for display and
/// manual edition; they run between `BEGIN` and `COMMIT`).
pub fn preview(set: &ChangeSet) -> String {
    set.changes
        .iter()
        .map(|c| format!("{};", build(set, c, &mut Values::Literals)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::catalog::{ObjectKind, ObjectRef};
    use crate::db::data::ColumnSpec;
    use pretty_assertions::assert_eq;

    fn set(changes: Vec<Change>) -> ChangeSet {
        let col = |n: &str, t: &str| ColumnSpec {
            name: n.into(),
            data_type: t.into(),
        };
        ChangeSet {
            object: ObjectRef {
                connection: "c".into(),
                database: "d".into(),
                schema: "shop".into(),
                name: "Order Items".into(),
                kind: ObjectKind::Table,
                signature: None,
            },
            columns: vec![
                col("id", "bigint"),
                col("user", "text"),
                col("total", "numeric(12,2)"),
            ],
            changes,
        }
    }

    fn key(id: &str) -> Vec<(String, String)> {
        vec![("id".into(), id.into())]
    }

    #[test]
    fn update_with_conflict_check() {
        let s = set(vec![Change::Update {
            key: key("7"),
            set: vec![
                ("total".into(), NewValue::Text("12.50".into())),
                ("user".into(), NewValue::Null),
            ],
            expect: vec![
                ("total".into(), Some("10.00".into())),
                ("user".into(), None),
            ],
        }]);
        let st = &statements(&s)[0];
        assert_eq!(
            st.text,
            "UPDATE shop.\"Order Items\" AS t SET total = CAST($1::text AS numeric(12,2)), \"user\" = NULL \
             WHERE t.id = CAST($2::text AS bigint) AND t.total::text = $3 AND t.\"user\" IS NULL"
        );
        assert_eq!(
            st.params,
            vec![Some("12.50".into()), Some("7".into()), Some("10.00".into())]
        );
    }

    #[test]
    fn insert_skips_defaults() {
        let s = set(vec![
            Change::Insert {
                values: vec![
                    ("id".into(), NewValue::Default),
                    ("user".into(), NewValue::Text("zoé".into())),
                    ("total".into(), NewValue::Null),
                ],
            },
            Change::Insert {
                values: vec![("id".into(), NewValue::Default)],
            },
        ]);
        let st = statements(&s);
        assert_eq!(
            st[0].text,
            "INSERT INTO shop.\"Order Items\" (\"user\", total) VALUES (CAST($1::text AS text), NULL)"
        );
        assert_eq!(st[0].params, vec![Some("zoé".into())]);
        assert_eq!(
            st[1].text,
            "INSERT INTO shop.\"Order Items\" DEFAULT VALUES"
        );
    }

    #[test]
    fn delete_checks_the_row_version() {
        let s = set(vec![
            Change::Delete {
                key: key("3"),
                expect_version: Some("1234".into()),
            },
            Change::Delete {
                key: key("4"),
                expect_version: None,
            },
        ]);
        let st = statements(&s);
        assert_eq!(
            st[0].text,
            "DELETE FROM shop.\"Order Items\" AS t WHERE t.id = CAST($1::text AS bigint) AND t.xmin::text = $2"
        );
        assert_eq!(st[0].params, vec![Some("3".into()), Some("1234".into())]);
        assert!(
            st[1]
                .text
                .ends_with("WHERE t.id = CAST($1::text AS bigint)")
        );
    }

    #[test]
    fn values_are_never_concatenated() {
        let evil = "x'); DROP TABLE shop.orders; --";
        let s = set(vec![
            Change::Update {
                key: vec![("id".into(), evil.into())],
                set: vec![("user".into(), NewValue::Text(evil.into()))],
                expect: vec![("user".into(), Some(evil.into()))],
            },
            Change::Insert {
                values: vec![("user".into(), NewValue::Text(evil.into()))],
            },
            Change::Delete {
                key: vec![("id".into(), evil.into())],
                expect_version: Some(evil.into()),
            },
        ]);
        for st in statements(&s) {
            assert!(!st.text.contains("DROP"), "{}", st.text);
            assert!(st.params.iter().all(|p| p.as_deref() == Some(evil)));
        }
    }

    #[test]
    fn preview_uses_quoted_literals() {
        let s = set(vec![Change::Update {
            key: key("7"),
            set: vec![("user".into(), NewValue::Text("it's".into()))],
            expect: vec![("user".into(), Some("old".into()))],
        }]);
        assert_eq!(
            preview(&s),
            "UPDATE shop.\"Order Items\" AS t SET \"user\" = 'it''s'::text \
             WHERE t.id = '7'::bigint AND t.\"user\"::text = 'old';"
        );
    }
}
