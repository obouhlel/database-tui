//! Text formats for copying and exporting rows: TSV, CSV, JSON, INSERT
//! statements. Exports are written in chunks ([`render_chunk`], [`finish`])
//! so that a large table never sits in memory.

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

    /// File extension for exports.
    pub fn extension(self) -> &'static str {
        match self {
            CopyFormat::Tsv => "tsv",
            CopyFormat::Csv => "csv",
            CopyFormat::Json => "json",
            CopyFormat::Insert => "sql",
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

/// What an export of the Data tab writes: the visible columns (index in
/// the request, name, kind), in display order.
#[derive(Debug, Clone)]
pub struct ExportSpec {
    pub format: CopyFormat,
    pub path: std::path::PathBuf,
    pub columns: Vec<(usize, String, ValueKind)>,
    pub schema: String,
    pub table: String,
}

impl ExportSpec {
    /// The chunk for a page of rows.
    pub fn chunk(&self, rows: &[Vec<Option<String>>], first: bool) -> String {
        let e = Extract {
            schema: &self.schema,
            table: &self.table,
            columns: self
                .columns
                .iter()
                .map(|(_, n, k)| (n.as_str(), *k))
                .collect(),
            rows: rows
                .iter()
                .map(|r| {
                    self.columns
                        .iter()
                        .map(|(i, _, _)| r.get(*i).and_then(|v| v.as_deref()))
                        .collect()
                })
                .collect(),
        };
        render_chunk(self.format, &e, first)
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

/// One chunk of an export file; `first` for the first chunk (header,
/// opening bracket). Ends with a newline when not empty.
pub fn render_chunk(fmt: CopyFormat, e: &Extract, first: bool) -> String {
    match fmt {
        CopyFormat::Tsv | CopyFormat::Csv => {
            if e.rows.is_empty() && !first {
                return String::new();
            }
            let d = if fmt == CopyFormat::Tsv { b'\t' } else { b',' };
            let mut s = delimited(e, d, first);
            if !s.is_empty() {
                s.push('\n');
            }
            s
        }
        CopyFormat::Json => {
            let mut s = String::new();
            if first {
                s.push('[');
            }
            for (i, row) in e.rows.iter().enumerate() {
                let mut obj = Map::new();
                for ((name, kind), v) in e.columns.iter().zip(row) {
                    obj.insert((*name).to_string(), json_value(*kind, *v));
                }
                if !(first && i == 0) {
                    s.push(',');
                }
                s.push_str("\n  ");
                s.push_str(&serde_json::to_string(&Json::Object(obj)).unwrap_or_default());
            }
            s
        }
        CopyFormat::Insert => {
            let mut s = insert(e);
            if !s.is_empty() {
                s.push('\n');
            }
            s
        }
    }
}

/// End of an export file.
pub fn finish(fmt: CopyFormat) -> &'static str {
    match fmt {
        CopyFormat::Json => "\n]\n",
        _ => "",
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
    fn chunks_make_valid_files() {
        let e = sample();
        for fmt in CopyFormat::ALL {
            let mut out = render_chunk(fmt, &e, true);
            out.push_str(&render_chunk(fmt, &e, false));
            out.push_str(finish(fmt));
            match fmt {
                CopyFormat::Json => {
                    let v: Json = serde_json::from_str(&out).unwrap();
                    assert_eq!(v.as_array().unwrap().len(), 4);
                }
                CopyFormat::Csv | CopyFormat::Tsv => {
                    // Header once, 4 records (one has an embedded tab, quoted).
                    let mut r = csv::ReaderBuilder::new()
                        .delimiter(if fmt == CopyFormat::Tsv { b'\t' } else { b',' })
                        .from_reader(out.as_bytes());
                    assert_eq!(r.headers().unwrap().len(), 5);
                    assert_eq!(r.records().count(), 4);
                }
                CopyFormat::Insert => assert_eq!(out.lines().count(), 4),
            }
        }
        let empty = Extract {
            rows: vec![],
            ..sample()
        };
        let json = render_chunk(CopyFormat::Json, &empty, true) + finish(CopyFormat::Json);
        assert_eq!(
            serde_json::from_str::<Json>(&json).unwrap(),
            Json::Array(vec![])
        );
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
