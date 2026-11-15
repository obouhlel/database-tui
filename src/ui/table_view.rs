//! Reusable table widget: header, virtualised rows (only visible rows are
//! read from the source), frozen leading columns, horizontal scrolling by
//! column, Unicode-aware truncation.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::app::grid::{GridSource, GridState};
use crate::ui::theme::Theme;

/// Extra marking of a cell (search match, visual selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Match,
    Selected,
}

/// Rendering options.
pub struct Options<'a> {
    pub focused: bool,
    /// Highlight the cell at (`state.row`, `state.col`) and keep it visible.
    pub cell_cursor: bool,
    pub mark: Option<&'a dyn Fn(usize, usize) -> Mark>,
}

impl Options<'_> {
    pub fn rows(focused: bool) -> Self {
        Self {
            focused,
            cell_cursor: false,
            mark: None,
        }
    }
}

/// Draws `source` in `area` and updates the viewport fields of `state`.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    source: &dyn GridSource,
    state: &mut GridState,
    theme: &Theme,
    opts: &Options,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let rows = source.row_count();
    state.viewport_rows = area.height.saturating_sub(1) as usize;
    state.scroll_into_view(rows);

    let widths = source.widths();
    let ncols = widths.len();
    let frozen = source.frozen().min(ncols);
    state.col_offset = state
        .col_offset
        .min(ncols.saturating_sub(frozen).saturating_sub(1));
    if opts.cell_cursor && ncols > 0 {
        state.col = state.col.min(ncols - 1);
        keep_column_visible(state, widths, frozen, area.width);
    }

    // Columns that fit: frozen ones, then scrollable ones from the offset.
    let mut visible: Vec<(usize, u16)> = Vec::new();
    let mut x: u16 = 0;
    let order = (0..frozen).chain(frozen + state.col_offset..ncols);
    for c in order {
        if x >= area.width {
            break;
        }
        // One space of padding on each side, plus a separator.
        let w = (widths[c] + 2).min(area.width - x);
        visible.push((c, w));
        x += w + 1;
    }
    let more_left = state.col_offset > 0;
    let more_right = visible.last().is_some_and(|(c, _)| *c + 1 < ncols) || x > area.width + 1;

    let header_style = Style::new()
        .bg(theme.header_bg)
        .fg(theme.fg)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::new().fg(theme.border);

    // Header.
    let header_area = Rect { height: 1, ..area };
    frame.buffer_mut().set_style(header_area, header_style);
    let cursor_col = opts.cell_cursor.then_some(state.col);
    let header = build_line(source, None, &visible, theme, sep_style, |c, _| {
        if Some(c) == cursor_col {
            header_style.fg(theme.accent)
        } else {
            header_style
        }
    });
    frame.render_widget(header, header_area);
    if more_left {
        frame.render_widget(
            Span::styled("‹", header_style.fg(theme.accent)),
            Rect {
                width: 1,
                ..header_area
            },
        );
    }
    if more_right {
        let r = Rect {
            x: area.right() - 1,
            width: 1,
            ..header_area
        };
        frame.render_widget(Span::styled("›", header_style.fg(theme.accent)), r);
    }

    if rows == 0 {
        let r = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        frame.render_widget(Span::styled(" (aucune ligne)", theme.dim()), r);
        return;
    }
    let row_bg = if opts.focused {
        theme.selection_bg
    } else {
        theme.selection_inactive_bg
    };
    for (i, row) in (state.row_offset..rows)
        .take(state.viewport_rows)
        .enumerate()
    {
        let r = Rect {
            y: area.y + 1 + i as u16,
            height: 1,
            ..area
        };
        let current = row == state.row;
        if current {
            let bg = if opts.cell_cursor {
                theme.selection_inactive_bg
            } else {
                row_bg
            };
            frame.buffer_mut().set_style(r, Style::new().bg(bg));
        }
        let line = build_line(source, Some(row), &visible, theme, sep_style, |c, style| {
            let mut style = style;
            if current {
                style = style.bg(if opts.cell_cursor {
                    theme.selection_inactive_bg
                } else {
                    row_bg
                });
                if opts.cell_cursor {
                    // Grey row background: default text colour stays readable
                    // in any terminal palette.
                    style = style.fg(theme.fg);
                }
                if !opts.cell_cursor && opts.focused {
                    style = style.add_modifier(Modifier::BOLD);
                }
            }
            match opts.mark.map_or(Mark::None, |m| m(row, c)) {
                Mark::Selected => style = style.bg(theme.selection_bg).fg(theme.selection_fg),
                Mark::Match => style = style.bg(theme.matched).fg(theme.tab_active_fg),
                Mark::None => {}
            }
            if current && Some(c) == cursor_col {
                style = if opts.focused {
                    style
                        .bg(theme.accent)
                        .fg(theme.tab_active_fg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style.add_modifier(Modifier::UNDERLINED)
                };
            }
            style
        });
        frame.render_widget(line, r);
    }
}

