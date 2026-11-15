//! Schema tab state: sections built from [`ObjectDetails`], per-section
//! cursors, and DDL scrolling.

use std::sync::Arc;

use super::format;
use super::grid::{Cell, CellStyle, GridData, GridState};
use crate::db::catalog::{ObjectKind, ObjectRef};
use crate::db::schema::{ConstraintKind, Generated, Identity, ObjectDetails};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Columns,
    Indexes,
    Constraints,
    Triggers,
    Relations,
    Ddl,
    Info,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::Columns => "Colonnes",
            Section::Indexes => "Index",
            Section::Constraints => "Contraintes",
            Section::Triggers => "Triggers",
            Section::Relations => "Relations",
            Section::Ddl => "DDL",
            Section::Info => "Infos",
        }
    }

    /// Sections relevant for an object kind.
    pub fn for_kind(kind: ObjectKind) -> &'static [Section] {
        use Section::*;
        match kind {
            ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => &[
                Columns,
                Indexes,
                Constraints,
                Triggers,
                Relations,
                Ddl,
                Info,
            ],
            ObjectKind::MaterializedView => &[Columns, Indexes, Ddl, Info],
            ObjectKind::View => &[Columns, Triggers, Ddl, Info],
            ObjectKind::Sequence => &[Info, Ddl],
            ObjectKind::Function | ObjectKind::Procedure => &[Ddl, Info],
        }
    }
}

/// A section rendered as a table, with an optional navigation target per row.
#[derive(Debug, Clone, Default)]
pub struct SectionTable {
    pub grid: GridData,
    pub links: Vec<Option<ObjectRef>>,
    pub state: GridState,
}

impl SectionTable {
    fn new(headers: &[&str], rows: Vec<(Vec<Cell>, Option<ObjectRef>)>) -> Self {
        let (cells, links): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        Self {
            grid: GridData::new(headers.iter().map(|h| h.to_string()).collect(), cells, 1),
            links,
            state: GridState::default(),
        }
    }

    pub fn selected_link(&self) -> Option<&ObjectRef> {
        self.links.get(self.state.row)?.as_ref()
    }
}

/// Scroll state of the DDL text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextState {
    pub scroll: usize,
    pub hscroll: usize,
    pub viewport: usize,
}

impl TextState {
    pub fn scroll_by(&mut self, delta: isize, lines: usize) {
        let max = lines.saturating_sub(self.viewport.max(1));
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }
}

#[derive(Debug, Clone, Default)]
pub struct SchemaView {
    pub object: Option<ObjectRef>,
    pub details: Option<Arc<ObjectDetails>>,
    pub sections: Vec<Section>,
    pub section: usize,
    pub columns: SectionTable,
    pub indexes: SectionTable,
    pub constraints: SectionTable,
    pub triggers: SectionTable,
    pub relations: SectionTable,
    pub info: SectionTable,
    pub ddl: TextState,
    pub ddl_lines: usize,
}

impl SchemaView {
    /// Shows `details`. The current section is kept when the new object has it.
    pub fn show(&mut self, details: Arc<ObjectDetails>) {
        let previous = self.current_section();
        let same_object = self.object.as_ref() == Some(&details.object);
        self.sections = Section::for_kind(details.object.kind).to_vec();
        self.section = previous
            .and_then(|s| self.sections.iter().position(|x| *x == s))
            .unwrap_or(0);

        let keep = |old: &SectionTable, mut new: SectionTable| {
            if same_object {
                new.state = old.state;
                new.state.scroll_into_view(new.grid.rows.len());
            }
            new
        };
        self.columns = keep(&self.columns, columns_table(&details));
        self.indexes = keep(&self.indexes, indexes_table(&details));
        self.constraints = keep(&self.constraints, constraints_table(&details));
        self.triggers = keep(&self.triggers, triggers_table(&details));
        self.relations = keep(&self.relations, relations_table(&details));
        self.info = keep(&self.info, info_table(&details));
        if !same_object {
            self.ddl = TextState::default();
        }
        self.ddl_lines = details.ddl.lines().count();
        self.object = Some(details.object.clone());
        self.details = Some(details);
    }

    pub fn clear(&mut self) {
        *self = Self {
            section: self.section,
            sections: self.sections.clone(),
            ..Self::default()
        };
    }

    pub fn current_section(&self) -> Option<Section> {
        self.sections.get(self.section).copied()
    }

    pub fn cycle_section(&mut self, delta: isize) {
        let n = self.sections.len() as isize;
        if n > 0 {
            self.section = (self.section as isize + delta).rem_euclid(n) as usize;
        }
    }

    pub fn select_section(&mut self, s: Section) {
        if let Some(i) = self.sections.iter().position(|x| *x == s) {
            self.section = i;
        }
    }

