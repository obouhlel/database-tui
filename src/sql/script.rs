//! SQL scripts: splitting into statements (on top-level `;`), finding the
//! statement under the cursor, and a few checks on a statement (kind, effect
//! on the transaction, `UPDATE` / `DELETE` without `WHERE`).

use std::ops::Range;

use super::lexer::{Token, TokenKind, tokenize};

/// Statements of `src` as byte ranges, without the `;` and without leading
/// or trailing whitespace. Empty statements (only blanks / comments) are
/// left out. `;` inside strings, comments, dollar-quoted bodies and
/// `BEGIN ATOMIC ... END` bodies does not split.
pub fn split(src: &str) -> Vec<Range<usize>> {
    let tokens = tokenize(src);
    let mut out = Vec::new();
    let mut start = 0;
    let mut atomic = 0usize;
    let mut prev_word: Option<String> = None;
    for t in &tokens {
        match t.kind {
            TokenKind::Word => {
                let w = t.text(src).to_ascii_lowercase();
                if atomic > 0 {
                    match w.as_str() {
                        "case" => atomic += 1,
                        "end" => atomic -= 1,
                        _ => {}
                    }
                } else if w == "atomic" && prev_word.as_deref() == Some("begin") {
                    atomic = 1;
                }
                prev_word = Some(w);
            }
            TokenKind::Punct if t.text(src) == ";" && atomic == 0 => {
                push(src, &tokens, start..t.start, &mut out);
                start = t.end;
                prev_word = None;
            }
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment => {}
            _ => prev_word = None,
        }
    }
    push(src, &tokens, start..src.len(), &mut out);
    out
}

/// Adds `range` trimmed, if it holds more than blanks and comments.
fn push(src: &str, tokens: &[Token], range: Range<usize>, out: &mut Vec<Range<usize>>) {
    let meaningful = tokens.iter().any(|t| {
        t.start >= range.start
            && t.end <= range.end
            && !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
    });
    if !meaningful {
        return;
    }
    let text = &src[range.clone()];
    let start = range.start + (text.len() - text.trim_start().len());
    let end = range.end - (text.len() - text.trim_end().len());
    out.push(start..end);
}

/// The statement containing byte `offset` (a cursor just after a `;` or on
/// a blank line between statements belongs to the previous statement).
pub fn statement_at(src: &str, offset: usize) -> Option<Range<usize>> {
    let stmts = split(src);
    stmts
        .iter()
        .find(|r| r.contains(&offset) || r.end == offset)
        .or_else(|| stmts.iter().rev().find(|r| r.end <= offset))
        .or_else(|| stmts.first())
        .cloned()
}

/// Lower-case words of a statement, outside strings and comments.
fn words(stmt: &str) -> Vec<String> {
    tokenize(stmt)
        .iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(stmt).to_ascii_lowercase())
        .collect()
}

/// Effect of a statement on the transaction state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnEffect {
    Begin,
    /// `COMMIT`, `END`, `ROLLBACK`, `ABORT` (but not `ROLLBACK TO`).
    End,
    None,
}

pub fn txn_effect(stmt: &str) -> TxnEffect {
    let w = words(stmt);
    let first = w.first().map(String::as_str);
    let second = w.get(1).map(String::as_str);
    match first {
        Some("begin") if second != Some("atomic") => TxnEffect::Begin,
        Some("start") if second == Some("transaction") => TxnEffect::Begin,
        Some("commit" | "end" | "abort") => {
            if second == Some("prepared") {
                TxnEffect::None
            } else {
                TxnEffect::End
            }
        }
        Some("rollback") => {
            let to = w.iter().skip(1).take(2).any(|x| x == "to");
            if to || second == Some("prepared") {
                TxnEffect::None
            } else {
                TxnEffect::End
            }
        }
        _ => TxnEffect::None,
    }
}

