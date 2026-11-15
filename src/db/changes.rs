//! Engine-neutral description of staged changes (Data tab edits) and of the
//! outcome of applying them. Each backend turns a [`ChangeSet`] into its own
//! statements; values always travel as bind parameters.

use super::catalog::ObjectRef;
use super::data::ColumnSpec;

/// New value of a cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NewValue {
    Null,
    /// Text representation, cast server-side to the column type.
    Text(String),
    /// The column default (`DEFAULT`).
    Default,
}

impl NewValue {
    pub fn from_text(v: Option<&str>) -> Self {
        match v {
            Some(s) => NewValue::Text(s.to_string()),
            None => NewValue::Null,
        }
    }

    /// Display form (`NULL`, `DEFAULT`, or the text).
    pub fn display(&self) -> &str {
        match self {
            NewValue::Null => "NULL",
            NewValue::Default => "DEFAULT",
            NewValue::Text(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Rows are identified by `key` (column, value as text). The update only
    /// applies if every `expect`ed column still has its old text value
    /// (`None` = NULL): otherwise it is a conflict.
    Update {
        key: Vec<(String, String)>,
        set: Vec<(String, NewValue)>,
        expect: Vec<(String, Option<String>)>,
    },
    /// Columns left out take their default.
    Insert { values: Vec<(String, NewValue)> },
    /// `expect_version`: row version (`xmin`) read with the row, if any.
    Delete {
        key: Vec<(String, String)>,
        expect_version: Option<String>,
    },
}

impl Change {
    pub fn verb(&self) -> &'static str {
        match self {
            Change::Update { .. } => "UPDATE",
            Change::Insert { .. } => "INSERT",
            Change::Delete { .. } => "DELETE",
        }
    }

    /// Updates and deletes must touch exactly one row.
    pub fn checks_one_row(&self) -> bool {
        !matches!(self, Change::Insert { .. })
    }
}

/// Changes for one table, in execution order (deletes, updates, inserts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub object: ObjectRef,
    pub columns: Vec<ColumnSpec>,
    pub changes: Vec<Change>,
}

impl ChangeSet {
    pub fn data_type(&self, column: &str) -> &str {
        self.columns
            .iter()
            .find(|c| c.name == column)
            .map_or("text", |c| c.data_type.as_str())
    }

    pub fn counts(&self) -> ChangeCounts {
        let mut c = ChangeCounts::default();
        for ch in &self.changes {
            match ch {
                Change::Update { .. } => c.updated += 1,
                Change::Insert { .. } => c.inserted += 1,
                Change::Delete { .. } => c.deleted += 1,
            }
        }
        c
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeCounts {
    pub updated: u64,
    pub inserted: u64,
    pub deleted: u64,
}

impl ChangeCounts {
    pub fn total(&self) -> u64 {
        self.updated + self.inserted + self.deleted
    }

    /// `2 UPDATE, 1 INSERT` (empty kinds left out).
    pub fn describe(&self) -> String {
        let parts: Vec<String> = [
            (self.updated, "UPDATE"),
            (self.inserted, "INSERT"),
            (self.deleted, "DELETE"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, v)| format!("{n} {v}"))
        .collect();
        if parts.is_empty() {
            "aucun changement".into()
        } else {
            parts.join(", ")
        }
    }
}

/// Why a change set was rolled back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// Change `index` matched no row: the row changed or disappeared since
    /// it was read.
    #[error("conflit sur le changement {} : la ligne a été modifiée ou supprimée entre-temps", index + 1)]
    Conflict { index: usize },
    /// The server refused statement `index` (`None`: BEGIN / COMMIT).
    #[error("{message}")]
    Failed {
        index: Option<usize>,
        message: String,
    },
}
