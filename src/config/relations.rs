//! Virtual relations: `~/.config/pgtui/relations.toml`, next to `config.toml`.
//!
//! References the database does not enforce: a column that points to a table
//! of another database, an id kept without a constraint on purpose, a
//! polymorphic `(kind, id)` pair. They are merged into the foreign keys of an
//! object when its details load, so the Schema tab lists them in Relations
//! and `Enter` follows them like a declared foreign key.
//!
//! ```toml
//! [[relation]]
//! from = { database = "api_core", table = "public.factures", columns = ["client_id"] }
//! to = { database = "api_core", table = "public.entreprises", columns = ["id"] }
//! kind = "polymorphe"
//! when = "client_type = 'entreprise'"
//! note = "client polymorphe"
//! ```
//!
//! `database` matches the database of the same name, and also a suffixed one
//! (`api_core_dev`, `api_core_test`): the suffix carries over to the other
//! end, so on a server holding `api_core_dev` and `auth_dev` a relation from
//! `api_core` to `auth` links those two. `table` without a schema means
//! `public`. The file is optional; an invalid file is reported as a warning
//! and ignored.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::db::catalog::{ObjectKind, ObjectRef};
use crate::db::schema::{FkAction, FkOrigin, ForeignKey, ObjectDetails};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub database: String,
    /// `schema.table`, or `table` for `public.table`.
    pub table: String,
    pub columns: Vec<String>,
}

impl Endpoint {
    fn schema_and_name(&self) -> (&str, &str) {
        self.table
            .split_once('.')
            .unwrap_or(("public", &self.table))
    }

