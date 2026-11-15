//! A small PostgreSQL lexer, tolerant to incomplete input (for an editor).
//!
//! Handles `'...'` with `''`, `E'...'` with backslash escapes, `$tag$...$tag$`
//! dollar quoting, `"quoted"` identifiers, nested `/* */` comments, `--`
//! comments, numbers and `$1` parameters. Used for syntax highlighting and,
//! later, statement splitting.

use std::collections::HashSet;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    /// String literal, any flavour (`'..'`, `E'..'`, `$$..$$`, `B'..'`, `X'..'`).
    String,
    QuotedIdent,
    /// Keyword or identifier (see [`is_keyword`] / [`is_type_name`]).
    Word,
    Number,
    /// `$1`
    Param,
    Operator,
    /// `( ) [ ] , . : ;`
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    /// Byte range in the source.
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let c = bytes[i];
        let kind = match c {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                TokenKind::Whitespace
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                TokenKind::LineComment
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(bytes, i);
                TokenKind::BlockComment
            }
            b'\'' => {
                i = quoted_end(bytes, i + 1, b'\'', false);
                TokenKind::String
            }
            b'"' => {
                i = quoted_end(bytes, i + 1, b'"', false);
                TokenKind::QuotedIdent
            }
            b'e' | b'E' if bytes.get(i + 1) == Some(&b'\'') => {
                i = quoted_end(bytes, i + 2, b'\'', true);
                TokenKind::String
            }
            b'b' | b'B' | b'x' | b'X' | b'n' | b'N' if bytes.get(i + 1) == Some(&b'\'') => {
                i = quoted_end(bytes, i + 2, b'\'', false);
                TokenKind::String
            }
            b'u' | b'U' if bytes.get(i + 1) == Some(&b'&') && bytes.get(i + 2) == Some(&b'\'') => {
                i = quoted_end(bytes, i + 3, b'\'', false);
                TokenKind::String
            }
            b'$' => {
                if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    TokenKind::Param
                } else if let Some(tag_end) = dollar_tag_end(bytes, i) {
                    let tag = &bytes[i..tag_end];
                    i = find(bytes, tag_end, tag).map_or(bytes.len(), |p| p + tag.len());
                    TokenKind::String
                } else {
                    i += 1;
                    TokenKind::Operator
                }
            }
            b'0'..=b'9' => {
                i = number_end(bytes, i);
                TokenKind::Number
            }
            b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i = number_end(bytes, i);
                TokenKind::Number
            }
            b'(' | b')' | b'[' | b']' | b',' | b';' | b'.' => {
                i += 1;
                TokenKind::Punct
            }
            b':' if bytes.get(i + 1) == Some(&b':') => {
                i += 2;
                TokenKind::Operator
            }
            b':' => {
                i += 1;
                TokenKind::Punct
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] == b'$'
                        || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                TokenKind::Word
            }
            b'+' | b'-' | b'*' | b'/' | b'<' | b'>' | b'=' | b'~' | b'!' | b'@' | b'#' | b'%'
            | b'^' | b'&' | b'|' | b'`' | b'?' => {
                while i < bytes.len()
                    && b"+-*/<>=~!@#%^&|`?".contains(&bytes[i])
                    && !(bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-'))
                    && !(bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*'))
                {
                    i += 1;
                }
                if i == start {
                    i += 1;
                }
                TokenKind::Operator
            }
            _ => {
                // Any other byte: consume the whole UTF-8 character.
                i += utf8_len(c);
                TokenKind::Operator
            }
        };
        let end = i.min(bytes.len());
        tokens.push(Token { kind, start, end });
        i = end;
    }
    tokens
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