/// `UPDATE` or `DELETE` without a `WHERE` clause (affects every row).
/// `WITH ... UPDATE` is checked too; `WHERE CURRENT OF` counts as a `WHERE`.
pub fn unrestricted_write(stmt: &str) -> Option<&'static str> {
    let tokens = tokenize(stmt);
    // Top-level words only (parentheses hide sub-queries).
    let mut depth = 0i32;
    let mut top: Vec<String> = Vec::new();
    for t in &tokens {
        match (t.kind, t.text(stmt)) {
            (TokenKind::Punct, "(") => depth += 1,
            (TokenKind::Punct, ")") => depth -= 1,
            (TokenKind::Word, w) if depth == 0 => top.push(w.to_ascii_lowercase()),
            _ => {}
        }
    }
    let verb = top
        .iter()
        .position(|w| w == "update" || w == "delete")
        .filter(|i| *i == 0 || top[0] == "with")?;
    let kind = if top[verb] == "update" {
        "UPDATE"
    } else {
        "DELETE"
    };
    // `ON CONFLICT DO UPDATE` is an INSERT, not an UPDATE statement.
    if top[..verb].iter().any(|w| w == "insert") {
        return None;
    }
    (!top[verb..].iter().any(|w| w == "where")).then_some(kind)
}

/// Table read by a `SELECT` (the first one after its top-level `FROM`),
/// without schema, for result labels. `None` for anything else.
pub fn main_table(stmt: &str) -> Option<String> {
    let tokens: Vec<Token> = tokenize(stmt)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .collect();
    let first = tokens.first()?.text(stmt).to_ascii_lowercase();
    if first != "select" && first != "with" && first != "table" {
        return None;
    }
    let name = |t: &Token| match t.kind {
        TokenKind::Word => Some(t.text(stmt).to_string()),
        TokenKind::QuotedIdent => Some(t.text(stmt).trim_matches('"').replace("\"\"", "\"")),
        _ => None,
    };
    // `TABLE t`
    let start = if first == "table" {
        1
    } else {
        // Top-level FROM of the main query (after any WITH ... AS (...)).
        let mut depth = 0i32;
        let mut at = None;
        for (i, t) in tokens.iter().enumerate() {
            match (t.kind, t.text(stmt)) {
                (TokenKind::Punct, "(") => depth += 1,
                (TokenKind::Punct, ")") => depth -= 1,
                (TokenKind::Word, w) if depth == 0 && w.eq_ignore_ascii_case("from") => {
                    at = Some(i + 1);
                    break;
                }
                _ => {}
            }
        }
        at?
    };
    let mut start = start;
    let first = tokens.get(start)?;
    if first.kind == TokenKind::Word && super::lexer::is_keyword(first.text(stmt)) {
        // `FROM ONLY t`; `FROM LATERAL ...`, `FROM (...)`: no single table.
        if !first.text(stmt).eq_ignore_ascii_case("only") {
            return None;
        }
        start += 1;
    }
    qualified_name_at(stmt, &tokens, start, name)
}

/// Last part of the (possibly qualified) name starting at `tokens[start]`.
fn qualified_name_at(
    stmt: &str,
    tokens: &[Token],
    start: usize,
    name: impl Fn(&Token) -> Option<String>,
) -> Option<String> {
    let mut table = name(tokens.get(start)?)?;
    let mut i = start;
    while tokens.get(i + 1).is_some_and(|t| t.text(stmt) == ".") {
        table = name(tokens.get(i + 2)?)?;
        i += 2;
    }
    Some(table)
}

