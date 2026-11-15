//! Data and navigation state for the reusable table widget
//! (`ui::table_view`). The widget only reads a [`GridSource`]; the Schema
//! tab uses the simple in-memory [`GridData`], the Data tab will plug its own
//! source (large, paginated) in milestone 3.

use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

/// Semantic style of a cell; the theme turns it into colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellStyle {
    #[default]
    Normal,
    Dim,
    /// SQL NULL: rendered distinctly from an empty string.
    Null,
    /// Primary key / identifying value.
    Key,
    /// Navigable reference (Enter follows it).
    Link,
    Accent,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    pub text: String,
    pub style: CellStyle,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: CellStyle::Normal,
        }
    }

    pub fn styled(text: impl Into<String>, style: CellStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// What the table widget needs to draw rows.
pub trait GridSource {
    fn headers(&self) -> &[String];
    /// Display width of each column.
    fn widths(&self) -> &[u16];
    fn row_count(&self) -> usize;
    fn cell(&self, row: usize, col: usize) -> (Cow<'_, str>, CellStyle);
    /// Number of leading columns that stay visible when scrolling right.
    fn frozen(&self) -> usize {
        0
    }
}

pub const MAX_COLUMN_WIDTH: u16 = 60;

/// Small, fully in-memory grid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub widths: Vec<u16>,
    pub frozen: usize,
}

impl GridData {
    pub fn new(headers: Vec<String>, rows: Vec<Vec<Cell>>, frozen: usize) -> Self {
        let widths = headers
            .iter()
            .enumerate()
            .map(|(c, h)| {
                let content = rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|cell| display_width(&cell.text))
                    .max()
                    .unwrap_or(0);
                content.max(display_width(h)).clamp(1, MAX_COLUMN_WIDTH)
            })
            .collect();
        Self {
            headers,
            rows,
            widths,
            frozen,
        }
    }

    /// Tab-separated text of a row (for copying).
    pub fn row_tsv(&self, row: usize) -> Option<String> {
        self.rows.get(row).map(|r| {
            r.iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\t")
        })
    }
}

impl GridSource for GridData {
    fn headers(&self) -> &[String] {
        &self.headers
    }

    fn widths(&self) -> &[u16] {
        &self.widths
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, col: usize) -> (Cow<'_, str>, CellStyle) {
        self.rows
            .get(row)
            .and_then(|r| r.get(col))
            .map_or((Cow::Borrowed(""), CellStyle::Normal), |c| {
                (Cow::Borrowed(c.text.as_str()), c.style)
            })
    }

    fn frozen(&self) -> usize {
        self.frozen
    }
}

/// Width of the first line of `s` (cells show one line).
pub fn display_width(s: &str) -> u16 {
    let first = s.lines().next().unwrap_or("");
    u16::try_from(first.width()).unwrap_or(u16::MAX)
}

/// Cursor and scroll offsets. Row-oriented for now (the Data tab adds a
/// cell cursor in milestone 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridState {
    pub row: usize,
    pub row_offset: usize,
    /// First scrollable (non-frozen) column shown.
    pub col_offset: usize,
    /// Visible data rows, updated by the renderer.
    pub viewport_rows: usize,
}

impl GridState {
    pub fn move_by(&mut self, delta: isize, rows: usize) {
        if rows == 0 {
            self.row = 0;
            return;
        }
        self.row = self.row.saturating_add_signed(delta).min(rows - 1);
        self.scroll_into_view(rows);
    }

    pub fn top(&mut self) {
        self.row = 0;
        self.row_offset = 0;
    }

    pub fn bottom(&mut self, rows: usize) {
        self.row = rows.saturating_sub(1);
        self.scroll_into_view(rows);
    }

    pub fn half_page(&self) -> isize {
        (self.viewport_rows / 2).max(1) as isize
    }

    pub fn page(&self) -> isize {
        self.viewport_rows.saturating_sub(1).max(1) as isize
    }

    /// Horizontal scroll by whole columns.
    pub fn scroll_cols(&mut self, delta: isize, cols: usize, frozen: usize) {
        let scrollable = cols.saturating_sub(frozen);
        self.col_offset = self
            .col_offset
            .saturating_add_signed(delta)
            .min(scrollable.saturating_sub(1));
    }

    pub fn scroll_into_view(&mut self, rows: usize) {
        let h = self.viewport_rows.max(1);
        self.row = self.row.min(rows.saturating_sub(1));
        if self.row < self.row_offset {
            self.row_offset = self.row;
        } else if self.row >= self.row_offset + h {
            self.row_offset = self.row + 1 - h;
        }
        self.row_offset = self.row_offset.min(rows.saturating_sub(h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_from_content_and_headers() {
        let g = GridData::new(
            vec!["id".into(), "nom".into()],
            vec![
                vec![Cell::new("1"), Cell::new("山田 太郎")],
                vec![Cell::new("22"), Cell::new("a\nvery long second line")],
            ],
            1,
        );
        assert_eq!(g.widths, vec![2, 9]);
        assert_eq!(g.row_tsv(0).as_deref(), Some("1\t山田 太郎"));
    }

    #[test]
    fn width_is_capped() {
        let g = GridData::new(vec!["x".into()], vec![vec![Cell::new("y".repeat(500))]], 0);
        assert_eq!(g.widths, vec![MAX_COLUMN_WIDTH]);
    }

    #[test]
    fn navigation() {
        let mut s = GridState {
            viewport_rows: 3,
            ..Default::default()
        };
        s.move_by(5, 10);
        assert_eq!((s.row, s.row_offset), (5, 3));
        s.bottom(10);
        assert_eq!((s.row, s.row_offset), (9, 7));
        s.move_by(-100, 10);
        assert_eq!((s.row, s.row_offset), (0, 0));
        s.move_by(1, 0);
        assert_eq!(s.row, 0);
    }

    #[test]
    fn horizontal_scroll_respects_frozen() {
        let mut s = GridState::default();
        s.scroll_cols(10, 5, 1);
        assert_eq!(s.col_offset, 3);
        s.scroll_cols(-10, 5, 1);
        assert_eq!(s.col_offset, 0);
    }
}
