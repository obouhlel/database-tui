//! Results of statements run from the SQL tab.

/// A result column: name and type name as the engine reports it (`int4`,
/// `numeric`, `_text` for arrays), empty when unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatementResult {
    /// Empty for a statement that returns no rows (`UPDATE` without
    /// `RETURNING`, `CREATE`, ...).
    pub columns: Vec<ResultColumn>,
    /// Every value as text (`None` = NULL), at most `max_rows`.
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows were returned than kept.
    pub truncated: bool,
    /// Rows returned or affected, as reported by the server.
    pub row_count: u64,
    /// The statement returns rows (a `SELECT` with no row still does).
    pub returns_rows: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ExecError {
    pub message: String,
    /// Position of the error in the statement (1-based, in characters).
    pub position: Option<u32>,
}
