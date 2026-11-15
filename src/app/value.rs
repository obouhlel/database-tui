//! Display of values received as text, according to the column type.

/// Display family of a PostgreSQL type (from `format_type` output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Bool,
    Integer,
    Float,
    Numeric,
    Json,
    Bytea,
    Array,
    Temporal,
    Text,
    Other,
}

impl ValueKind {
    pub fn of(data_type: &str) -> Self {
        let t = data_type.trim();
        if t.ends_with("[]") {
            return ValueKind::Array;
        }
        let base = t.split('(').next().unwrap_or(t).trim();
        match base {
            "boolean" => ValueKind::Bool,
            "smallint" | "integer" | "bigint" | "oid" => ValueKind::Integer,
            "real" | "double precision" => ValueKind::Float,
            "numeric" | "money" => ValueKind::Numeric,
            "json" | "jsonb" => ValueKind::Json,
            "bytea" => ValueKind::Bytea,
            "text" | "character varying" | "character" | "name" | "citext" | "uuid" => {
                ValueKind::Text
            }
            b if b.starts_with("timestamp")
                || b.starts_with("time")
                || b == "date"
                || b == "interval" =>
            {
                ValueKind::Temporal
            }
            _ => ValueKind::Other,
        }
    }

    /// From an engine type name (`int4`, `_text`, `timestamptz`, ...), as
    /// reported for query results.
    pub fn of_engine(name: &str) -> Self {
        if name.starts_with('_') {
            return ValueKind::Array;
        }
        match name {
            "bool" => ValueKind::Bool,
            "int2" | "int4" | "int8" | "oid" => ValueKind::Integer,
            "float4" | "float8" => ValueKind::Float,
            "numeric" | "money" => ValueKind::Numeric,
            "json" | "jsonb" => ValueKind::Json,
            "bytea" => ValueKind::Bytea,
            "text" | "varchar" | "bpchar" | "name" | "citext" | "uuid" => ValueKind::Text,
            "date" | "time" | "timetz" | "timestamp" | "timestamptz" | "interval" => {
                ValueKind::Temporal
            }
            _ => ValueKind::Other,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ValueKind::Integer | ValueKind::Float | ValueKind::Numeric
        )
    }
}

/// One-line cell text (the grid shows the first line and truncates).
pub fn cell_text(kind: ValueKind, value: &str) -> &str {
    match (kind, value) {
        (ValueKind::Bool, "t") => "true",
        (ValueKind::Bool, "f") => "false",
        _ => value,
    }
}

/// Maximum number of bytea bytes shown in the detail view.
const BYTEA_MAX: usize = 256;

/// Multi-line rendering for the row detail view.
pub fn pretty(kind: ValueKind, value: &str) -> String {
    match kind {
        ValueKind::Json => serde_json::from_str::<serde_json::Value>(value)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|_| value.to_string()),
        ValueKind::Bytea => pretty_bytea(value),
        ValueKind::Bool => cell_text(kind, value).to_string(),
        _ => value.to_string(),
    }
}

/// `\x0a0b...` -> grouped hex, 32 bytes per line, truncated.
fn pretty_bytea(value: &str) -> String {
    let Some(hex) = value.strip_prefix("\\x") else {
        return value.to_string();
    };
    let len = hex.len() / 2;
    let shown = len.min(BYTEA_MAX);
    let mut out = format!("{len} octets\n");
    for line in hex.as_bytes()[..shown * 2].chunks(64) {
        let groups: Vec<&str> = line
            .chunks(8)
            .map(|g| std::str::from_utf8(g).unwrap_or(""))
            .collect();
        out.push_str(&groups.join(" "));
        out.push('\n');
    }
    if len > shown {
        out.push_str(&format!("… {} octets non affichés", len - shown));
    } else {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(ValueKind::of("bigint"), ValueKind::Integer);
        assert_eq!(ValueKind::of("numeric(12,2)"), ValueKind::Numeric);
        assert_eq!(ValueKind::of("double precision"), ValueKind::Float);
        assert_eq!(ValueKind::of("jsonb"), ValueKind::Json);
        assert_eq!(ValueKind::of("text[]"), ValueKind::Array);
        assert_eq!(
            ValueKind::of("timestamp with time zone"),
            ValueKind::Temporal
        );
        assert_eq!(
            ValueKind::of("time(3) without time zone"),
            ValueKind::Temporal
        );
        assert_eq!(ValueKind::of("interval"), ValueKind::Temporal);
        assert_eq!(ValueKind::of("character varying(32)"), ValueKind::Text);
        assert_eq!(ValueKind::of("shop.order_status"), ValueKind::Other);
        assert_eq!(ValueKind::of("inet"), ValueKind::Other);
    }

    #[test]
    fn booleans() {
        assert_eq!(cell_text(ValueKind::Bool, "t"), "true");
        assert_eq!(cell_text(ValueKind::Text, "t"), "t");
    }

    #[test]
    fn json_is_indented() {
        assert_eq!(
            pretty(ValueKind::Json, r#"{"a": [1, 2]}"#),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(pretty(ValueKind::Json, "not json"), "not json");
    }

    #[test]
    fn bytea_is_grouped_and_truncated() {
        assert_eq!(
            pretty(ValueKind::Bytea, "\\x0011223344556677"),
            "8 octets\n00112233 44556677"
        );
        let long = format!("\\x{}", "ab".repeat(300));
        let p = pretty(ValueKind::Bytea, &long);
        assert!(p.starts_with("300 octets\nabababab "));
        assert!(p.ends_with("… 44 octets non affichés"), "{p}");
        assert_eq!(p.lines().count(), 1 + 8 + 1);
    }
}
