//! Engine-neutral details of one object, shown in the Schema tab.

use chrono::{DateTime, Utc};

use super::catalog::ObjectRef;

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectDetails {
    pub object: ObjectRef,
    pub owner: Option<String>,
    pub comment: Option<String>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub constraints: Vec<Constraint>,
    pub triggers: Vec<Trigger>,
    /// Outgoing foreign keys (this table references others).
    pub foreign_keys: Vec<ForeignKey>,
    /// Incoming foreign keys (other tables reference this one).
    pub referenced_by: Vec<ForeignKey>,
    pub stats: Option<Stats>,
    /// Free-form `(label, value)` pairs: sequence parameters, function
    /// language, partition key, ...
    pub properties: Vec<(String, String)>,
    /// Complete DDL (`CREATE TABLE ...`, `CREATE FUNCTION ...`).
    pub ddl: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    Always,
    ByDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generated {
    Stored,
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// Formatted type, e.g. `numeric(12,2)`, `shop.order_status`.
    pub data_type: String,
    pub nullable: bool,
    /// Default expression, or generation expression when `generated` is set.
    pub default: Option<String>,
    pub identity: Option<Identity>,
    pub generated: Option<Generated>,
    /// Non-default collation, already quoted.
    pub collation: Option<String>,
    pub comment: Option<String>,
    pub primary_key: bool,
    /// Target of a single-column foreign key: `shop.customers(id)`.
    pub references: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    /// Access method: `btree`, `gin`, ...
    pub method: String,
    /// Key columns or expressions, with options (`ordered_at DESC`).
    pub columns: Vec<String>,
    /// `INCLUDE` columns.
    pub include: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    pub predicate: Option<String>,
    /// Full `CREATE INDEX` statement.
    pub definition: String,
    pub size_bytes: Option<i64>,
    /// Backs a PRIMARY KEY / UNIQUE / EXCLUDE constraint (created with it).
    pub constraint_backed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
    Exclusion,
    Trigger,
}

impl ConstraintKind {
    pub fn from_pg(c: &str) -> Option<Self> {
        Some(match c {
            "p" => Self::PrimaryKey,
            "u" => Self::Unique,
            "f" => Self::ForeignKey,
            "c" => Self::Check,
            "x" => Self::Exclusion,
            "t" => Self::Trigger,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PrimaryKey => "PRIMARY KEY",
            Self::Unique => "UNIQUE",
            Self::ForeignKey => "FOREIGN KEY",
            Self::Check => "CHECK",
            Self::Exclusion => "EXCLUDE",
            Self::Trigger => "TRIGGER",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub name: String,
    pub kind: ConstraintKind,
    /// `PRIMARY KEY (id)`, `CHECK (price > 0)`, ...
    pub definition: String,
    pub columns: Vec<String>,
    pub deferrable: bool,
    pub initially_deferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub name: String,
    /// `BEFORE`, `AFTER`, `INSTEAD OF`
    pub timing: String,
    /// `INSERT OR UPDATE`
    pub events: String,
    /// `ROW` or `STATEMENT`
    pub level: String,
    pub function: String,
    pub enabled: bool,
    /// Full `CREATE TRIGGER` statement.
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FkAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl FkAction {
    pub fn from_pg(c: &str) -> Self {
        match c {
            "r" => Self::Restrict,
            "c" => Self::Cascade,
            "n" => Self::SetNull,
            "d" => Self::SetDefault,
            _ => Self::NoAction,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub from: ObjectRef,
    pub from_columns: Vec<String>,
    pub to: ObjectRef,
    pub to_columns: Vec<String>,
    pub on_update: FkAction,
    pub on_delete: FkAction,
    pub origin: FkOrigin,
}

/// Where a foreign key comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FkOrigin {
    /// A constraint of the database.
    #[default]
    Declared,
    /// A reference the database does not enforce, declared in
    /// `relations.toml` (see [`crate::config::relations`]).
    Virtual {
        /// Free-form nature: `souple`, `cross_base`, `polymorphe`, ...
        kind: String,
        /// SQL condition on the source row, for a polymorphic reference.
        when: Option<String>,
        note: Option<String>,
    },
}

impl ForeignKey {
    pub fn is_virtual(&self) -> bool {
        matches!(self.origin, FkOrigin::Virtual { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stats {
    /// Planner estimate; `None` if the table was never analyzed.
    pub estimated_rows: Option<i64>,
    pub total_bytes: i64,
    pub table_bytes: i64,
    pub index_bytes: i64,
    pub toast_bytes: i64,
    pub live_tuples: Option<i64>,
    pub dead_tuples: Option<i64>,
    pub seq_scans: Option<i64>,
    pub index_scans: Option<i64>,
    pub last_vacuum: Option<DateTime<Utc>>,
    pub last_autovacuum: Option<DateTime<Utc>>,
    pub last_analyze: Option<DateTime<Utc>>,
    pub last_autoanalyze: Option<DateTime<Utc>>,
}
