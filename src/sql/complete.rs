//! Context-aware completion for the SQL editor.
//!
//! From the text and the cursor: the word being typed (and its qualifier,
//! `alias.` or `schema.`), the tables of the current statement with their
//! aliases (`FROM`, `JOIN`, `UPDATE`, `INTO`), and what is expected
//! (a relation after `FROM` / `JOIN` ..., else columns, keywords,
//! functions). Engine-neutral: the catalog is given as plain lists.

use std::collections::HashSet;
use std::ops::Range;

use super::lexer::{TokenKind, tokenize};
use super::quote_ident;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub schema: String,
    pub name: String,
    /// `table`, `vue`, ... (shown next to the candidate).
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub data_type: String,
}

/// What the completion knows about a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionCatalog {
    pub schemas: Vec<String>,
    pub relations: Vec<Relation>,
    pub columns: Vec<Column>,
    /// (schema, name)
    pub functions: Vec<(String, String)>,
    /// Schemas whose objects need no qualification (`search_path`).
    pub search_path: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateKind {
    Column,
    Relation,
    Schema,
    Function,
    Keyword,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Text inserted in place of the word.
    pub insert: String,
    /// Shown in the list.
    pub label: String,
    /// Type, kind, table ... (dim, on the right).
    pub detail: String,
    pub kind: CandidateKind,
}

/// Keywords offered by the completion (the most common ones).
const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "and",
    "or",
    "not",
    "in",
    "is",
    "null",
    "like",
    "ilike",
    "between",
    "exists",
    "as",
    "on",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "outer",
    "cross",
    "lateral",
    "using",
    "natural",
    "group",
    "by",
    "having",
    "order",
    "asc",
    "desc",
    "nulls",
    "first",
    "last",
    "limit",
    "offset",
    "fetch",
    "distinct",
    "union",
    "all",
    "intersect",
    "except",
    "with",
    "recursive",
    "case",
    "when",
    "then",
    "else",
    "end",
    "insert",
    "into",
    "values",
    "default",
    "returning",
    "update",
    "set",
    "delete",
    "merge",
    "matched",
    "conflict",
    "do",
    "nothing",
    "create",
    "table",
    "view",
    "index",
    "unique",
    "primary",
    "key",
    "foreign",
    "references",
    "check",
    "constraint",
    "alter",
    "add",
    "column",
    "drop",
    "rename",
    "to",
    "truncate",
    "cascade",
    "restrict",
    "begin",
    "commit",
    "rollback",
    "savepoint",
    "release",
    "transaction",
    "explain",
    "analyze",
    "vacuum",
    "grant",
    "revoke",
    "schema",
    "sequence",
    "function",
    "procedure",
    "trigger",
    "materialized",
    "refresh",
    "if",
    "true",
    "false",
    "cast",
    "coalesce",
    "nullif",
    "interval",
    "current_date",
    "current_timestamp",
    "now",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "string_agg",
    "over",
    "partition",
    "window",
    "filter",
    "within",
];

/// Keywords after which a relation name is expected.
const RELATION_CONTEXT: &[&str] = &["from", "join", "update", "into", "table", "truncate"];

/// A table of the statement and its alias.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableRef {
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Unquoted, lower-cased identifier (`"Foo"` stays `Foo`).
fn ident(text: &str) -> String {
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(q) => q.replace("\"\"", "\""),
        None => text.to_lowercase(),
    }
}