/// Scrolls horizontally so that the cursor column is fully visible.
fn keep_column_visible(state: &mut GridState, widths: &[u16], frozen: usize, width: u16) {
    if state.col < frozen {
        return;
    }
    let target = state.col - frozen;
    if target < state.col_offset {
        state.col_offset = target;
        return;
    }
    let frozen_w: u32 = widths[..frozen].iter().map(|w| u32::from(*w) + 3).sum();
    let avail = u32::from(width).saturating_sub(frozen_w);
    // Advance the offset until columns offset..=col fit.
    loop {
        let span: u32 = widths[frozen + state.col_offset..=state.col]
            .iter()
            .map(|w| u32::from(*w) + 3)
            .sum();
        if span <= avail || state.col_offset >= target {
            break;
        }
        state.col_offset += 1;
    }
}

fn build_line<'a>(
    source: &'a dyn GridSource,
    row: Option<usize>,
    visible: &[(usize, u16)],
    theme: &Theme,
    sep: Style,
    style_of: impl Fn(usize, Style) -> Style,
) -> Line<'a> {
    let mut spans = Vec::with_capacity(visible.len() * 2);
    let frozen = source.frozen();
    for (i, (c, w)) in visible.iter().enumerate() {
        let (text, style, right) = match row {
            None => (
                source.headers()[*c].as_str().into(),
                style_of(*c, Style::new()),
                false,
            ),
            Some(r) => {
                let (t, s) = source.cell(r, *c);
                (t, style_of(*c, theme.cell(s)), source.right_aligned(*c))
            }
        };
        let inner = w.saturating_sub(2) as usize;
        let text = if right {
            fit_right(&text, inner)
        } else {
            fit(&text, inner)
        };
        spans.push(Span::styled(format!(" {text} "), style));
        if i + 1 < visible.len() {
            // A heavier separator after the frozen columns.
            let glyph = if *c + 1 == frozen && frozen > 0 {
                "┃"
            } else {
                "│"
            };
            spans.push(Span::styled(glyph, sep));
        }
    }
    Line::from(spans)
}

/// Like [`fit`], padded on the left (numbers).
pub fn fit_right(s: &str, width: usize) -> String {
    let left = fit(s, width);
    let trimmed = left.trim_end_matches(' ');
    let used: usize = trimmed.chars().map(|c| c.width().unwrap_or(0)).sum();
    format!("{}{trimmed}", " ".repeat(width.saturating_sub(used)))
}

/// First line of `s`, truncated with `…` and padded to exactly `width` columns.
pub fn fit(s: &str, width: usize) -> String {
    let first = s.lines().next().unwrap_or("");
    let multiline = s.contains('\n');
    let mut out = String::new();
    let mut used = 0;
    let mut truncated = false;
    for ch in first.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > width {
            truncated = true;
            break;
        }
        out.push(ch);
        used += cw;
    }
    if (truncated || multiline) && width > 0 {
        // Make room for the ellipsis.
        while used + 1 > width {
            match out.pop() {
                Some(ch) => used -= ch.width().unwrap_or(0),
                None => break,
            }
        }
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_alignment() {
        assert_eq!(fit_right("42", 5), "   42");
        assert_eq!(fit_right("123456", 4), "123…");
    }

    #[test]
    fn cursor_column_scrolls_into_view() {
        let widths = [5, 10, 10, 10, 10];
        let mut st = GridState {
            col: 4,
            ..Default::default()
        };
        // 1 frozen column (8) + room for two 13-wide columns.
        keep_column_visible(&mut st, &widths, 1, 8 + 26);
        assert_eq!(st.col_offset, 2);
        st.col = 1;
        keep_column_visible(&mut st, &widths, 1, 8 + 26);
        assert_eq!(st.col_offset, 0);
    }

    #[test]
    fn fit_pads_and_truncates() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本語", 5), "日本…");
        assert_eq!(fit("日本語", 4), "日… ");
        assert_eq!(fit("line1\nline2", 10), "line1…    ");
        assert_eq!(fit("\nx", 3), "…  ");
        assert_eq!(fit("abc", 0), "");
    }
}
