//! Data tab popups: structured filter, copy format, row detail.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::centered;
use crate::app::App;
use crate::app::export::CopyFormat;
use crate::app::filter_editor::{Field, FilterEditor};
use crate::app::value::pretty;
use crate::db::data::Combinator;

fn popup<'a>(app: &App, title: String, hint: &'a str) -> Block<'a> {
    let t = &app.theme;
    Block::bordered()
        .title(Span::styled(
            title,
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(hint, t.dim()))
        .border_style(t.border(true))
        .style(t.base())
}

pub fn draw_filter_editor(frame: &mut Frame, app: &App, e: &FilterEditor, area: Rect) {
    let t = &app.theme;
    let height = (e.rows.len() as u16 + 5).min(area.height);
    let rect = centered(area, 96, height);
    let comb = match e.combinator {
        Combinator::And => "toutes les conditions (AND)",
        Combinator::Or => "au moins une condition (OR)",
    };
    let block = popup(
        app,
        format!(" Filtre : {comb} "),
        " Tab champ · Up/Down ligne · Left/Right choix · Ctrl+n ajouter · Ctrl+d supprimer · Ctrl+o AND/OR · Enter appliquer · Esc ",
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let col_w = e
        .columns
        .iter()
        .map(|c| c.width())
        .max()
        .unwrap_or(4)
        .clamp(6, 30);
    let mut lines = vec![Line::styled(
        format!("   {:<col_w$}  {:<11}  valeur", "colonne", "opérateur"),
        t.dim(),
    )];
    let mut cursor_pos = None;
    for (i, row) in e.rows.iter().enumerate() {
        let active = i == e.cursor;
        let field_style = |f: Field| {
            if active && e.field == f {
                Style::new()
                    .bg(t.accent)
                    .fg(t.tab_active_fg)
                    .add_modifier(Modifier::BOLD)
            } else if active {
                Style::new().fg(t.fg).bg(t.selection_inactive_bg)
            } else {
                Style::new().fg(t.fg)
            }
        };
        let joiner = if i == 0 {
            "   ".to_string()
        } else {
            format!("{:<3}", e.combinator.keyword())
        };
        let value = if row.op.has_value() {
            row.value.text().to_string()
        } else {
            "—".to_string()
        };
        let prefix = format!(
            "{joiner}{:<col_w$}  {:<11}  ",
            e.columns[row.column],
            row.op.label()
        );
        if active && e.field == Field::Value && row.op.has_value() {
            cursor_pos = Some((
                inner.x + prefix.width() as u16 + row.value.cursor_width() as u16,
                inner.y + 1 + i as u16,
            ));
        }
        lines.push(Line::from(vec![
            Span::styled(joiner, t.dim()),
            Span::styled(
                format!("{:<col_w$}", e.columns[row.column]),
                field_style(Field::Column),
            ),
            Span::raw("  "),
            Span::styled(format!("{:<11}", row.op.label()), field_style(Field::Op)),
            Span::raw("  "),
            Span::styled(format!("{value:<20}"), field_style(Field::Value)),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Les valeurs sont envoyées en paramètres ; LIKE / ILIKE / ~ portent sur le texte de la colonne.",
        t.dim(),
    ));
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some((x, y)) = cursor_pos {
        frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
    }
}

pub fn draw_copy_format(frame: &mut Frame, app: &App, cursor: usize, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 40, CopyFormat::ALL.len() as u16 + 2);
    let what = if app.data.visual.is_some() {
        "sélection"
    } else {
        "ligne"
    };
    let block = popup(
        app,
        format!(" Copier la {what} "),
        " lettre, ou Up/Down et Enter · Esc ",
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let lines: Vec<Line> = CopyFormat::ALL
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let style = if i == cursor {
                t.selection(true)
            } else {
                Style::new().fg(t.fg)
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ", f.key()),
                    Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {:<30}", f.label()), style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

pub fn draw_row_detail(frame: &mut Frame, app: &App, scroll: u16, area: Rect) {
    let t = &app.theme;
    let d = &app.data;
    let rect = centered(
        area,
        area.width.saturating_sub(8).max(40),
        area.height.saturating_sub(2),
    );
    let title = match app.data_position() {
        Some(p) => format!(" Détail — {p} "),
        None => " Détail ".to_string(),
    };
    let block = popup(
        app,
        title,
        " j/k défiler · [ ] ligne précédente / suivante · y copier en JSON · Esc ",
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let Some(row) = d.row_at(d.grid.row) else {
        frame.render_widget(
            Line::styled("Ligne en cours de chargement…", t.dim()),
            inner,
        );
        return;
    };
    let mut lines = Vec::new();
    for &i in &d.order {
        let col = &d.columns[i];
        let name_style = if col.key {
            Style::new().fg(t.key)
        } else {
            Style::new().fg(t.accent)
        };
        lines.push(Line::from(vec![
            Span::styled(col.name.clone(), name_style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", col.data_type), t.dim()),
        ]));
        match &row[i] {
            None => lines.push(Line::styled(
                "  NULL",
                t.cell(crate::app::grid::CellStyle::Null),
            )),
            Some(v) if v.is_empty() => lines.push(Line::styled("  (chaîne vide)", t.dim())),
            Some(v) => {
                for l in pretty(col.kind, v).lines() {
                    // Wrap long lines on character boundaries.
                    let width = inner.width.saturating_sub(2).max(10) as usize;
                    let chars: Vec<char> = l.chars().collect();
                    if chars.is_empty() {
                        lines.push(Line::raw(""));
                    }
                    for chunk in chars.chunks(width) {
                        lines.push(Line::styled(
                            format!("  {}", chunk.iter().collect::<String>()),
                            Style::new().fg(t.fg),
                        ));
                    }
                }
            }
        }
    }
    let hidden = d.columns.len() - d.order.len();
    if hidden > 0 {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("{hidden} colonne(s) masquée(s) non affichée(s)"),
            t.dim(),
        ));
    }
    let max = (lines.len() as u16).saturating_sub(inner.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll.min(max), 0)), inner);
}

pub fn draw_export(
    frame: &mut Frame,
    app: &App,
    d: &crate::app::export_dialog::ExportDialog,
    area: Rect,
) {
    let t = &app.theme;
    let rect = centered(area, 90, 11);
    let block = popup(
        app,
        " Exporter ".into(),
        " Up/Down format · Entrée exporter · Esc annuler ",
    );
    frame.render_widget(Clear, rect);
    let inner = block.inner(rect);
    let what = if d.result {
        format!(
            "Lignes du résultat ({})",
            d.rows.clone().unwrap_or_else(|| "?".into())
        )
    } else {
        match &d.rows {
            Some(n) => format!("Toute la requête : {n} ligne(s), filtres et tri compris"),
            None => "Toute la requête (filtres et tri compris)".into(),
        }
    };
    let mut lines = vec![Line::styled(what, t.dim()), Line::default()];
    for (i, f) in CopyFormat::ALL.iter().enumerate() {
        let selected = i == d.format;
        let style = if selected {
            Style::new()
                .bg(t.selection_bg)
                .fg(t.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.fg)
        };
        lines.push(Line::styled(format!(" {:<16}", f.label()), style));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Fichier : ", t.dim()),
        Span::styled(d.path.text().to_string(), Style::new().fg(t.fg)),
    ]));
    let y = inner.y + lines.len() as u16 - 1;
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    frame.set_cursor_position((
        (inner.x + 10 + d.path.cursor_width() as u16).min(inner.right().saturating_sub(1)),
        y,
    ));
}
