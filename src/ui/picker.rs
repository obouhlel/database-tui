//! Connection picker shown at startup.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::{centered, spinner};
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let n = app.connections.len() as u16;
    let width = 90.min(area.width);
    // Warnings wrap: count the lines they will take at this width.
    let inner_w = usize::from(width.saturating_sub(2)).max(1);
    let warn_lines: usize = app
        .warnings
        .iter()
        .map(|w| (w.chars().count() + 4).div_ceil(inner_w))
        .sum();
    let warn_h = if app.warnings.is_empty() {
        0
    } else {
        warn_lines as u16 + 2
    };
    let height = (n.max(2) + 4 + warn_h).min(area.height);
    let rect = centered(area, width, height);

    let block = Block::bordered()
        .title(Span::styled(
            " pgtui — connexions ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .border_style(t.border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let [list_area, footer, warn_area] = Layout::vertical([
        Constraint::Length(n.max(2)),
        Constraint::Length(2),
        Constraint::Length(warn_h),
    ])
    .areas(inner);

    if app.connections.is_empty() {
        let text = vec![
            Line::styled("Aucune connexion configurée.", Style::new().fg(t.warning)),
            Line::styled(
                "Ajoutez une section [[connections]] dans ~/.config/pgtui/config.toml, ou lancez pgtui --url postgres://…",
                t.dim(),
            ),
        ];
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), list_area);
    }

    let name_w = app
        .connections
        .iter()
        .map(|(c, _)| c.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for (i, (conn, _)) in app
        .connections
        .iter()
        .enumerate()
        .take(list_area.height as usize)
    {
        let rect = Rect {
            y: list_area.y + i as u16,
            height: 1,
            ..list_area
        };
        let selected = i == app.picker_cursor;
        let color = conn.parsed_color().unwrap_or(t.accent);
        let mut spans = vec![
            Span::styled(
                if selected { "▶ " } else { "  " },
                Style::new().fg(t.accent),
            ),
            Span::styled("● ", Style::new().fg(color)),
            Span::styled(
                format!("{:<name_w$}", conn.name),
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(conn.target(), t.dim()),
            Span::styled(format!("  ssl:{}", conn.sslmode.as_str()), t.dim()),
        ];
        if conn.read_only {
            spans.push(Span::styled(
                "  RO",
                Style::new().fg(t.warning).add_modifier(Modifier::BOLD),
            ));
        }
        if conn.is_production() {
            spans.push(Span::styled(
                "  PROD",
                Style::new().fg(t.error).add_modifier(Modifier::BOLD),
            ));
        }
        if selected {
            frame.buffer_mut().set_style(rect, t.selection(true));
        }
        let mut line = Line::from(spans);
        if selected {
            line = line.patch_style(Style::new().bg(t.selection_bg));
        }
        frame.render_widget(line, rect);
    }

    let footer_text = match &app.connecting {
        Some((conn, since)) => Line::styled(
            format!(
                "{} Connexion à {} ({})… {:.1} s",
                spinner(app.spinner),
                conn.name,
                conn.target(),
                since.elapsed().as_secs_f64()
            ),
            Style::new().fg(t.info),
        ),
        None => Line::styled("Entrée pour se connecter", t.dim()),
    };
    frame.render_widget(Paragraph::new(vec![Line::default(), footer_text]), footer);

    if !app.warnings.is_empty() {
        let mut lines = vec![
            Line::default(),
            Line::styled("Avertissements :", Style::new().fg(t.warning)),
        ];
        lines.extend(
            app.warnings
                .iter()
                .map(|w| Line::styled(format!("  • {w}"), Style::new().fg(t.warning))),
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), warn_area);
    }
}
