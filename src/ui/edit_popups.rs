//! Data tab editing: inline / multi-line cell editor, pending changes view
//! (diff and generated SQL), confirmations.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::table_view::{self, fit};
use super::{centered, sql_view};
use crate::app::App;
use crate::app::edit::{After, CellEditor, ChangesMode, ChangesView, EditorInput};
use crate::app::input::LineInput;
use crate::app::textarea::TextArea;
use crate::db::changes::ChangeCounts;
use crate::ui::theme::Theme;

fn popup<'a>(t: &Theme, title: String, hint: &'a str, color: ratatui::style::Color) -> Block<'a> {
    Block::bordered()
        .title(Span::styled(
            title,
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(hint, t.dim()))
        .border_style(Style::new().fg(color))
        .style(t.base())
}

/// One-line editor drawn over the cell (`x`, `y`, `w` from the grid).
pub fn draw_inline_editor(
    frame: &mut Frame,
    app: &App,
    ed: &CellEditor,
    input: &LineInput,
    cell: (u16, u16, u16),
    bounds: Rect,
) {
    let t = &app.theme;
    let (x, y, w) = cell;
    let w = w.max(24).min(bounds.right().saturating_sub(x));
    let rect = Rect {
        x,
        y,
        width: w,
        height: 1,
    };
    let style = Style::new().bg(t.selection_bg).fg(t.selection_fg);
    let text = input.text();
    // Keep the cursor visible in a long value.
    let cursor = input.cursor_width();
    let skip = cursor.saturating_sub(w.saturating_sub(1) as usize);
    let visible: String = skip_width(text, skip);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(fit(&visible, w as usize)).style(style), rect);
    frame.set_cursor_position((x + (cursor - skip) as u16, y));
    if let Some(err) = &ed.error {
        let ey = if y + 1 < bounds.bottom() {
            y + 1
        } else {
            y.saturating_sub(1)
        };
        let msg = format!(" {err} ");
        let ew = (msg.width() as u16).min(bounds.right().saturating_sub(x));
        let r = Rect {
            x,
            y: ey,
            width: ew,
            height: 1,
        };
        frame.render_widget(Clear, r);
        frame.render_widget(
            Paragraph::new(msg).style(Style::new().bg(t.error).fg(t.tab_active_fg)),
            r,
        );
    }
}

/// Drops the first `width` display columns of `s`.
fn skip_width(s: &str, width: usize) -> String {
    let mut used = 0;
    s.chars()
        .skip_while(|c| {
            if used >= width {
                return false;
            }
            used += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            true
        })
        .collect()
}

/// Multi-line editor popup (long values, JSON).
pub fn draw_multi_editor(frame: &mut Frame, app: &App, ed: &CellEditor, area: Rect) {
    let t = &app.theme;
    let EditorInput::Multi(ta) = &ed.input else {
        return;
    };
    let rect = centered(
        area,
        area.width.saturating_sub(8).min(110),
        area.height.saturating_sub(4).min(30),
    );
    let block = popup(
        t,
        format!(" {} ({}) ", ed.column, ed.data_type),
        " Ctrl+s / Alt+Enter valider · Esc annuler · Ctrl+n NULL · Ctrl+d DEFAULT · Ctrl+g UUID ",
        t.accent,
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let [text_area, err_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::from(ed.error.is_some())),
    ])
    .areas(inner);
    draw_textarea(frame, t, ta, text_area, None);
    if let Some(err) = &ed.error {
        frame.render_widget(
            Line::styled(err.clone(), Style::new().fg(t.error)),
            err_area,
        );
    }
}

