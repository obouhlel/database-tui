//! Engine-neutral description of the database objects shown in the tree.

/// Kind of a database object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    PartitionedTable,
    ForeignTable,
    View,
    MaterializedView,
    Sequence,
    Function,
    Procedure,
}

impl ObjectKind {
    pub fn group(self) -> ObjectGroup {
        match self {
            ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => {
                ObjectGroup::Tables
            }
            ObjectKind::View => ObjectGroup::Views,
            ObjectKind::MaterializedView => ObjectGroup::MaterializedViews,
            ObjectKind::Sequence => ObjectGroup::Sequences,
            ObjectKind::Function | ObjectKind::Procedure => ObjectGroup::Functions,
        }
    }

    /// Objects whose rows can be browsed in the Data tab.
    pub fn has_rows(self) -> bool {
        matches!(
            self,
            ObjectKind::Table
                | ObjectKind::PartitionedTable
                | ObjectKind::ForeignTable
                | ObjectKind::View
                | ObjectKind::MaterializedView
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ObjectKind::Table => "table",
            ObjectKind::PartitionedTable => "partitioned table",
            ObjectKind::ForeignTable => "foreign table",
            ObjectKind::View => "view",
            ObjectKind::MaterializedView => "materialized view",
            ObjectKind::Sequence => "sequence",
            ObjectKind::Function => "function",
            ObjectKind::Procedure => "procedure",
        }
    }
}

/// Folder under a schema in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectGroup {
    Tables,
    Views,
    MaterializedViews,
    Sequences,
    Functions,
}

impl ObjectGroup {
    pub const ALL: [ObjectGroup; 5] = [
        ObjectGroup::Tables,
        ObjectGroup::Views,
        ObjectGroup::MaterializedViews,
        ObjectGroup::Sequences,
        ObjectGroup::Functions,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectGroup::Tables => "Tables",
            ObjectGroup::Views => "Views",
            ObjectGroup::MaterializedViews => "Materialized views",
            ObjectGroup::Sequences => "Sequences",
            ObjectGroup::Functions => "Functions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbObject {
    pub name: String,
    pub kind: ObjectKind,
    pub comment: Option<String>,
    /// Identity arguments for functions (`integer, text`).
    pub signature: Option<String>,
}

impl DbObject {
    /// Name shown in the tree (`fn(integer, text)` for functions).
    pub fn display_name(&self) -> String {
        match &self.signature {
            Some(sig) => format!("{}({sig})", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    pub comment: Option<String>,
    pub is_system: bool,
    /// Sorted by kind then name.
    pub objects: Vec<DbObject>,
}

impl Schema {
    pub fn objects_in(&self, group: ObjectGroup) -> impl Iterator<Item = (usize, &DbObject)> {
        self.objects
            .iter()
            .enumerate()
            .filter(move |(_, o)| o.kind.group() == group)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub schemas: Vec<Schema>,
}

impl Catalog {
    pub fn object_count(&self) -> usize {
        self.schemas.iter().map(|s| s.objects.len()).sum()
    }
}

/// Stable reference to an object (survives catalog reloads).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectRef {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub signature: Option<String>,
}

impl ObjectRef {
    pub fn new(schema: &str, obj: &DbObject) -> Self {
        Self {
            schema: schema.to_string(),
            name: obj.name.clone(),
            kind: obj.kind,
            signature: obj.signature.clone(),
        }
    }

    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}
