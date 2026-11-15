//! Engine-neutral description of a Data tab query (table, filters, sort,
//! window) and of its result. Each backend turns a [`DataRequest`] into its
//! own SQL; user values always travel as bind parameters.

use super::catalog::ObjectRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    ILike,
    /// Regular expression, case sensitive (`~`).
    Regex,
    /// Regular expression, case insensitive (`~*`).
    IRegex,
    IsNull,
    IsNotNull,
}

impl Op {
    pub const ALL: [Op; 12] = [
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::Like,
        Op::ILike,
        Op::Regex,
        Op::IRegex,
        Op::IsNull,
        Op::IsNotNull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Like => "LIKE",
            Op::ILike => "ILIKE",
            Op::Regex => "~",
            Op::IRegex => "~*",
            Op::IsNull => "IS NULL",
            Op::IsNotNull => "IS NOT NULL",
        }
    }

    /// Whether the operator takes a value.
    pub fn has_value(self) -> bool {
        !matches!(self, Op::IsNull | Op::IsNotNull)
    }

    /// Pattern operators compare the text representation of the column.
    pub fn is_textual(self) -> bool {
        matches!(self, Op::Like | Op::ILike | Op::Regex | Op::IRegex)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub column: String,
    pub op: Op,
    /// Ignored for `IS [NOT] NULL`.
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Combinator {
    #[default]
    And,
    Or,
}

impl Combinator {
    pub fn keyword(self) -> &'static str {
        match self {
            Combinator::And => "AND",
            Combinator::Or => "OR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub dir: SortDir,
}

/// A column of the table, with its SQL type (used to cast parameters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub conditions: Vec<Condition>,
    pub combinator: Combinator,
    /// Free SQL condition typed by the user (`WHERE ...` without the keyword).
    pub free_where: Option<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty() && self.free_where.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRequest {
    pub object: ObjectRef,
    pub columns: Vec<ColumnSpec>,
    pub filter: Filter,
    pub sort: Vec<SortKey>,
    /// Appended to the sort for a deterministic order (primary key).
    pub tiebreak: Vec<String>,
    /// Use the physical row order (`ctid`) when nothing else orders rows.
    pub order_by_ctid: bool,
    /// Also return the row version (`xmin`), for conflict detection.
    pub with_row_version: bool,
    pub offset: u64,
    pub limit: u64,
}

/// Rows with every value in its text representation (`None` = SQL NULL).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Result of a bounded count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCount {
    Exact(u64),
    /// More rows than the counting bound.
    AtLeast(u64),
}