/// Tables referenced by a statement, with their aliases.
fn table_refs(stmt: &str) -> Vec<TableRef> {
    let toks: Vec<(TokenKind, String)> = tokenize(stmt)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .map(|t| (t.kind, t.text(stmt).to_string()))
        .collect();
    let is_word = |i: usize| {
        toks.get(i)
            .is_some_and(|(k, _)| matches!(k, TokenKind::Word | TokenKind::QuotedIdent))
    };
    let lower = |i: usize| {
        toks.get(i)
            .map(|(_, t)| t.to_lowercase())
            .unwrap_or_default()
    };
    let mut refs = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let kw = lower(i);
        let starts = matches!(kw.as_str(), "from" | "join" | "update" | "into");
        i += 1;
        if !starts {
            continue;
        }
        // A list of `name [AS] alias` separated by commas (FROM a, b).
        loop {
            if lower(i) == "only" || lower(i) == "lateral" {
                i += 1;
            }
            if !is_word(i) {
                break;
            }
            let first = ident(&toks[i].1);
            let (schema, name) = if toks.get(i + 1).is_some_and(|(_, t)| t == ".") && is_word(i + 2)
            {
                let n = ident(&toks[i + 2].1);
                i += 3;
                (Some(first), n)
            } else {
                i += 1;
                (None, first)
            };
            if lower(i) == "as" {
                i += 1;
            }
            let alias = if is_word(i) && toks[i].0 == TokenKind::QuotedIdent
                || (is_word(i) && !super::lexer::is_keyword(&toks[i].1))
            {
                let a = ident(&toks[i].1);
                i += 1;
                Some(a)
            } else {
                None
            };
            refs.push(TableRef {
                schema,
                name,
                alias,
            });
            if toks.get(i).is_some_and(|(_, t)| t == ",") && kw == "from" {
                i += 1;
                continue;
            }
            break;
        }
    }
    refs
}

/// Byte range of the word being typed and its qualifier (`a` in `a.b`).
fn word_at(src: &str, cursor: usize) -> (Range<usize>, Option<String>) {
    let before = &src[..cursor];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(cursor, |(i, _)| i);
    let qualifier = before[..start].strip_suffix('.').map(|q| {
        let q_start = q
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_ident_char(*c) || *c == '"')
            .last()
            .map_or(q.len(), |(i, _)| i);
        ident(&q[q_start..])
    });
    (start..cursor, qualifier.filter(|q| !q.is_empty()))
}

/// The keyword the word follows directly (`FROM |`), or the keyword of a
/// list it continues (`FROM a, |`). `None` after an identifier
/// (`FROM t |` expects an alias or a keyword) or an operator.
fn previous_keyword(stmt: &str) -> Option<String> {
    let toks: Vec<_> = tokenize(stmt)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .collect();
    let mut it = toks.iter().rev();
    let first = it.next()?;
    let keyword = |t: &super::lexer::Token| {
        let w = t.text(stmt).to_lowercase();
        (t.kind == TokenKind::Word && super::lexer::is_keyword(&w)).then_some(w)
    };
    if let Some(k) = keyword(first) {
        return Some(k);
    }
    if first.text(stmt) != "," {
        return None;
    }
    for t in it {
        if let Some(k) = keyword(t) {
            return Some(k);
        }
        let skip = matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent)
            || matches!(t.text(stmt), "," | ".");
        if !skip {
            return None;
        }
    }
    None
}

