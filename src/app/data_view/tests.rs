use super::*;
use crate::db::data::RowCount;
use crate::db::schema::{Column, Constraint};
use pretty_assertions::assert_eq;

fn col(name: &str, ty: &str, pk: bool, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        default: None,
        identity: None,
        generated: None,
        collation: None,
        comment: None,
        primary_key: pk,
        references: None,
    }
}

fn details(kind: ObjectKind, columns: Vec<Column>) -> ObjectDetails {
    ObjectDetails {
        object: ObjectRef {
            connection: "test".into(),
            database: "demo".into(),
            schema: "shop".into(),
            name: "t".into(),
            kind,
            signature: None,
        },
        owner: None,
        comment: None,
        columns,
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

fn view() -> DataView {
    let d = details(
        ObjectKind::Table,
        vec![
            col("name", "text", false, true),
            col("id", "bigint", true, false),
            col("total", "numeric(12,2)", false, true),
        ],
    );
    let mut v = DataView::new(&d, 100, false);
    v.grid.viewport_rows = 20;
    v
}

fn rows(range: Range<usize>) -> Vec<Vec<Option<String>>> {
    range
        .map(|i| {
            vec![
                Some(format!("name {i}")),
                Some(i.to_string()),
                (i % 2 == 0).then(|| format!("{i}.50")),
            ]
        })
        .collect()
}

/// Simulates the reducer: plan, mark pending, apply the rows from `table`.
fn load(v: &mut DataView, id: u64, table_len: usize) -> Option<(usize, usize, LoadMode)> {
    let plan = v.plan_fetch()?;
    let (offset, limit, mode) = plan;
    v.loading = Some(Pending {
        id,
        offset,
        limit,
        mode,
    });
    let end = (offset + limit).min(table_len);
    assert!(v.apply_rows(id, offset, rows(offset..end.max(offset))));
    Some(plan)
}

#[test]
fn key_columns_first_and_frozen() {
    let v = view();
    let names: Vec<_> = v
        .order
        .iter()
        .map(|i| v.columns[*i].name.as_str())
        .collect();
    assert_eq!(names, ["id", "name", "total"]);
    assert_eq!(GridSource::frozen(&v), 1);
    assert_eq!(v.key_columns, ["id"]);
    assert!(v.read_only_reason.is_none());
    assert!(GridSource::right_aligned(&v, 2), "numeric is right-aligned");
    let req = v.request(0, 10);
    assert_eq!(req.tiebreak, ["id"]);
    assert!(req.with_row_version && !req.order_by_ctid);
}

#[test]
fn editability_reasons() {
    let no_key = details(ObjectKind::Table, vec![col("a", "text", false, true)]);
    let v = DataView::new(&no_key, 100, false);
    assert!(
        v.read_only_reason
            .as_deref()
            .unwrap()
            .contains("clé primaire")
    );
    assert!(v.request(0, 1).order_by_ctid);

    let mut unique = details(ObjectKind::Table, vec![col("code", "text", false, false)]);
    unique.constraints.push(Constraint {
        name: "u".into(),
        kind: ConstraintKind::Unique,
        definition: "UNIQUE (code)".into(),
        columns: vec!["code".into()],
        deferrable: false,
        initially_deferred: false,
    });
    assert_eq!(DataView::new(&unique, 100, false).key_columns, ["code"]);
    assert!(
        DataView::new(&unique, 100, true)
            .read_only_reason
            .unwrap()
            .contains("connexion")
    );

    let view = details(ObjectKind::View, vec![col("a", "text", false, true)]);
    let v = DataView::new(&view, 100, false);
    assert!(v.read_only_reason.is_some());
    let req = v.request(0, 1);
    assert!(!req.with_row_version && !req.order_by_ctid);
}

#[test]
fn lazy_loading_then_end_of_result() {
    let mut v = view();
    assert_eq!(load(&mut v, 1, 250), Some((0, 100, LoadMode::Replace)));
    assert_eq!(
        v.row_count(),
        101,
        "one placeholder row while more may exist"
    );
    assert_eq!(v.plan_fetch(), None, "visible rows are loaded");

    // Approaching the end of the window loads the next page.
    v.grid.row = 85;
    v.grid.scroll_into_view(v.row_count());
    assert_eq!(load(&mut v, 2, 250), Some((100, 100, LoadMode::Append)));
    v.grid.row = 190;
    v.grid.scroll_into_view(v.row_count());
    assert_eq!(load(&mut v, 3, 250), Some((200, 100, LoadMode::Append)));
    assert!(v.exhausted);
    assert_eq!(v.total, Total::Exact(250));
    assert_eq!(v.row_count(), 250);
    v.grid.bottom(v.row_count());
    assert_eq!(v.plan_fetch(), None);
}

#[test]
fn jump_replaces_window_and_scrolling_back_prepends() {
    let mut v = view();
    load(&mut v, 1, 100_000);
    v.total = Total::Exact(100_000);
    v.grid.bottom(100_000);
    let (offset, _, mode) = load(&mut v, 2, 100_000).unwrap();
    assert_eq!(mode, LoadMode::Replace);
    assert!(offset <= 100_000 - 20 && v.end() == 100_000);
    v.grid.row = v.start - 5;
    v.grid.scroll_into_view(100_000);
    let (offset, limit, mode) = load(&mut v, 3, 100_000).unwrap();
    assert_eq!(mode, LoadMode::Prepend);
    assert_eq!(offset + limit, v.start + limit, "contiguous");
    assert_eq!(
        v.value(v.grid.row, 0),
        Some(Some(v.grid.row.to_string().as_str()))
    );
}

#[test]
fn window_is_trimmed_around_cursor() {
    let mut v = view();
    for i in 0..(MAX_WINDOW_PAGES as u64 + 3) {
        v.grid.row = v.end().saturating_sub(1);
        v.grid.scroll_into_view(v.row_count());
        load(&mut v, i, 1_000_000);
    }
    assert_eq!(v.rows.len(), 100 * MAX_WINDOW_PAGES);
    assert!(v.start > 0);
    assert!(v.row(v.grid.row).is_some(), "cursor row stays loaded");
}

#[test]
fn stale_responses_are_ignored() {
    let mut v = view();
    v.loading = Some(Pending {
        id: 7,
        offset: 0,
        limit: 100,
        mode: LoadMode::Replace,
    });
    assert!(!v.apply_rows(6, 0, rows(0..100)));
    assert!(v.rows.is_empty());
    v.counting = Some(3);
    v.apply_count(2, RowCount::Exact(5));
    assert_eq!(v.total, Total::Unknown);
    v.apply_count(3, RowCount::AtLeast(1_000_000));
    assert_eq!(v.total, Total::AtLeast(1_000_000));
}

#[test]
fn empty_result() {
    let mut v = view();
    load(&mut v, 1, 0);
    assert_eq!(v.total, Total::Exact(0));
    assert_eq!(v.row_count(), 0);
    assert_eq!(v.plan_fetch(), None);
}

#[test]
fn null_and_cells() {
    let mut v = view();
    load(&mut v, 1, 10);
    assert_eq!(v.cell(1, 2), (Cow::Borrowed("NULL"), CellStyle::Null));
    assert_eq!(v.cell(2, 2), (Cow::Borrowed("2.50"), CellStyle::Normal));
    assert_eq!(v.cell(2, 0).1, CellStyle::Key);
    assert_eq!(v.cell(500, 0), (Cow::Borrowed("…"), CellStyle::Dim));
}

#[test]
fn sort_cycle_and_headers() {
    let mut v = view();
    load(&mut v, 1, 10);
    v.grid.col = 2;
    v.cycle_sort();
    assert_eq!(v.headers[2], "total ↑");
    assert!(v.rows.is_empty(), "sort change reloads");
    v.grid.col = 1;
    v.cycle_sort();
    assert_eq!(v.headers[2], "total ↑1");
    assert_eq!(v.headers[1], "name ↑2");
    v.cycle_sort();
    assert_eq!(v.headers[1], "name ↓2");
    v.cycle_sort();
    assert_eq!(v.headers[1], "name");
    assert_eq!(v.headers[2], "total ↑");
    assert_eq!(v.describe_query(), ["tri : total asc"]);
}

#[test]
fn hide_resize_show() {
    let mut v = view();
    v.grid.col = 0;
    assert!(v.hide_current());
    assert_eq!(
        GridSource::frozen(&v),
        0,
        "hidden key column no longer frozen"
    );
    assert_eq!(v.order.len(), 2);
    let w = v.widths[0];
    v.resize_current(4);
    assert_eq!(v.widths[0], w + 4);
    v.auto_width_current();
    assert_eq!(v.widths[0], w);
    assert!(v.hide_current());
    assert!(!v.hide_current(), "the last column stays");
    assert_eq!(v.show_all_columns(), 2);
    assert_eq!(v.order.len(), 3);
}

#[test]
fn search_smart_case_and_wrap() {
    let mut v = view();
    load(&mut v, 1, 10);
    v.search = Some("NAME 3".into());
    assert_eq!(
        v.find_match(true),
        None,
        "uppercase query is case-sensitive"
    );
    v.search = Some("name 3".into());
    assert_eq!(v.find_match(true), Some((3, 1)));
    v.grid.row = 3;
    v.grid.col = 1;
    assert_eq!(
        v.find_match(true),
        Some((3, 1)),
        "wraps around to the only match"
    );
    v.search = Some(".50".into());
    assert_eq!(v.match_count(), 5);
    assert_eq!(v.find_match(false), Some((2, 2)));
    assert!(v.is_match(4, 2) && !v.is_match(3, 2));
}

#[test]
fn selection_and_extract() {
    let mut v = view();
    load(&mut v, 1, 10);
    v.grid.row = 2;
    v.grid.col = 1;
    assert_eq!(v.selection(false), (2..3, vec![1]));
    assert_eq!(v.selection(true), (2..3, vec![0, 1, 2]));
    v.visual = Some(Visual {
        row: 4,
        col: 2,
        line: false,
    });
    assert_eq!(v.selection(false), (2..5, vec![1, 2]));
    assert!(v.is_selected(3, 2) && !v.is_selected(3, 0));
    v.visual = Some(Visual {
        row: 4,
        col: 2,
        line: true,
    });
    let (rows, cols) = v.selection(false);
    let (e, missing) = v.extract(rows, &cols);
    assert_eq!(missing, 0);
    assert_eq!(
        e.columns.iter().map(|c| c.0).collect::<Vec<_>>(),
        ["id", "name", "total"]
    );
    assert_eq!(e.rows[0], [Some("2"), Some("name 2"), Some("2.50")]);
    let (_, missing) = v.extract(8..15, &[0]);
    assert_eq!(missing, 5);
}
