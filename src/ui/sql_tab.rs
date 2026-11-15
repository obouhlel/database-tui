//! SQL tab: target and transaction line, editor (highlighting, line
//! numbers, selection), result sub-tabs and the active result. Also the
//! popups of the tab (history search, saved queries, confirmations).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::table_view::{self, Mark};
use super::{centered, spinner, sql_view, status_bar::fmt_duration};
use crate::app::edit::After;
use crate::app::editor::Pos;
use crate::app::sql_tab::{HistorySearch, SqlFocus, TxnState};
use crate::app::{App, Focus, Overlay};
use crate::ui::theme::Theme;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let has_results = !app.sql.results.is_empty();
    let editor_h = if has_results {
        (area.height.saturating_sub(1) * 45 / 100).max(5)
    } else {
        area.height.saturating_sub(1)
    };
    let [head, editor_area, tabs_area, results_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(editor_h),
        Constraint::Length(u16::from(has_results)),
        Constraint::Fill(1),
    ])
    .areas(area);

    draw_header(frame, app, &t, head);
    draw_editor(frame, app, &t, editor_area);
    if has_results {
        draw_result_tabs(frame, app, &t, tabs_area);
        draw_result(frame, app, &t, results_area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mut spans = Vec::new();
    match app.sql_target() {
        Some((c, d)) => {
            spans.push(Span::styled(
                c,
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" / {d}"), Style::new().fg(t.fg)));
        }
        None => spans.push(Span::styled(
            "aucune connexion : connectez-vous dans l'arbre",
            t.dim(),
        )),
    }
    match app.sql.txn {
        TxnState::Idle => spans.push(Span::styled("  autocommit", t.dim())),
        TxnState::Open => spans.push(Span::styled(
            "  TRANSACTION OUVERTE (COMMIT / ROLLBACK)",
            Style::new().fg(t.warning).add_modifier(Modifier::BOLD),
        )),
        TxnState::Aborted => spans.push(Span::styled(
            "  TRANSACTION EN ÉCHEC : ROLLBACK requis",
            Style::new().fg(t.error).add_modifier(Modifier::BOLD),
        )),
    }
    if let Some(f) = &app.sql.file {
        spans.push(Span::styled(format!("  {f}.sql"), t.dim()));
    }
    if let Some(run) = &app.sql.running {
        let done = run.total - run.queue.len();
        spans.push(Span::styled(
            format!(
                "  {} exécution {done}/{} · {}",
                spinner(app.spinner),
                run.total,
                fmt_duration(run.started.elapsed())
            ),
            Style::new().fg(t.info),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_editor(frame: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    if area.height == 0 {
        return;
    }
    let focused = app.focus == Focus::Center && app.sql.focus == SqlFocus::Editor;
    let ed = &mut app.sql.editor;
    let gutter = ed.lines().len().to_string().len().max(3);
    let text_w = (area.width as usize).saturating_sub(gutter + 1);
    ed.scroll_into_view(area.height as usize, text_w);
    let (scroll, hscroll) = (ed.scroll, ed.hscroll);

    if ed.is_empty() && !focused {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "Écrivez une requête, puis Ctrl+Entrée (ou Ctrl+e) pour l'exécuter,",
                    t.dim(),
                ),
                Line::styled("F5 pour tout le script, Ctrl+r pour l'historique.", t.dim()),
            ]),
            area,
        );
        return;
    }
    let text = ed.text();
    let highlighted = sql_view::highlight_lines(&text, t);
    let num = Style::new().fg(t.line_number);
    let num_cur = Style::new().fg(t.fg);
    for (i, spans) in highlighted
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(area.height as usize)
    {
        let style = if i == ed.cursor.row && focused {
            num_cur
        } else {
            num
        };
        let mut line = vec![Span::styled(format!("{:>gutter$} ", i + 1), style)];
        line.extend(sql_view::hscroll(spans, hscroll));
        let r = Rect {
            y: area.y + (i - scroll) as u16,
            height: 1,
            ..area
        };
        frame.render_widget(Line::from(line), r);
    }

    // Selection.
    if let Some(sel) = ed.selection() {
        let (s, e) = (ed.pos_of(sel.start), ed.pos_of(sel.end));
        let width_to = |p: Pos| -> usize {
            let line = &ed.lines()[p.row];
            let b = line
                .char_indices()
                .nth(p.col)
                .map_or(line.len(), |(i, _)| i);
            line[..b].width()
        };
        let x0 = area.x + gutter as u16 + 1;
        for row in s.row.max(scroll)..=e.row.min(scroll + area.height as usize - 1) {
            let from = if row == s.row { width_to(s) } else { 0 };
            let to = if row == e.row {
                width_to(e)
            } else {
                ed.lines()[row].width() + 1
            };
            let from = from.saturating_sub(hscroll);
            let to = to.saturating_sub(hscroll).min(text_w);
            if to > from {
                let r = Rect {
                    x: x0 + from as u16,
                    y: area.y + (row - scroll) as u16,
                    width: (to - from) as u16,
                    height: 1,
                };
                frame
                    .buffer_mut()
                    .set_style(r, Style::new().bg(t.selection_bg).fg(t.selection_fg));
            }
        }
    }
    if focused && app.overlay.is_none() {
        let x = (ed.cursor_width().saturating_sub(hscroll) + gutter + 1) as u16;
        let y = (ed.cursor.row - scroll) as u16;
        if y < area.height {
            frame.set_cursor_position((area.x + x.min(area.width.saturating_sub(1)), area.y + y));
        }
    }
}