/// First keyword, upper-case (`SELECT`, `UPDATE`, ...), for labels.
pub fn verb(stmt: &str) -> String {
    words(stmt)
        .into_iter()
        .next()
        .map(|w| w.to_ascii_uppercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn texts(src: &str) -> Vec<&str> {
        split(src).into_iter().map(|r| &src[r]).collect()
    }

    #[test]
    fn splits_on_top_level_semicolons() {
        let src = "select 1;\n  select ';' -- ;\n , 2 ; /* ; */\n\n; select $$a;b$$;select e'\\';'";
        assert_eq!(
            texts(src),
            [
                "select 1",
                "select ';' -- ;\n , 2",
                "select $$a;b$$",
                "select e'\\';'"
            ]
        );
        assert_eq!(texts("  -- only a comment\n"), Vec::<&str>::new());
        assert_eq!(texts("select 1"), ["select 1"]);
    }

    #[test]
    fn main_table_of_a_select() {
        assert_eq!(
            main_table("select * from shop.orders where id = 1").as_deref(),
            Some("orders")
        );
        assert_eq!(
            main_table("SELECT a FROM \"My T\" t").as_deref(),
            Some("My T")
        );
        assert_eq!(
            main_table("with x as (select 1 from a) select * from b join c on true").as_deref(),
            Some("b")
        );
        assert_eq!(
            main_table("select (select 1 from a) from only public.b").as_deref(),
            Some("b")
        );
        assert_eq!(
            main_table("table emargements").as_deref(),
            Some("emargements")
        );
        assert_eq!(main_table("select 1"), None);
        assert_eq!(main_table("select * from (select 1) s"), None);
        assert_eq!(main_table("update t set a = 1"), None);
    }

    #[test]
    fn begin_atomic_bodies_are_kept_whole() {
        let src = "create function f() returns int language sql begin atomic \
                   select case when true then 1 end; select 2; end; select 3";
        assert_eq!(texts(src).len(), 2);
        assert!(texts(src)[0].ends_with("select 2; end"));
        // A plain BEGIN is a statement of its own.
        assert_eq!(texts("begin; update t set a = 1; commit;").len(), 3);
    }

    #[test]
    fn statement_under_the_cursor() {
        let src = "select 1;\n\nselect 2;\nselect 3";
        let at = |o| statement_at(src, o).map(|r| &src[r]);
        assert_eq!(at(0), Some("select 1"));
        assert_eq!(at(9), Some("select 1"), "right after the semicolon");
        assert_eq!(at(10), Some("select 1"), "blank line after it");
        assert_eq!(at(12), Some("select 2"));
        assert_eq!(at(src.len()), Some("select 3"));
        assert_eq!(statement_at("", 0), None);
    }

    #[test]
    fn transaction_effects() {
        assert_eq!(txn_effect("BEGIN"), TxnEffect::Begin);
        assert_eq!(
            txn_effect("begin isolation level serializable"),
            TxnEffect::Begin
        );
        assert_eq!(txn_effect("START TRANSACTION"), TxnEffect::Begin);
        assert_eq!(txn_effect("commit"), TxnEffect::End);
        assert_eq!(txn_effect("end"), TxnEffect::End);
        assert_eq!(txn_effect("rollback"), TxnEffect::End);
        assert_eq!(txn_effect("rollback to savepoint a"), TxnEffect::None);
        assert_eq!(txn_effect("ROLLBACK TO a"), TxnEffect::None);
        assert_eq!(txn_effect("commit prepared 'x'"), TxnEffect::None);
        assert_eq!(txn_effect("savepoint a"), TxnEffect::None);
        assert_eq!(txn_effect("-- hi\nselect 1"), TxnEffect::None);
    }

    #[test]
    fn writes_without_where() {
        assert_eq!(unrestricted_write("update t set a = 1"), Some("UPDATE"));
        assert_eq!(unrestricted_write("DELETE FROM t"), Some("DELETE"));
        assert_eq!(unrestricted_write("update t set a = 1 where id = 2"), None);
        assert_eq!(
            unrestricted_write("delete from t where id in (select id from u)"),
            None
        );
        // A WHERE in a sub-query does not count.
        assert_eq!(
            unrestricted_write("update t set a = (select max(x) from u where u.k = 1)"),
            Some("UPDATE")
        );
        assert_eq!(
            unrestricted_write("with x as (select 1) delete from t"),
            Some("DELETE")
        );
        assert_eq!(
            unrestricted_write("insert into t values (1) on conflict (id) do update set a = 2"),
            None
        );
        assert_eq!(unrestricted_write("select 'delete from t'"), None);
        assert_eq!(unrestricted_write("delete from t where current of c"), None);
    }
}