    /// The table of the current section (`None` for DDL).
    pub fn table_mut(&mut self) -> Option<&mut SectionTable> {
        Some(match self.current_section()? {
            Section::Columns => &mut self.columns,
            Section::Indexes => &mut self.indexes,
            Section::Constraints => &mut self.constraints,
            Section::Triggers => &mut self.triggers,
            Section::Relations => &mut self.relations,
            Section::Info => &mut self.info,
            Section::Ddl => return None,
        })
    }

    pub fn table(&self) -> Option<&SectionTable> {
        Some(match self.current_section()? {
            Section::Columns => &self.columns,
            Section::Indexes => &self.indexes,
            Section::Constraints => &self.constraints,
            Section::Triggers => &self.triggers,
            Section::Relations => &self.relations,
            Section::Info => &self.info,
            Section::Ddl => return None,
        })
    }

    /// Row count shown next to a section title.
    pub fn count(&self, s: Section) -> Option<usize> {
        let t = match s {
            Section::Columns => &self.columns,
            Section::Indexes => &self.indexes,
            Section::Constraints => &self.constraints,
            Section::Triggers => &self.triggers,
            Section::Relations => &self.relations,
            Section::Ddl | Section::Info => return None,
        };
        Some(t.grid.rows.len())
    }
}

fn dim(s: impl Into<String>) -> Cell {
    Cell::styled(s, CellStyle::Dim)
}

fn opt(s: Option<&str>) -> Cell {
    match s {
        Some(v) => Cell::new(v),
        None => dim(""),
    }
}

fn columns_table(d: &ObjectDetails) -> SectionTable {
    let rows = d
        .columns
        .iter()
        .map(|c| {
            let fk = d
                .foreign_keys
                .iter()
                .find(|fk| fk.from_columns.len() == 1 && fk.from_columns[0] == c.name);
            let mut keys = Vec::new();
            if c.primary_key {
                keys.push("PK".to_string());
            }
            if let Some(r) = &c.references {
                keys.push(format!("FK → {r}"));
            }
            let key_style = if fk.is_some() {
                CellStyle::Link
            } else if c.primary_key {
                CellStyle::Key
            } else {
                CellStyle::Normal
            };
            let default = match (c.identity, c.generated, &c.default) {
                (Some(Identity::Always), _, _) => {
                    Cell::styled("GENERATED ALWAYS AS IDENTITY", CellStyle::Accent)
                }
                (Some(Identity::ByDefault), _, _) => {
                    Cell::styled("GENERATED BY DEFAULT AS IDENTITY", CellStyle::Accent)
                }
                (_, Some(g), Some(e)) => Cell::styled(
                    format!(
                        "GENERATED ({e}) {}",
                        if g == Generated::Stored {
                            "STORED"
                        } else {
                            "VIRTUAL"
                        }
                    ),
                    CellStyle::Accent,
                ),
                (_, _, Some(e)) => Cell::new(e.clone()),
                _ => dim(""),
            };
            let name_style = if c.primary_key {
                CellStyle::Key
            } else {
                CellStyle::Normal
            };
            (
                vec![
                    Cell::styled(c.name.clone(), name_style),
                    Cell::new(c.data_type.clone()),
                    if c.nullable {
                        dim("NULL")
                    } else {
                        Cell::new("NOT NULL")
                    },
                    default,
                    Cell::styled(keys.join(", "), key_style),
                    opt(c.comment.as_deref()),
                ],
                fk.map(|fk| fk.to.clone()),
            )
        })
        .collect();
    SectionTable::new(
        &["Colonne", "Type", "Null", "Défaut", "Clé", "Commentaire"],
        rows,
    )
}

fn indexes_table(d: &ObjectDetails) -> SectionTable {
    let rows = d
        .indexes
        .iter()
        .map(|i| {
            let mut cols = i.columns.join(", ");
            if !i.include.is_empty() {
                cols.push_str(&format!(" INCLUDE ({})", i.include.join(", ")));
            }
            let kind = if i.primary {
                Cell::styled("PRIMARY", CellStyle::Key)
            } else if i.unique {
                Cell::styled("UNIQUE", CellStyle::Accent)
            } else {
                dim("")
            };
            (
                vec![
                    Cell::new(i.name.clone()),
                    Cell::new(i.method.clone()),
                    Cell::new(cols),
                    kind,
                    i.size_bytes
                        .map_or_else(|| dim(""), |b| Cell::new(format::bytes(b))),
                    opt(i.predicate.as_deref()),
                ],
                None,
            )
        })
        .collect();
    SectionTable::new(
        &[
            "Index",
            "Méthode",
            "Colonnes",
            "Unicité",
            "Taille",
            "Condition",
        ],
        rows,
    )
}

