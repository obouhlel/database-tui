//! Read-only SQL text with syntax highlighting, line numbers and scrolling
//! (DDL in the Schema tab; the SQL editor reuses the highlighter).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::sql::lexer::{TokenKind, is_keyword, is_type_name, tokenize};
use crate::ui::theme::Theme;

/// Splits highlighted SQL into one `Vec<Span>` per line.
pub fn highlight_lines(src: &str, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    for tok in tokenize(src) {
        let text = tok.text(src);
        let style = token_style(tok.kind, text, theme);
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines
                    .last_mut()
                    .expect("never empty")
                    .push(Span::styled(part.to_string(), style));
            }
        }
    }
    if src.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn token_style(kind: TokenKind, text: &str, t: &Theme) -> Style {
    match kind {
        TokenKind::Word if is_keyword(text) => {
            Style::new().fg(t.sql_keyword).add_modifier(Modifier::BOLD)
        }
        TokenKind::Word if is_type_name(text) => Style::new().fg(t.sql_type),
        TokenKind::String => Style::new().fg(t.sql_string),
        TokenKind::Number | TokenKind::Param => Style::new().fg(t.sql_number),
        TokenKind::LineComment | TokenKind::BlockComment => Style::new()
            .fg(t.sql_comment)
            .add_modifier(Modifier::ITALIC),
        _ => Style::new().fg(t.fg),
    }
}

/// Removes the first `skip` display columns of a line.
fn hscroll(spans: Vec<Span<'static>>, skip: usize) -> Vec<Span<'static>> {
    if skip == 0 {
        return spans;
    }
    let mut remaining = skip;
    let mut out = Vec::new();
    for span in spans {
        if remaining == 0 {
            out.push(span);
            continue;
        }
        let mut kept = String::new();
        for ch in span.content.chars() {
            if remaining > 0 {
                remaining = remaining.saturating_sub(ch.width().unwrap_or(0));
            } else {
                kept.push(ch);
            }
        }
        if !kept.is_empty() {
            out.push(Span::styled(kept, span.style));
        }
    }
    out
}

/// Draws `src` from line `scroll`, shifted left by `hscroll_cols` columns.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    src: &str,
    scroll: usize,
    hscroll_cols: usize,
    theme: &Theme,
) {
    let lines = highlight_lines(src, theme);
    let gutter = lines.len().to_string().len();
    let num_style = Style::new().fg(theme.line_number);
    for (i, spans) in lines
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(area.height as usize)
    {
        let mut line = vec![Span::styled(format!("{:>gutter$} ", i + 1), num_style)];
        line.extend(hscroll(spans, hscroll_cols));
        let r = Rect {
            y: area.y + (i - scroll) as u16,
            height: 1,
            ..area
        };
        frame.render_widget(Line::from(line), r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Vec<Span>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn multiline_tokens_are_split() {
        let t = Theme::dark();
        let lines = highlight_lines("SELECT 'a\nb' /* c\nd */;\n", &t);
        assert_eq!(plain(&lines), vec!["SELECT 'a", "b' /* c", "d */;"]);
        assert_eq!(lines[0][0].style.fg, Some(t.sql_keyword));
        assert_eq!(lines[1][0].style.fg, Some(t.sql_string));
    }

    #[test]
    fn horizontal_scroll() {
        let t = Theme::dark();
        let line = highlight_lines("CREATE TABLE x", &t).remove(0);
        assert_eq!(plain(&[hscroll(line, 7)]), vec!["TABLE x"]);
    }
}
