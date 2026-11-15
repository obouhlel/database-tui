//! Left panel: schema tree with fuzzy filter. Only visible rows are drawn.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{highlighted, spinner};
use crate::app::tree::RowKind;
use crate::app::{App, Focus};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tree;
    let t = app.theme.clone();
    let title = match &app.session {
        Some(s) => format!(" {} — {} base(s) ", s.conn.name, s.databases.len()),
        None => " Objets ".to_string(),
    };
    let block = Block::bordered()
        .title(Span::styled(
            title,
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ))
        .border_style(t.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let show_filter = app.tree.filter_editing || !app.tree.filter.is_empty();
    let [list_area, filter_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::from(show_filter)),
    ])
    .areas(inner);

    app.tree.viewport = list_area.height as usize;
    app.tree.scroll_into_view();

    if app.tree.rows.is_empty() {
        let text = if app.tree.filter.is_empty() {
            "Aucun objet"
        } else {
            "Aucun résultat"
        };
        frame.render_widget(Paragraph::new(text).style(t.dim()), list_area);
    }

    let tree = &app.tree;
    let hl = Style::new()
        .fg(t.matched)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    for (i, row) in tree
        .rows
        .iter()
        .enumerate()
        .skip(tree.offset)
        .take(list_area.height as usize)
    {
        let y = list_area.y + (i - tree.offset) as u16;
        let rect = Rect {
            y,
            height: 1,
            ..list_area
        };
        let selected = i == tree.cursor;

        let mut spans = vec![Span::raw("  ".repeat(row.depth as usize))];
        let arrow = if row.expanded { "▾ " } else { "▸ " };
        let (icon, icon_color, label_style) = match row.kind {
            RowKind::Database => (
                arrow,
                t.accent,
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            RowKind::Schema => (
                arrow,
                t.schema,
                Style::new().fg(t.schema).add_modifier(Modifier::BOLD),
            ),
            RowKind::Group(_) => (arrow, t.dim, Style::new().fg(t.fg)),
            RowKind::Object(k) => ("  ", t.kind_color(k), Style::new().fg(t.kind_color(k))),
        };
        spans.push(Span::styled(icon, Style::new().fg(icon_color)));
        spans.extend(highlighted(&row.label, &row.highlights, label_style, hl));
        if let Some(n) = row.count {
            spans.push(Span::styled(format!(" {n}"), t.dim()));
        }
        if let Some(note) = &row.note {
            let style = if note == "chargement…" {
                Style::new().fg(t.info)
            } else if note == "erreur" || note == "accès refusé" {
                Style::new().fg(t.warning)
            } else {
                t.dim()
            };
            let text = if note == "chargement…" {
                format!(" {} {note}", spinner(app.spinner))
            } else {
                format!(" {note}")
            };
            spans.push(Span::styled(text, style));
        }
        if selected {
            frame.buffer_mut().set_style(rect, t.selection(focused));
        }
        let mut line = Line::from(spans);
        if selected {
            line = line.patch_style(t.selection(focused));
        }
        frame.render_widget(line, rect);
    }

    if show_filter {
        let style = if tree.filter_editing {
            Style::new().fg(t.accent)
        } else {
            t.dim()
        };
        let line = Line::from(vec![
            Span::styled("/", style.add_modifier(Modifier::BOLD)),
            Span::styled(tree.filter.text().to_string(), Style::new().fg(t.fg)),
        ]);
        frame.render_widget(line, filter_area);
        if tree.filter_editing {
            let x = filter_area.x + 1 + tree.filter.cursor_width() as u16;
            frame
                .set_cursor_position((x.min(filter_area.right().saturating_sub(1)), filter_area.y));
        }
    }
}