fn constraints_table(d: &ObjectDetails) -> SectionTable {
    let rows = d
        .constraints
        .iter()
        .map(|c| {
            let style = match c.kind {
                ConstraintKind::PrimaryKey => CellStyle::Key,
                ConstraintKind::ForeignKey => CellStyle::Link,
                _ => CellStyle::Accent,
            };
            let deferrable = match (c.deferrable, c.initially_deferred) {
                (false, _) => dim("non"),
                (true, false) => Cell::new("oui (IMMEDIATE)"),
                (true, true) => Cell::new("oui (DEFERRED)"),
            };
            (
                vec![
                    Cell::new(c.name.clone()),
                    Cell::styled(c.kind.label(), style),
                    Cell::new(c.definition.clone()),
                    deferrable,
                ],
                None,
            )
        })
        .collect();
    SectionTable::new(&["Contrainte", "Type", "Définition", "Différable"], rows)
}

fn triggers_table(d: &ObjectDetails) -> SectionTable {
    let rows = d
        .triggers
        .iter()
        .map(|t| {
            (
                vec![
                    Cell::new(t.name.clone()),
                    Cell::new(t.timing.clone()),
                    Cell::new(t.events.clone()),
                    Cell::new(t.level.clone()),
                    Cell::new(t.function.clone()),
                    if t.enabled {
                        Cell::new("oui")
                    } else {
                        Cell::styled("non", CellStyle::Warning)
                    },
                ],
                None,
            )
        })
        .collect();
    SectionTable::new(
        &[
            "Trigger",
            "Moment",
            "Événements",
            "Niveau",
            "Fonction",
            "Actif",
        ],
        rows,
    )
}

fn relations_table(d: &ObjectDetails) -> SectionTable {
    let outgoing = d.foreign_keys.iter().map(|fk| (true, fk));
    let incoming = d.referenced_by.iter().map(|fk| (false, fk));
    let rows = outgoing
        .chain(incoming)
        .map(|(out, fk)| {
            let (dir, other, other_cols, own_cols) = if out {
                ("→ référence", &fk.to, &fk.to_columns, &fk.from_columns)
            } else {
                (
                    "← référencée par",
                    &fk.from,
                    &fk.from_columns,
                    &fk.to_columns,
                )
            };
            (
                vec![
                    Cell::styled(dir, CellStyle::Accent),
                    Cell::styled(other.qualified(), CellStyle::Link),
                    Cell::new(own_cols.join(", ")),
                    Cell::new(other_cols.join(", ")),
                    Cell::new(fk.name.clone()),
                    Cell::new(fk.on_update.label()),
                    Cell::new(fk.on_delete.label()),
                ],
                Some(other.clone()),
            )
        })
        .collect();
    SectionTable::new(
        &[
            "Sens",
            "Table liée",
            "Colonnes",
            "Colonnes liées",
            "Contrainte",
            "ON UPDATE",
            "ON DELETE",
        ],
        rows,
    )
}