/// End of a quoted string starting after the opening quote. A doubled quote
/// is an escaped quote; with `backslash`, `\x` escapes `x`.
fn quoted_end(bytes: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    while i < bytes.len() {
        if backslash && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn block_comment_end(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

/// `$tag$` or `$$` starting at `i` (which holds `$`): end of the opening tag.
fn dollar_tag_end(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if bytes.get(j).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    while j < bytes.len()
        && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80)
    {
        j += 1;
    }
    (bytes.get(j) == Some(&b'$')).then_some(j + 1)
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn number_end(bytes: &[u8], mut i: usize) -> usize {
    // Hex / octal / binary integers and `_` separators (PG 16+).
    if bytes[i] == b'0'
        && matches!(
            bytes.get(i + 1),
            Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
        )
    {
        i += 2;
        while i < bytes.len() && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
            i += 1;
        }
        return i;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1) != Some(&b'.') {
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

/// SQL keywords highlighted as such (case-insensitive).
pub fn is_keyword(word: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "add",
        "all",
        "alter",
        "always",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "begin",
        "between",
        "by",
        "cascade",
        "case",
        "cast",
        "check",
        "collate",
        "column",
        "comment",
        "commit",
        "concurrently",
        "constraint",
        "copy",
        "create",
        "cross",
        "current_date",
        "current_timestamp",
        "current_user",
        "cycle",
        "data",
        "database",
        "deferrable",
        "deferred",
        "default",
        "delete",
        "desc",
        "distinct",
        "do",
        "drop",
        "each",
        "else",
        "end",
        "except",
        "execute",
        "exists",
        "explain",
        "extension",
        "false",
        "fetch",
        "filter",
        "first",
        "for",
        "foreign",
        "from",
        "full",
        "function",
        "generated",
        "grant",
        "group",
        "having",
        "identity",
        "if",
        "ilike",
        "immediate",
        "in",
        "increment",
        "index",
        "inherits",
        "initially",
        "inner",
        "insert",
        "instead",
        "intersect",
        "into",
        "is",
        "isnull",
        "join",
        "key",
        "language",
        "last",
        "lateral",
        "left",
        "like",
        "limit",
        "materialized",
        "maxvalue",
        "minvalue",
        "no",
        "not",
        "notnull",
        "null",
        "nulls",
        "of",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "outer",
        "over",
        "owned",
        "owner",
        "partition",
        "policy",
        "primary",
        "procedure",
        "range",
        "recursive",
        "references",
        "refresh",
        "rename",
        "replace",
        "restrict",
        "returning",
        "returns",
        "revoke",
        "right",
        "rollback",
        "row",
        "rows",
        "savepoint",
        "schema",
        "select",
        "sequence",
        "server",
        "set",
        "start",
        "statement",
        "stored",
        "table",
        "tablespace",
        "temp",
        "temporary",
        "then",
        "to",
        "transaction",
        "trigger",
        "true",
        "truncate",
        "type",
        "union",
        "unique",
        "unlogged",
        "update",
        "using",
        "vacuum",
        "values",
        "view",
        "virtual",
        "when",
        "where",
        "window",
        "with",
        "without",
    ];
    static SET: LazyLock<HashSet<&'static str>> =
        LazyLock::new(|| KEYWORDS.iter().copied().collect());
    SET.contains(word.to_ascii_lowercase().as_str())
}

/// Built-in type names, highlighted differently from keywords.
pub fn is_type_name(word: &str) -> bool {
    const TYPES: &[&str] = &[
        "bigint",
        "bigserial",
        "bit",
        "bool",
        "boolean",
        "box",
        "bpchar",
        "bytea",
        "char",
        "character",
        "cidr",
        "circle",
        "date",
        "daterange",
        "decimal",
        "double",
        "float",
        "float4",
        "float8",
        "inet",
        "int",
        "int2",
        "int4",
        "int8",
        "integer",
        "interval",
        "json",
        "jsonb",
        "line",
        "lseg",
        "macaddr",
        "money",
        "numeric",
        "path",
        "point",
        "polygon",
        "precision",
        "real",
        "serial",
        "smallint",
        "smallserial",
        "text",
        "time",
        "timestamp",
        "timestamptz",
        "timetz",
        "tsquery",
        "tsrange",
        "tstzrange",
        "tsvector",
        "uuid",
        "varchar",
        "varying",
        "xml",
        "zone",
    ];
    static SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| TYPES.iter().copied().collect());
    SET.contains(word.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        tokenize(src)
            .into_iter()
            .filter(|t| t.kind != Whitespace)
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    #[test]
    fn keyword_lookup() {
        assert!(is_keyword("SELECT") && is_keyword("where") && is_keyword("without"));
        assert!(is_type_name("JSONB") && is_type_name("bigint") && is_type_name("zone"));
        assert!(!is_keyword("customers"));
    }

    #[test]
    fn basic_statement() {
        assert_eq!(
            kinds("SELECT a.id, 'x' FROM t WHERE n >= 1.5e3;"),
            vec![
                (Word, "SELECT"),
                (Word, "a"),
                (Punct, "."),
                (Word, "id"),
                (Punct, ","),
                (String, "'x'"),
                (Word, "FROM"),
                (Word, "t"),
                (Word, "WHERE"),
                (Word, "n"),
                (Operator, ">="),
                (Number, "1.5e3"),
                (Punct, ";"),
            ]
        );
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(kinds("'it''s'"), vec![(String, "'it''s'")]);
        assert_eq!(kinds(r"E'a\'b'"), vec![(String, r"E'a\'b'")]);
        assert_eq!(
            kinds("X'ff' B'01'"),
            vec![(String, "X'ff'"), (String, "B'01'")]
        );
        assert_eq!(kinds("U&'d\\0061t'"), vec![(String, "U&'d\\0061t'")]);
        assert_eq!(
            kinds("\"My \"\"Col\"\"\""),
            vec![(QuotedIdent, "\"My \"\"Col\"\"\"")]
        );
    }

    #[test]
    fn dollar_quotes_and_params() {
        let src = "$fn$ select ';' $$ $fn$ $1 $$x$$";
        assert_eq!(
            kinds(src),
            vec![
                (String, "$fn$ select ';' $$ $fn$"),
                (Param, "$1"),
                (String, "$$x$$")
            ]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(
            kinds("a -- c ; d\n/* x /* nested */ y */ b"),
            vec![
                (Word, "a"),
                (LineComment, "-- c ; d"),
                (BlockComment, "/* x /* nested */ y */"),
                (Word, "b")
            ]
        );
    }

    #[test]
    fn casts_and_operators() {
        assert_eq!(
            kinds("x::int <> -1 ->> 'k'"),
            vec![
                (Word, "x"),
                (Operator, "::"),
                (Word, "int"),
                (Operator, "<>"),
                (Operator, "-"),
                (Number, "1"),
                (Operator, "->>"),
                (String, "'k'"),
            ]
        );
        // `--` inside an operator run starts a comment.
        assert_eq!(
            kinds("a+--x"),
            vec![(Word, "a"), (Operator, "+"), (LineComment, "--x")]
        );
    }

    #[test]
    fn unterminated_input_runs_to_end() {
        assert_eq!(kinds("'abc"), vec![(String, "'abc")]);
        assert_eq!(kinds("/* abc"), vec![(BlockComment, "/* abc")]);
        assert_eq!(kinds("$$ abc"), vec![(String, "$$ abc")]);
    }

    #[test]
    fn unicode_words_and_numbers() {
        assert_eq!(
            kinds("été 0x1F 1_000"),
            vec![(Word, "été"), (Number, "0x1F"), (Number, "1_000")]
        );
        // Like PostgreSQL, any non-ASCII byte may appear in an identifier.
        assert_eq!(kinds("→"), vec![(Word, "→")]);
    }

    #[test]
    fn tokens_cover_input() {
        let src = "SELECT $a$ é $a$, \"q\", 'x' -- end\n/* c */ 1.;";
        let toks = tokenize(src);
        assert_eq!(toks.first().unwrap().start, 0);
        assert_eq!(toks.last().unwrap().end, src.len());
        assert!(toks.windows(2).all(|w| w[0].end == w[1].start));
    }
}