    /// Suffix of `database` relative to this endpoint's database: `""` for
    /// the same name, `"_dev"` for `api_core_dev`; `None` if unrelated.
    fn suffix<'a>(&self, database: &'a str) -> Option<&'a str> {
        let rest = database.strip_prefix(self.database.as_str())?;
        (rest.is_empty() || rest.starts_with('_')).then_some(rest)
    }

    fn is<'a>(&self, obj: &'a ObjectRef) -> Option<&'a str> {
        let (schema, name) = self.schema_and_name();
        if schema != obj.schema || name != obj.name {
            return None;
        }
        self.suffix(&obj.database)
    }

    /// This endpoint as an object of `connection`, on the database carrying
    /// the same suffix as the other end.
    fn object(&self, connection: &str, suffix: &str) -> ObjectRef {
        let (schema, name) = self.schema_and_name();
        ObjectRef {
            connection: connection.to_string(),
            database: format!("{}{suffix}", self.database),
            schema: schema.to_string(),
            name: name.to_string(),
            kind: ObjectKind::Table,
            signature: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub from: Endpoint,
    pub to: Endpoint,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_kind() -> String {
    "virtuelle".into()
}

impl Relation {
    fn foreign_key(&self, from: ObjectRef, to: ObjectRef) -> ForeignKey {
        ForeignKey {
            name: format!("~ {}", self.kind),
            from,
            from_columns: self.from.columns.clone(),
            to,
            to_columns: self.to.columns.clone(),
            on_update: FkAction::NoAction,
            on_delete: FkAction::NoAction,
            origin: FkOrigin::Virtual {
                kind: self.kind.clone(),
                when: self.when.clone(),
                note: self.note.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relations {
    #[serde(default, rename = "relation")]
    pub list: Vec<Relation>,
}

impl Relations {
    /// `relations.toml` in the directory of `config_file`.
    pub fn path_for(config_file: &Path) -> PathBuf {
        config_file.with_file_name("relations.toml")
    }

    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads the file if it exists. A missing file is not an error; an
    /// unreadable or invalid one yields no relations and a warning.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("{} ignoré : {e}", path.display())),
                );
            }
        };
        match Self::parse(&text) {
            Ok(r) => (r, None),
            Err(e) => (
                Self::default(),
                Some(format!("{} ignoré : {e}", path.display())),
            ),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Adds the virtual relations of `details.object`: outgoing ones to
    /// `foreign_keys`, incoming ones to `referenced_by`, and the target of a
    /// single-column outgoing one to the column's `references`.
    pub fn apply(&self, details: &mut ObjectDetails) {
        let obj = details.object.clone();
        if !obj.kind.has_rows() {
            return;
        }
        for rel in &self.list {
            if let Some(suffix) = rel.from.is(&obj) {
                let fk = rel.foreign_key(obj.clone(), rel.to.object(&obj.connection, suffix));
                if !details.foreign_keys.iter().any(|d| same_link(d, &fk)) {
                    details.foreign_keys.push(fk);
                }
            }
            if let Some(suffix) = rel.to.is(&obj) {
                let fk = rel.foreign_key(rel.from.object(&obj.connection, suffix), obj.clone());
                if !details.referenced_by.iter().any(|d| same_link(d, &fk)) {
                    details.referenced_by.push(fk);
                }
            }
        }
        for col in &mut details.columns {
            let targets: Vec<String> = details
                .foreign_keys
                .iter()
                .filter(|fk| fk.is_virtual() && fk.from_columns == [col.name.as_str()])
                .map(|fk| {
                    let table = if fk.to.database == obj.database {
                        fk.to.qualified()
                    } else {
                        format!("{}:{}", fk.to.database, fk.to.qualified())
                    };
                    format!("~{table}({})", fk.to_columns.join(", "))
                })
                .collect();
            if !targets.is_empty() {
                let joined = targets.join(" | ");
                col.references = Some(match col.references.take() {
                    Some(declared) => format!("{declared} | {joined}"),
                    None => joined,
                });
            }
        }
    }
}

/// Same columns between the same tables: a virtual relation that a declared
/// foreign key already covers (or a duplicate line in the file) adds nothing.
/// Polymorphic relations differ by their condition and are all kept.
fn same_link(a: &ForeignKey, b: &ForeignKey) -> bool {
    let when = |fk: &ForeignKey| match &fk.origin {
        FkOrigin::Virtual { when, .. } => when.clone(),
        FkOrigin::Declared => None,
    };
    a.from == b.from
        && a.to == b.to
        && a.from_columns == b.from_columns
        && a.to_columns == b.to_columns
        && (!a.is_virtual() || when(a) == when(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::Column;

    const TOML: &str = r#"
[[relation]]
from = { database = "api_core", table = "public.factures", columns = ["client_id"] }
to = { database = "api_core", table = "public.entreprises", columns = ["id"] }
kind = "polymorphe"
when = "client_type = 'entreprise'"

[[relation]]
from = { database = "api_core", table = "factures", columns = ["client_id"] }
to = { database = "api_core", table = "public.candidats", columns = ["id"] }
kind = "polymorphe"
when = "client_type = 'candidat'"

[[relation]]
from = { database = "api_core", table = "public.factures", columns = ["verifiee_par"] }
to = { database = "auth", table = "public.users", columns = ["id"] }
kind = "cross_base"
note = "compte du personnel"
"#;

    fn table(database: &str, name: &str) -> ObjectRef {
        ObjectRef {
            connection: "local".into(),
            database: database.into(),
            schema: "public".into(),
            name: name.into(),
            kind: ObjectKind::Table,
            signature: None,
        }
    }

    fn details(obj: ObjectRef, columns: &[&str]) -> ObjectDetails {
        ObjectDetails {
            object: obj,
            owner: None,
            comment: None,
            columns: columns
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    data_type: "uuid".into(),
                    nullable: true,
                    default: None,
                    identity: None,
                    generated: None,
                    collation: None,
                    comment: None,
                    primary_key: *c == "id",
                    references: None,
                })
                .collect(),
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            foreign_keys: vec![],
            referenced_by: vec![],
            stats: None,
            properties: vec![],
            ddl: String::new(),
        }
    }

    #[test]
    fn parses_and_defaults() {
        let r = Relations::parse(TOML).unwrap();
        assert_eq!(r.list.len(), 3);
        assert_eq!(r.list[1].from.schema_and_name(), ("public", "factures"));
        assert_eq!(r.list[2].when, None);
        let bare = Relations::parse(
            "[[relation]]\nfrom = { database = \"a\", table = \"t\", columns = [\"x\"] }\n\
             to = { database = \"a\", table = \"u\", columns = [\"id\"] }\n",
        )
        .unwrap();
        assert_eq!(bare.list[0].kind, "virtuelle");
        assert!(Relations::parse("[[relation]]\nfoo = 1\n").is_err());
    }

    #[test]
    fn missing_file_is_silent_and_invalid_file_warns() {
        let dir = std::env::temp_dir().join(format!("pgtui-relations-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("relations.toml");
        let (r, w) = Relations::load(&path);
        assert!(r.is_empty() && w.is_none());
        std::fs::write(&path, "pas du toml [").unwrap();
        let (r, w) = Relations::load(&path);
        assert!(r.is_empty() && w.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn outgoing_relations_and_column_references() {
        let r = Relations::parse(TOML).unwrap();
        let mut d = details(
            table("api_core", "factures"),
            &["id", "client_id", "verifiee_par"],
        );
        r.apply(&mut d);
        assert_eq!(d.foreign_keys.len(), 3);
        assert!(d.foreign_keys.iter().all(ForeignKey::is_virtual));
        let cross = &d.foreign_keys[2];
        assert_eq!(cross.to, table("auth", "users"));
        assert_eq!(
            d.columns[1].references.as_deref(),
            Some("~public.entreprises(id) | ~public.candidats(id)")
        );
        assert_eq!(
            d.columns[2].references.as_deref(),
            Some("~auth:public.users(id)")
        );
        assert_eq!(d.columns[0].references, None);
    }

    #[test]
    fn incoming_relations_keep_the_database_suffix() {
        let r = Relations::parse(TOML).unwrap();
        let mut d = details(table("auth_dev", "users"), &["id"]);
        r.apply(&mut d);
        assert_eq!(d.referenced_by.len(), 1);
        assert_eq!(d.referenced_by[0].from, table("api_core_dev", "factures"));
        assert!(d.foreign_keys.is_empty());
        // `auth` does not match `authority`.
        let mut other = details(table("authority", "users"), &["id"]);
        r.apply(&mut other);
        assert!(other.referenced_by.is_empty());
    }

    #[test]
    fn declared_foreign_key_wins_and_apply_is_idempotent() {
        let r = Relations::parse(TOML).unwrap();
        let mut d = details(table("api_core", "factures"), &["id", "client_id"]);
        let mut declared = r.list[2].foreign_key(d.object.clone(), table("auth", "users"));
        declared.origin = FkOrigin::Declared;
        d.foreign_keys.push(declared);
        r.apply(&mut d);
        assert_eq!(d.foreign_keys.len(), 3);
        assert!(!d.foreign_keys[0].is_virtual());
        let before = d.foreign_keys.len();
        r.apply(&mut d);
        assert_eq!(d.foreign_keys.len(), before);
    }
}