fn info_table(d: &ObjectDetails) -> SectionTable {
    let mut rows: Vec<(String, Cell)> = vec![
        (
            "Objet".into(),
            Cell::styled(d.object.qualified(), CellStyle::Key),
        ),
        ("Type".into(), Cell::new(d.object.kind.label())),
    ];
    if let Some(o) = &d.owner {
        rows.push(("Propriétaire".into(), Cell::new(o.clone())));
    }
    rows.push(("Commentaire".into(), opt(d.comment.as_deref())));
    if let Some(s) = &d.stats {
        rows.push((
            "Lignes (estimation)".into(),
            s.estimated_rows.map_or_else(
                || Cell::styled("inconnue (table jamais analysée)", CellStyle::Warning),
                |n| Cell::new(format::count(n)),
            ),
        ));
        if let (Some(live), Some(dead)) = (s.live_tuples, s.dead_tuples) {
            rows.push((
                "Lignes vivantes / mortes".into(),
                Cell::new(format!("{} / {}", format::count(live), format::count(dead))),
            ));
        }
        rows.push((
            "Taille totale".into(),
            Cell::new(format::bytes(s.total_bytes)),
        ));
        rows.push(("  Données".into(), Cell::new(format::bytes(s.table_bytes))));
        rows.push(("  Index".into(), Cell::new(format::bytes(s.index_bytes))));
        rows.push(("  TOAST".into(), Cell::new(format::bytes(s.toast_bytes))));
        if let Some(n) = s.seq_scans {
            rows.push(("Parcours séquentiels".into(), Cell::new(format::count(n))));
        }
        if let Some(n) = s.index_scans {
            rows.push(("Parcours d'index".into(), Cell::new(format::count(n))));
        }
        let ts = |t: &Option<chrono::DateTime<chrono::Utc>>| {
            t.as_ref()
                .map_or_else(|| dim("jamais"), |t| Cell::new(format::timestamp(t)))
        };
        if s.live_tuples.is_some() {
            rows.push(("Dernier VACUUM".into(), ts(&s.last_vacuum)));
            rows.push(("Dernier autovacuum".into(), ts(&s.last_autovacuum)));
            rows.push(("Dernier ANALYZE".into(), ts(&s.last_analyze)));
            rows.push(("Dernier autoanalyze".into(), ts(&s.last_autoanalyze)));
        }
    }
    rows.extend(
        d.properties
            .iter()
            .map(|(k, v)| (k.clone(), Cell::new(v.clone()))),
    );
    SectionTable::new(
        &["Propriété", "Valeur"],
        rows.into_iter()
            .map(|(k, v)| (vec![dim(k), v], None))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::{Column, FkAction, ForeignKey, Stats};

    fn obj(name: &str) -> ObjectRef {
        ObjectRef {
            schema: "shop".into(),
            name: name.into(),
            kind: ObjectKind::Table,
            signature: None,
        }
    }

    fn details() -> ObjectDetails {
        let fk = ForeignKey {
            name: "orders_customer_id_fkey".into(),
            from: obj("orders"),
            from_columns: vec!["customer_id".into()],
            to: obj("customers"),
            to_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::Cascade,
        };
        let incoming = ForeignKey {
            name: "items_order_fkey".into(),
            from: obj("order_items"),
            from_columns: vec!["order_id".into()],
            to: obj("orders"),
            to_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::Cascade,
        };
        let col = |name: &str, pk: bool, refs: Option<&str>| Column {
            name: name.into(),
            data_type: "bigint".into(),
            nullable: !pk,
            default: None,
            identity: pk.then_some(Identity::Always),
            generated: None,
            collation: None,
            comment: None,
            primary_key: pk,
            references: refs.map(str::to_string),
        };
        ObjectDetails {
            object: obj("orders"),
            owner: Some("pgtui".into()),
            comment: None,
            columns: vec![
                col("id", true, None),
                col("customer_id", false, Some("shop.customers(id)")),
            ],
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            foreign_keys: vec![fk],
            referenced_by: vec![incoming],
            stats: Some(Stats {
                estimated_rows: Some(100_000),
                total_bytes: 20 << 20,
                ..Stats::default()
            }),
            properties: vec![],
            ddl: "CREATE TABLE shop.orders (\n);\n".into(),
        }
    }

    #[test]
    fn builds_sections_and_links() {
        let mut v = SchemaView::default();
        v.show(Arc::new(details()));
        assert_eq!(v.current_section(), Some(Section::Columns));
        assert_eq!(v.columns.grid.rows.len(), 2);
        assert_eq!(v.columns.grid.rows[0][4].text, "PK");
        assert_eq!(v.columns.grid.rows[1][4].text, "FK → shop.customers(id)");
        assert_eq!(
            v.columns.links[1].as_ref().map(|o| o.name.as_str()),
            Some("customers")
        );
        assert_eq!(v.relations.grid.rows.len(), 2);
        assert_eq!(
            v.relations.links[1].as_ref().map(|o| o.name.as_str()),
            Some("order_items")
        );
        assert_eq!(v.ddl_lines, 2);
        let info: Vec<_> = v.info.grid.rows.iter().map(|r| r[1].text.clone()).collect();
        assert!(info.contains(&"100 000".to_string()), "{info:?}");
        assert!(info.contains(&"20 MB".to_string()), "{info:?}");
    }

    #[test]
    fn keeps_section_across_objects_when_possible() {
        let mut v = SchemaView::default();
        v.show(Arc::new(details()));
        v.select_section(Section::Ddl);
        let mut seq = details();
        seq.object.kind = ObjectKind::Sequence;
        seq.object.name = "seq".into();
        v.show(Arc::new(seq));
        assert_eq!(
            v.current_section(),
            Some(Section::Ddl),
            "sequences have a DDL section"
        );
        v.cycle_section(1);
        assert_eq!(v.current_section(), Some(Section::Info));
        v.cycle_section(1);
        assert_eq!(
            v.current_section(),
            Some(Section::Ddl),
            "wraps: [Info, Ddl]"
        );
    }

    #[test]
    fn reload_keeps_cursor() {
        let mut v = SchemaView::default();
        v.show(Arc::new(details()));
        v.columns.state.viewport_rows = 10;
        v.columns.state.move_by(1, 2);
        v.show(Arc::new(details()));
        assert_eq!(v.columns.state.row, 1);
    }
}
