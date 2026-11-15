//! `E`: export the current result to a file (CSV, TSV, JSON, INSERT).
//!
//! Data tab: the whole query (filters and sort included, every row) is
//! written by the worker page by page. SQL tab: the rows of the result,
//! from memory. Visible columns only, in display order.

use crossterm::event::{KeyCode, KeyEvent};

use super::export::{self, CopyFormat, ExportSpec};
use super::input::{InputOutcome, LineInput};
use super::{App, Busy, Effect, Level, Overlay};
use crate::db::worker::{DbCommand, DbEvent};

#[derive(Debug, Clone)]
pub struct ExportDialog {
    pub format: usize,
    pub path: LineInput,
    /// The path was typed: changing the format keeps it.
    pub edited: bool,
    pub base: String,
    /// Rows that will be written, when known.
    pub rows: Option<String>,
    /// From a SQL result (in memory).
    pub result: bool,
}

impl ExportDialog {
    pub fn format(&self) -> CopyFormat {
        CopyFormat::ALL[self.format]
    }

    fn default_path(&self) -> String {
        format!("{}.{}", self.base, self.format().extension())
    }
}

/// `~/x` -> home directory.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

impl App {
    pub(super) fn open_export(&mut self) {
        if self.exporting.is_some() {
            self.notify(Level::Info, "Export déjà en cours…");
            return;
        }
        let name = self
            .data
            .object
            .as_ref()
            .map_or_else(|| "resultat".to_string(), |o| o.name.clone());
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let dir = self.export_dir.clone().unwrap_or_else(|| ".".into());
        let base = format!("{}/{safe}-{stamp}", dir.display());
        let rows = self
            .data
            .known_total()
            .map(|n| super::format::count(n as i64));
        let mut d = ExportDialog {
            format: 1, // CSV
            path: LineInput::default(),
            edited: false,
            base,
            rows,
            result: self.results_mode,
        };
        d.path = LineInput::with_text(d.default_path());
        self.overlay = Some(Overlay::Export(Box::new(d)));
    }

    pub(super) fn on_export_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Export(mut d)) = self.overlay.take() else {
            return Vec::new();
        };
        let n = CopyFormat::ALL.len();
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                d.format = if matches!(key.code, KeyCode::Up | KeyCode::BackTab) {
                    (d.format + n - 1) % n
                } else {
                    (d.format + 1) % n
                };
                if !d.edited {
                    d.path = LineInput::with_text(d.default_path());
                }
            }
            _ => match d.path.handle_key(&key) {
                InputOutcome::Cancel => return Vec::new(),
                InputOutcome::Submit => {
                    let path = d.path.text().trim().to_string();
                    if path.is_empty() {
                        self.overlay = Some(Overlay::Export(d));
                        return Vec::new();
                    }
                    let fmt = d.format();
                    let path = expand_home(&path);
                    return if d.result && self.results_mode {
                        self.export_result(fmt, path)
                    } else if d.result {
                        self.with_results(|app| app.export_result(fmt, path))
                            .unwrap_or_default()
                    } else {
                        self.export_data(fmt, path)
                    };
                }
                InputOutcome::Changed => d.edited = true,
                _ => {}
            },
        }
        self.overlay = Some(Overlay::Export(d));
        Vec::new()
    }

    fn export_spec(&self, format: CopyFormat, path: std::path::PathBuf) -> ExportSpec {
        let d = &self.data;
        let (schema, table) = d
            .object
            .as_ref()
            .map_or((String::new(), "resultat".to_string()), |o| {
                (o.schema.clone(), o.name.clone())
            });
        ExportSpec {
            format,
            path,
            columns: d
                .order
                .iter()
                .map(|i| (*i, d.columns[*i].name.clone(), d.columns[*i].kind))
                .collect(),
            schema,
            table,
        }
    }

    /// A SQL result: rows are in memory.
    fn export_result(&mut self, format: CopyFormat, path: std::path::PathBuf) -> Vec<Effect> {
        let spec = self.export_spec(format, path.clone());
        let mut text = spec.chunk(&self.data.rows, true);
        text.push_str(export::finish(format));
        let rows = self.data.rows.len();
        vec![Effect::WriteFile {
            path,
            text,
            done: format!("{rows} ligne(s) exportée(s) en {}", format.label()),
        }]
    }

    /// The Data tab: the whole query, written by the worker.
    fn export_data(&mut self, format: CopyFormat, path: std::path::PathBuf) -> Vec<Effect> {
        let spec = self.export_spec(format, path);
        self.request_seq += 1;
        let id = self.request_seq;
        self.exporting = Some(id);
        self.busy = Some(Busy {
            label: "export".into(),
            since: std::time::Instant::now(),
        });
        let request = Box::new(self.data.request(0, 0));
        vec![
            DbCommand::Export {
                id,
                request,
                spec: Box::new(spec),
            }
            .into(),
        ]
    }

    pub(super) fn on_export_event(&mut self, ev: DbEvent) {
        match ev {
            DbEvent::ExportProgress { id, rows } if self.exporting == Some(id) => {
                if let Some(b) = &mut self.busy {
                    b.label = format!("export : {} lignes", super::format::count(rows as i64));
                }
            }
            DbEvent::ExportDone {
                id,
                rows,
                path,
                elapsed,
            } if self.exporting == Some(id) => {
                self.exporting = None;
                self.busy = None;
                self.last_op = Some(super::LastOp {
                    label: "export".into(),
                    elapsed,
                });
                self.notify(
                    Level::Success,
                    format!(
                        "{} ligne(s) exportée(s) : {}",
                        super::format::count(rows as i64),
                        path.display()
                    ),
                );
            }
            DbEvent::ExportFailed { id, error } if self.exporting == Some(id) => {
                self.exporting = None;
                self.busy = None;
                self.notify(Level::Error, format!("Export impossible : {error}"));
            }
            _ => {}
        }
    }
}