fn draw_result_tabs(frame: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let focused = app.focus == Focus::Center && app.sql.focus == SqlFocus::Results;
    let mut spans = vec![Span::styled("─ ", Style::new().fg(t.border))];
    for (i, r) in app.sql.results.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, r.label);
        let style = if i == app.sql.active {
            let bg = if r.error {
                t.error
            } else if focused {
                t.tab_active_bg
            } else {
                t.border
            };
            Style::new()
                .bg(bg)
                .fg(t.tab_active_fg)
                .add_modifier(Modifier::BOLD)
        } else if r.error {
            Style::new().fg(t.error)
        } else {
            t.dim()
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    if let Some(r) = app.sql.results.get(app.sql.active) {
        spans.push(Span::styled(
            format!(
                " {} · {}",
                r.message.lines().next().unwrap_or(""),
                fmt_duration(r.elapsed)
            ),
            if r.error {
                Style::new().fg(t.error)
            } else {
                t.dim()
            },
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_result(frame: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let focused = app.focus == Focus::Center && app.sql.focus == SqlFocus::Results;
    let i = app.sql.active;
    let Some(r) = app.sql.results.get(i) else {
        return;
    };
    if r.view.is_none() {
        let mut lines = Vec::new();
        if r.error {
            for l in r.message.lines() {
                lines.push(Line::styled(l.to_string(), Style::new().fg(t.error)));
            }
            lines.push(Line::default());
            lines.push(Line::styled("Instruction :", t.dim()));
            lines.extend(
                sql_view::highlight_lines(&r.statement, t)
                    .into_iter()
                    .map(Line::from),
            );
        } else {
            lines.push(Line::styled(r.message.clone(), Style::new().fg(t.success)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        return;
    }
    let search_line = focused && matches!(app.overlay, Some(Overlay::DataSearch { .. }));
    let [grid_area, input_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(u16::from(search_line)),
    ])
    .areas(area);
    let view = app.sql.results[i].view.as_mut().expect("checked");
    let mut grid = view.grid;
    {
        let data = &*view;
        let mark = |row: usize, col: usize| {
            if data.is_selected(row, col) {
                Mark::Selected
            } else if data.search.is_some() && data.is_match(row, col) {
                Mark::Match
            } else {
                Mark::None
            }
        };
        let opts = table_view::Options {
            focused,
            cell_cursor: true,
            mark: Some(&mark),
        };
        table_view::draw(frame, grid_area, data, &mut grid, t, &opts);
    }
    view.grid = grid;
    if let Some(Overlay::DataSearch { input, .. }) = &app.overlay
        && search_line
    {
        let line = Line::from(vec![
            Span::styled("/", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(input.text().to_string(), Style::new().fg(t.fg)),
        ]);
        frame.render_widget(line, input_area);
        frame.set_cursor_position((
            (input_area.x + 1 + input.cursor_width() as u16)
                .min(input_area.right().saturating_sub(1)),
            input_area.y,
        ));
    }
}

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

pub fn draw_confirm_writes(
    frame: &mut Frame,
    app: &App,
    risky: &[String],
    target: &(String, String),
    area: Rect,
) {
    let t = &app.theme;
    let rect = centered(area, 80, (risky.len() as u16 + 7).min(area.height));
    let block = popup(
        t,
        " Modification de toutes les lignes ? ".into(),
        " o : exécuter · n / Esc : annuler ",
        t.error,
    );
    frame.render_widget(Clear, rect);
    let mut lines = vec![Line::from(vec![
        Span::styled("Sur ", t.dim()),
        Span::styled(
            format!("{} / {}", target.0, target.1),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(", sans clause WHERE :", t.dim()),
    ])];
    lines.push(Line::default());
    for r in risky {
        lines.push(Line::styled(format!("  {r}"), Style::new().fg(t.error)));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Chaque ligne de la table sera touchée.",
        Style::new().fg(t.fg),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub fn draw_confirm_txn_exit(frame: &mut Frame, app: &App, after: &After, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 72, 8);
    let block = popup(
        t,
        " Transaction ouverte ".into(),
        " o : continuer · n / Esc : annuler ",
        t.warning,
    );
    frame.render_widget(Clear, rect);
    let target = app
        .sql
        .target
        .as_ref()
        .map(|(c, d)| format!("{c} / {d}"))
        .unwrap_or_default();
    let lines = vec![
        Line::from(vec![
            Span::styled("Une transaction est ouverte sur ", Style::new().fg(t.fg)),
            Span::styled(
                target,
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(".", Style::new().fg(t.fg)),
        ]),
        Line::default(),
        Line::styled(
            format!(
                "Pour {}, la session est fermée et le serveur annule la transaction (ROLLBACK).",
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

pub fn draw_history(frame: &mut Frame, app: &App, s: &HistorySearch, area: Rect) {
    let t = &app.theme;
    let rect = centered(
        area,
        area.width.saturating_sub(6).min(120),
        area.height.saturating_sub(2),
    );
    let block = popup(
        t,
        format!(" Historique ({}) ", app.sql.history.len()),
        " Up/Down choisir · Entrée insérer · Esc fermer ",
        t.accent,
    );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let [input_area, list_area, preview_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(8.min(inner.height / 3)),
    ])
    .areas(inner);
    frame.render_widget(
        Line::from(vec![
            Span::styled("> ", Style::new().fg(t.accent)),
            Span::styled(s.input.text().to_string(), Style::new().fg(t.fg)),
            Span::styled(format!("   {} résultat(s)", s.matches.len()), t.dim()),
        ]),
        input_area,
    );
    frame.set_cursor_position((
        input_area.x + 2 + s.input.cursor_width() as u16,
        input_area.y,
    ));
    let h = list_area.height as usize;
    let offset = s.cursor.saturating_sub(h.saturating_sub(1));
    for (row, idx) in s.matches.iter().enumerate().skip(offset).take(h) {
        let e = &app.sql.history[*idx];
        let when = chrono::DateTime::from_timestamp(e.at, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%d/%m %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let one_line: String = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let selected = row == s.cursor;
        let base = if selected {
            Style::new().bg(t.selection_bg).fg(t.selection_fg)
        } else {
            Style::new().fg(t.fg)
        };
        let status = if e.ok {
            Style::new().fg(t.success)
        } else {
            Style::new().fg(t.error)
        };
        let line = Line::from(vec![
            Span::styled(
                format!("{when} "),
                base.fg(if selected { t.selection_fg } else { t.dim }),
            ),
            Span::styled(
                if e.ok { "ok  " } else { "err " },
                if selected { base } else { status },
            ),
            Span::styled(
                format!("{:<12} ", e.connection),
                base.fg(if selected { t.selection_fg } else { t.accent }),
            ),
            Span::styled(one_line, base),
        ]);
        let r = Rect {
            y: list_area.y + (row - offset) as u16,
            height: 1,
            ..list_area
        };
        if selected {
            frame.buffer_mut().set_style(r, base);
        }
        frame.render_widget(line, r);
    }
    if let Some(e) = s
        .matches
        .get(s.cursor)
        .and_then(|i| app.sql.history.get(*i))
    {
        let mut lines = vec![Line::styled(
            format!("{} / {} · {} ms", e.connection, e.database, e.ms),
            t.dim(),
        )];
        lines.extend(
            sql_view::highlight_lines(&e.sql, t)
                .into_iter()
                .map(Line::from),
        );
        frame.render_widget(Paragraph::new(lines), preview_area);
    }
}

pub fn draw_query_list(frame: &mut Frame, app: &App, names: &[String], cursor: usize, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 60, (names.len() as u16 + 2).min(area.height));
    let block = popup(
        t,
        " Requêtes enregistrées ".into(),
        " Entrée ouvrir · Esc fermer ",
        t.accent,
    );
    frame.render_widget(Clear, rect);
    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if i == cursor {
                Line::styled(
                    format!(" {n}"),
                    Style::new().bg(t.selection_bg).fg(t.selection_fg),
                )
            } else {
                Line::styled(format!(" {n}"), Style::new().fg(t.fg))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}
