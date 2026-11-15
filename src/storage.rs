//! Files of the SQL tab in the data directory (`~/.local/share/pgtui`):
//! the history (`history`, one JSON object per line) and saved queries
//! (`queries/<name>.sql`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::sql_tab::{HistoryEntry, MAX_HISTORY};

pub fn history_path(data_dir: &Path) -> PathBuf {
    data_dir.join("history")
}

pub fn queries_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("queries")
}

/// The last [`MAX_HISTORY`] entries; unreadable lines are skipped.
pub fn load_history(path: &Path) -> Vec<HistoryEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries: Vec<HistoryEntry> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if entries.len() > MAX_HISTORY {
        entries.drain(..entries.len() - MAX_HISTORY);
        // Rewrite the file so that it does not grow forever.
        let body: String = entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .map(|l| l + "\n")
            .collect();
        let _ = fs::write(path, body);
    }
    entries
}

pub fn append_history(path: &Path, entry: &HistoryEntry) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("ouverture de {}", path.display()))?;
    #[cfg(unix)]
    {
        // Statements may contain sensitive data.
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

pub fn save_query(dir: &Path, name: &str, text: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.sql"));
    let mut body = text.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(&path, body).with_context(|| format!("écriture de {}", path.display()))?;
    Ok(path)
}

pub fn load_query(dir: &Path, name: &str) -> Result<String> {
    let path = dir.join(format!("{name}.sql"));
    let text =
        fs::read_to_string(&path).with_context(|| format!("lecture de {}", path.display()))?;
    Ok(text.strip_suffix('\n').unwrap_or(&text).to_string())
}

/// Names of the saved queries, sorted.
pub fn list_queries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "sql" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str) -> HistoryEntry {
        HistoryEntry {
            at: 1,
            connection: "c".into(),
            database: "d".into(),
            sql: sql.into(),
            ok: true,
            ms: 3,
        }
    }

    #[test]
    fn history_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = history_path(dir.path());
        assert!(load_history(&path).is_empty());
        append_history(&path, &entry("select 1")).unwrap();
        append_history(&path, &entry("select\n2")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"garbage\n")
            .unwrap();
        let h = load_history(&path);
        assert_eq!(
            h.iter().map(|e| e.sql.as_str()).collect::<Vec<_>>(),
            ["select 1", "select\n2"]
        );
    }

    #[test]
    fn saved_queries() {
        let dir = tempfile::tempdir().unwrap();
        let q = queries_dir(dir.path());
        assert!(list_queries(&q).is_empty());
        save_query(&q, "rapport mensuel", "select 1;").unwrap();
        save_query(&q, "a", "select 2").unwrap();
        assert_eq!(list_queries(&q), ["a", "rapport mensuel"]);
        assert_eq!(load_query(&q, "rapport mensuel").unwrap(), "select 1;");
        assert!(load_query(&q, "nope").is_err());
    }
}