fn matches(label: &str, prefix: &str) -> bool {
    label.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// Candidates for the word at `cursor` (byte offset). Returns the range to
/// replace and the candidates, best first.
pub fn complete(
    src: &str,
    cursor: usize,
    cat: &CompletionCatalog,
) -> (Range<usize>, Vec<Candidate>) {
    let (range, qualifier) = word_at(src, cursor);
    let prefix = &src[range.clone()];
    let stmt_range = super::script::statement_at(src, cursor).unwrap_or(0..src.len());
    let stmt_start = stmt_range.start.min(range.start);
    let stmt_full = &src[stmt_start..stmt_range.end.max(cursor)];
    let refs = table_refs(stmt_full);
    let before_word = &src[stmt_start..range.start];
    let lower_case = prefix.chars().next().is_none_or(char::is_lowercase);
    let mut out: Vec<Candidate> = Vec::new();
    let in_path = |s: &str| cat.search_path.iter().any(|p| p == s);

    let relation = |r: &Relation, qualified: bool| Candidate {
        insert: if qualified && !in_path(&r.schema) {
            format!("{}.{}", quote_ident(&r.schema), quote_ident(&r.name))
        } else {
            quote_ident(&r.name)
        },
        label: if in_path(&r.schema) {
            r.name.clone()
        } else {
            format!("{}.{}", r.schema, r.name)
        },
        detail: r.kind.to_string(),
        kind: CandidateKind::Relation,
    };
    let column = |c: &Column, table_label: &str| Candidate {
        insert: quote_ident(&c.name),
        label: c.name.clone(),
        detail: format!("{} · {table_label}", c.data_type),
        kind: CandidateKind::Column,
    };

    if let Some(q) = qualifier {
        // alias. / table. -> columns; schema. -> its objects.
        let target = refs
            .iter()
            .find(|r| r.alias.as_deref() == Some(q.as_str()))
            .or_else(|| refs.iter().find(|r| r.name == q));
        match target {
            Some(t) => {
                for c in cat.columns.iter().filter(|c| {
                    c.table == t.name && t.schema.as_ref().is_none_or(|s| *s == c.schema)
                }) {
                    if matches(&c.name, prefix) {
                        out.push(column(c, &t.name));
                    }
                }
            }
            None if cat.schemas.contains(&q) => {
                for r in cat.relations.iter().filter(|r| r.schema == q) {
                    if matches(&r.name, prefix) {
                        out.push(Candidate {
                            insert: quote_ident(&r.name),
                            label: r.name.clone(),
                            detail: r.kind.to_string(),
                            kind: CandidateKind::Relation,
                        });
                    }
                }
                for (s, f) in cat.functions.iter().filter(|(s, _)| *s == q) {
                    if matches(f, prefix) {
                        out.push(Candidate {
                            insert: format!("{}(", quote_ident(f)),
                            label: format!("{f}()"),
                            detail: format!("fonction · {s}"),
                            kind: CandidateKind::Function,
                        });
                    }
                }
            }
            None => {
                // An unknown table (not in the statement): try by name.
                for c in cat.columns.iter().filter(|c| c.table == q) {
                    if matches(&c.name, prefix) {
                        out.push(column(c, &c.table));
                    }
                }
            }
        }
        dedup(&mut out);
        return (range, out);
    }

    let prev = previous_keyword(before_word);
    if prev
        .as_deref()
        .is_some_and(|k| RELATION_CONTEXT.contains(&k))
    {
        for r in &cat.relations {
            let label_ok =
                matches(&r.name, prefix) || matches(&format!("{}.{}", r.schema, r.name), prefix);
            if label_ok {
                out.push(relation(r, true));
            }
        }
        for s in &cat.schemas {
            if matches(s, prefix) {
                out.push(Candidate {
                    insert: quote_ident(s),
                    label: s.clone(),
                    detail: "schéma".into(),
                    kind: CandidateKind::Schema,
                });
            }
        }
        // Tables in the search path first.
        out.sort_by_key(|c| (c.kind, c.label.contains('.')));
        dedup(&mut out);
        return (range, out);
    }

    // Columns of the tables of the statement, then keywords, functions,
    // relations and schemas.
    for t in &refs {
        let label = t.alias.clone().unwrap_or_else(|| t.name.clone());
        for c in cat
            .columns
            .iter()
            .filter(|c| c.table == t.name && t.schema.as_ref().is_none_or(|s| *s == c.schema))
        {
            if matches(&c.name, prefix) {
                out.push(column(c, &label));
            }
        }
    }
    if !prefix.is_empty() {
        for k in KEYWORDS {
            if matches(k, prefix) {
                let text = if lower_case {
                    k.to_string()
                } else {
                    k.to_uppercase()
                };
                out.push(Candidate {
                    insert: text.clone(),
                    label: text,
                    detail: "mot-clé".into(),
                    kind: CandidateKind::Keyword,
                });
            }
        }
        for (s, f) in &cat.functions {
            if matches(f, prefix) {
                out.push(Candidate {
                    insert: if in_path(s) {
                        format!("{}(", quote_ident(f))
                    } else {
                        format!("{}.{}(", quote_ident(s), quote_ident(f))
                    },
                    label: format!("{f}()"),
                    detail: format!("fonction · {s}"),
                    kind: CandidateKind::Function,
                });
            }
        }
        for r in &cat.relations {
            if matches(&r.name, prefix) {
                out.push(relation(r, true));
            }
        }
        for s in &cat.schemas {
            if matches(s, prefix) {
                out.push(Candidate {
                    insert: quote_ident(s),
                    label: s.clone(),
                    detail: "schéma".into(),
                    kind: CandidateKind::Schema,
                });
            }
        }
    }
    // An exact keyword match is not worth a popup.
    if out.len() == 1 && out[0].insert.eq_ignore_ascii_case(prefix) {
        out.clear();
    }
    dedup(&mut out);
    (range, out)
}

fn dedup(out: &mut Vec<Candidate>) {
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert((c.insert.clone(), c.detail.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn cat() -> CompletionCatalog {
        let rel = |s: &str, n: &str| Relation {
            schema: s.into(),
            name: n.into(),
            kind: "table",
        };
        let col = |t: &str, n: &str, ty: &str| Column {
            schema: "shop".into(),
            table: t.into(),
            name: n.into(),
            data_type: ty.into(),
        };
        CompletionCatalog {
            schemas: vec!["public".into(), "shop".into()],
            relations: vec![
                rel("shop", "orders"),
                rel("shop", "customers"),
                rel("public", "settings"),
            ],
            columns: vec![
                col("orders", "id", "bigint"),
                col("orders", "customer_id", "bigint"),
                col("orders", "total", "numeric"),
                col("customers", "id", "bigint"),
                col("customers", "email", "text"),
                col("customers", "Full Name", "text"),
            ],
            functions: vec![("shop".into(), "order_total".into())],
            search_path: vec!["public".into()],
        }
    }

    /// Completes at `|` in `src`.
    fn at(src: &str) -> Vec<String> {
        let cursor = src.find('|').unwrap();
        let text = src.replace('|', "");
        complete(&text, cursor, &cat())
            .1
            .into_iter()
            .map(|c| c.insert)
            .collect()
    }

    #[test]
    fn columns_through_aliases() {
        assert_eq!(
            at("select o.| from shop.orders o join shop.customers c on c.id = o.customer_id"),
            ["id", "customer_id", "total"]
        );
        assert_eq!(at("select c.e| from shop.customers as c"), ["email"]);
        assert_eq!(at("select c.F| from shop.customers c"), ["\"Full Name\""]);
        // The table name works as a qualifier too.
        assert_eq!(at("select orders.t| from shop.orders"), ["total"]);
    }

    #[test]
    fn relations_after_from_and_join() {
        assert_eq!(at("select * from cu|"), ["shop.customers"]);
        assert_eq!(
            at("select * from shop.orders o join |"),
            [
                "settings",
                "shop.orders",
                "shop.customers",
                "public",
                "shop"
            ]
        );
        assert_eq!(at("select * from a, se|"), ["settings"]);
        assert_eq!(
            at("update s|"),
            ["settings", "shop.orders", "shop.customers", "shop"]
        );
        // schema. -> its objects.
        assert_eq!(at("select * from shop.o|"), ["orders", "order_total("]);
    }

    #[test]
    fn columns_then_keywords_elsewhere() {
        let c = at("select cu| from shop.orders");
        assert_eq!(c[0], "customer_id");
        assert!(c.contains(&"current_date".to_string()));
        assert!(c.contains(&"shop.customers".to_string()));
        assert_eq!(at("SELECT * FROM shop.orders WH|")[0], "WHERE");
        assert_eq!(at("select * from shop.orders where tot|"), ["total"]);
        assert!(at("select * from shop.orders where |").contains(&"customer_id".to_string()));
        // Only the current statement counts.
        assert_eq!(
            at("select * from shop.customers; select em|"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn exact_keyword_gives_nothing() {
        assert!(at("selec|").contains(&"select".to_string()));
        assert!(at("select|").is_empty());
    }
}
