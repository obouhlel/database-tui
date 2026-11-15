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

/// Draws `source` in `area` and updates the viewport fields of `state`.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    source: &dyn GridSource,
    state: &mut GridState,
    theme: &Theme,
    focused: bool,
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
    frame.render_widget(
        build_line(source, None, &visible, theme, sep_style, header_style),
        header_area,
    );
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
    for (i, row) in (state.row_offset..rows)
        .take(state.viewport_rows)
        .enumerate()
    {
        let r = Rect {
            y: area.y + 1 + i as u16,
            height: 1,
            ..area
        };
        let selected = row == state.row;
        if selected {
            frame.buffer_mut().set_style(r, theme.selection(focused));
        }
        let mut line = build_line(source, Some(row), &visible, theme, sep_style, Style::new());
        if selected {
            line = line.patch_style(Style::new().bg(if focused {
                theme.selection_bg
            } else {
                theme.selection_inactive_bg
            }));
        }
        frame.render_widget(line, r);
    }
}

fn build_line<'a>(
    source: &'a dyn GridSource,
    row: Option<usize>,
    visible: &[(usize, u16)],
    theme: &Theme,
    sep: Style,
    base: Style,
) -> Line<'a> {
    let mut spans = Vec::with_capacity(visible.len() * 2);
    let frozen = source.frozen();
    for (i, (c, w)) in visible.iter().enumerate() {
        let (text, style) = match row {
            None => (source.headers()[*c].as_str().into(), base),
            Some(r) => {
                let (t, s) = source.cell(r, *c);
                (t, theme.cell(s))
            }
        };
        let inner = w.saturating_sub(2) as usize;
        spans.push(Span::styled(format!(" {} ", fit(&text, inner)), style));
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