/// Text with line numbers and the cursor; `highlight` colours SQL.
fn draw_textarea(frame: &mut Frame, t: &Theme, ta: &TextArea, area: Rect, highlight: Option<()>) {
    let h = area.height.max(1) as usize;
    let scroll = ta.row.saturating_sub(h - 1);
    let gutter = ta.lines().len().to_string().len().max(2);
    let spans_of = |line: &str| -> Vec<Span<'static>> {
        match highlight {
            Some(()) => sql_view::highlight_lines(line, t)
                .into_iter()
                .next()
                .unwrap_or_default(),
            None => vec![Span::styled(line.to_string(), Style::new().fg(t.fg))],
        }
    };
    let lines: Vec<Line> = ta
        .lines()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(h)
        .map(|(i, l)| {
            let mut spans = vec![Span::styled(
                format!("{:>gutter$} ", i + 1),
                Style::new().fg(t.line_number),
            )];
            spans.extend(spans_of(l));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    let x = area.x + gutter as u16 + 1 + ta.cursor_width() as u16;
    let y = area.y + (ta.row - scroll) as u16;
    frame.set_cursor_position((x.min(area.right().saturating_sub(1)), y));
}

pub fn draw_changes(frame: &mut Frame, app: &App, v: &ChangesView, area: Rect) {
    let t = &app.theme;
    let rect = centered(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let hint = match (v.mode, v.editing.is_some()) {
        (_, true) => " Ctrl+s appliquer ce script · Esc fin de l'édition ",
        (ChangesMode::Diff, _) => " Tab SQL généré · j/k naviguer · Ctrl+s appliquer · Esc fermer ",
        (ChangesMode::Sql, _) => " Tab diff · e modifier le SQL · Ctrl+s appliquer · Esc fermer ",
    };
    let block = popup(
        t,
        format!(" Changements en attente : {} ", v.counts.describe()),
        hint,
        t.accent,
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let [tabs_area, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(inner);
    let tab = |label: &str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .bg(t.tab_active_bg)
                    .fg(t.tab_active_fg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), t.dim())
        }
    };
    frame.render_widget(
        Line::from(vec![
            tab("Diff", v.mode == ChangesMode::Diff),
            Span::raw(" "),
            tab("SQL généré", v.mode == ChangesMode::Sql),
        ]),
        tabs_area,
    );
    match (v.mode, &v.editing) {
        (_, Some(ta)) => {
            let [head, text, foot] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(body);
            frame.render_widget(
                Line::styled(
                    "BEGIN;  -- script modifié à la main, sans détection de conflit",
                    t.dim(),
                ),
                head,
            );
            draw_textarea(frame, t, ta, text, Some(()));
            frame.render_widget(Line::styled("COMMIT;", t.dim()), foot);
        }
        (ChangesMode::Diff, None) => {
            let mut state = v.state;
            let opts = table_view::Options::rows(true);
            table_view::draw(frame, body, &v.grid, &mut state, t, &opts);
        }
        (ChangesMode::Sql, None) => {
            let mut lines = vec![Line::styled("BEGIN;", t.dim())];
            lines.extend(
                sql_view::highlight_lines(&v.sql, t)
                    .into_iter()
                    .map(Line::from),
            );
            lines.push(Line::styled("COMMIT;", t.dim()));
            lines.push(Line::default());
            lines.push(Line::styled(
                "Valeurs affichées en littéraux pour la lecture ; à l'exécution elles sont \
                 passées en paramètres, et chaque UPDATE / DELETE vérifie que la ligne n'a pas \
                 changé depuis sa lecture.",
                t.dim(),
            ));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((v.scroll, 0)),
                body,
            );
        }
    }
}

pub fn draw_confirm_apply(
    frame: &mut Frame,
    app: &App,
    counts: &ChangeCounts,
    production: bool,
    script: bool,
    input: &LineInput,
    area: Rect,
) {
    let t = &app.theme;
    let color = if production { t.error } else { t.warning };
    let height = if production { 11 } else { 8 };
    let rect = centered(area, 72, height);
    let hint = if production {
        " Enter valider · Esc annuler "
    } else {
        " o / Enter appliquer · n / Esc annuler "
    };
    let block = popup(t, " Appliquer les changements ? ".into(), hint, color);
    frame.render_widget(Clear, rect);
    let table = app
        .data
        .object
        .as_ref()
        .map(|o| format!("{} / {}", o.connection, o.qualified()))
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Table : ", t.dim()),
            Span::styled(table, Style::new().fg(t.fg).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Lignes : ", t.dim()),
            Span::styled(counts.describe(), Style::new().fg(t.fg)),
        ]),
        Line::styled(
            if script {
                "Script modifié à la main, exécuté dans une transaction."
            } else {
                "Une seule transaction ; tout est annulé à la première erreur ou au premier conflit."
            },
            t.dim(),
        ),
    ];
    let mut cursor = None;
    if production {
        let name = app
            .data
            .object
            .as_ref()
            .map(|o| o.connection.clone())
            .unwrap_or_default();
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                "Connexion de PRODUCTION. ",
                Style::new().fg(t.error).add_modifier(Modifier::BOLD),
            ),
            Span::styled("Tapez « ", Style::new().fg(t.fg)),
            Span::styled(name, Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" » pour confirmer :", Style::new().fg(t.fg)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("> ", Style::new().fg(t.accent)),
            Span::styled(input.text().to_string(), Style::new().fg(t.fg)),
        ]));
        cursor = Some(lines.len() as u16 - 1);
    }
    let inner = block.inner(rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
    if let Some(row) = cursor {
        frame.set_cursor_position((inner.x + 2 + input.cursor_width() as u16, inner.y + row));
    }
}

pub fn draw_confirm_discard(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 64, 6);
    let block = popup(
        t,
        " Abandonner les changements ? ".into(),
        " o : abandonner · n / Esc : garder ",
        t.warning,
    );
    frame.render_widget(Clear, rect);
    let counts = app.data.staging.counts();
    let lines = vec![
        Line::styled(
            format!("{} seront perdus.", counts.describe()),
            Style::new().fg(t.fg),
        ),
        Line::styled("u les restaure ensuite (annuler).", t.dim()),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

pub fn draw_pending_changes(frame: &mut Frame, app: &App, after: &After, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 72, 8);
    let block = popup(
        t,
        " Changements non appliqués ".into(),
        " a appliquer · x abandonner · D voir · Esc rester ",
        t.warning,
    );
    frame.render_widget(Clear, rect);
    let table = app
        .data
        .object
        .as_ref()
        .map(|o| o.qualified())
        .unwrap_or_default();
    let lines = vec![
        Line::from(vec![
            Span::styled(
                table,
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" : {}", app.data.staging.counts().describe()),
                Style::new().fg(t.fg),
            ),
        ]),
        Line::default(),
        Line::styled(
            format!(
                "Les changements sont propres à une table : appliquez-les ou abandonnez-les pour {}.",
                after.describe()
            ),
            t.dim(),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}
