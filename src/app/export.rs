//! Text formats for copying rows: TSV, CSV, JSON, INSERT statements.

use serde_json::{Map, Value as Json};

use super::value::ValueKind;
use crate::sql::{quote_ident, quote_literal, quote_qualified};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    Tsv,
    Csv,
    Json,
    Insert,
}

impl CopyFormat {
    pub const ALL: [CopyFormat; 4] = [
        CopyFormat::Tsv,
        CopyFormat::Csv,
        CopyFormat::Json,
        CopyFormat::Insert,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::Tsv => "TSV (tableur)",
            CopyFormat::Csv => "CSV",
            CopyFormat::Json => "JSON",
            CopyFormat::Insert => "INSERT SQL",
        }
    }

    /// Single-key shortcut in the format chooser.
    pub fn key(self) -> char {
        match self {
            CopyFormat::Tsv => 't',
            CopyFormat::Csv => 'c',
            CopyFormat::Json => 'j',
            CopyFormat::Insert => 'i',
        }
    }
}

/// Columns and rows to export (values as text, `None` = NULL).
pub struct Extract<'a> {
    pub schema: &'a str,
    pub table: &'a str,
    pub columns: Vec<(&'a str, ValueKind)>,
    pub rows: Vec<Vec<Option<&'a str>>>,
}

pub fn render(fmt: CopyFormat, e: &Extract, header: bool) -> String {
    match fmt {
        CopyFormat::Tsv => delimited(e, b'\t', header),
        CopyFormat::Csv => delimited(e, b',', header),
        CopyFormat::Json => json(e),
        CopyFormat::Insert => insert(e),
    }
}

/// NULL becomes an empty field (spreadsheets have no NULL).
fn delimited(e: &Extract, delimiter: u8, header: bool) -> String {
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    if header {
        let _ = w.write_record(e.columns.iter().map(|(n, _)| *n));
    }
    for row in &e.rows {
        let _ = w.write_record(row.iter().map(|v| v.unwrap_or("")));
    }
    let bytes = w.into_inner().unwrap_or_default();
    let mut s = String::from_utf8(bytes).unwrap_or_default();
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

fn json_value(kind: ValueKind, v: Option<&str>) -> Json {
    let Some(v) = v else { return Json::Null };
    match kind {
        ValueKind::Bool => Json::Bool(v == "t"),
        ValueKind::Integer => v
            .parse::<i64>()
            .map_or_else(|_| Json::String(v.into()), Json::from),
        ValueKind::Float => v
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Json::String(v.into()), Json::Number),
        ValueKind::Json => serde_json::from_str(v).unwrap_or_else(|_| Json::String(v.into())),
        // numeric stays a string: JSON numbers would lose precision.
        _ => Json::String(v.into()),
    }
}

fn json(e: &Extract) -> String {
    let rows: Vec<Json> = e
        .rows
        .iter()
        .map(|row| {
            let mut obj = Map::new();
            for ((name, kind), v) in e.columns.iter().zip(row) {
                obj.insert((*name).to_string(), json_value(*kind, *v));
            }
            Json::Object(obj)
        })
        .collect();
    serde_json::to_string_pretty(&Json::Array(rows)).unwrap_or_default()
}

/// For display / copy only: values are quoted literals, never executed by pgtui.
fn insert(e: &Extract) -> String {
    let table = quote_qualified(e.schema, e.table);
    let cols: Vec<String> = e.columns.iter().map(|(n, _)| quote_ident(n)).collect();
    e.rows
        .iter()
        .map(|row| {
            let values: Vec<String> = row
                .iter()
                .map(|v| v.map_or_else(|| "NULL".to_string(), quote_literal))
                .collect();
            format!(
                "INSERT INTO {table} ({}) VALUES ({});",
                cols.join(", "),
                values.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sample() -> Extract<'static> {
        Extract {
            schema: "shop",
            table: "orders",
            columns: vec![
                ("id", ValueKind::Integer),
                ("note", ValueKind::Text),
                ("ok", ValueKind::Bool),
                ("data", ValueKind::Json),
                ("total", ValueKind::Numeric),
            ],
            rows: vec![
                vec![
                    Some("1"),
                    Some("a, \"b\"\ttab"),
                    Some("t"),
                    Some(r#"{"k":1}"#),
                    Some("10.50"),
                ],
                vec![Some("2"), None, Some("f"), None, Some("0")],
            ],
        }
    }

    #[test]
    fn tsv_and_csv_quote_properly() {
        let e = sample();
        assert_eq!(
            render(CopyFormat::Tsv, &e, true),
            "id\tnote\tok\tdata\ttotal\n1\t\"a, \"\"b\"\"\ttab\"\tt\t\"{\"\"k\"\":1}\"\t10.50\n2\t\tf\t\t0"
        );
        assert_eq!(
            render(CopyFormat::Csv, &e, false),
            "1,\"a, \"\"b\"\"\ttab\",t,\"{\"\"k\"\":1}\",10.50\n2,,f,,0"
        );
    }

    #[test]
    fn json_keeps_types_and_nulls() {
        let v: Json = serde_json::from_str(&render(CopyFormat::Json, &sample(), true)).unwrap();
        assert_eq!(v[0]["id"], Json::from(1));
        assert_eq!(v[0]["ok"], Json::Bool(true));
        assert_eq!(v[0]["data"]["k"], Json::from(1));
        assert_eq!(v[0]["total"], Json::String("10.50".into()));
        assert_eq!(v[1]["note"], Json::Null);
    }

    #[test]
    fn insert_statements() {
        assert_eq!(
            render(CopyFormat::Insert, &sample(), true),
            "INSERT INTO shop.orders (id, note, ok, data, total) VALUES ('1', 'a, \"b\"\ttab', 't', '{\"k\":1}', '10.50');\n\
             INSERT INTO shop.orders (id, note, ok, data, total) VALUES ('2', NULL, 'f', NULL, '0');"
        );
    }
}
