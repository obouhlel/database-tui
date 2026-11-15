//! Data tab: summary lines, the paginated grid, and the search / WHERE
//! input line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::table_view::{self, Mark};
use super::{spinner, status_bar::fmt_duration};
use crate::app::{App, DetailsState, Focus, Overlay};
use crate::db::catalog::ObjectRef;

pub fn draw(frame: &mut Frame, app: &mut App, obj: &ObjectRef, area: Rect) {
    let t = app.theme.clone();
    if !obj.kind.has_rows() {
        frame.render_widget(Line::styled("Cet objet n'a pas de lignes.", t.dim()), area);
        return;
    }
    if !app.data.is_for(obj) {
        let text = match app.current_details() {
            Some(DetailsState::Failed(e)) => {
                Line::styled(format!("Erreur : {e}"), Style::new().fg(t.error))
            }
            _ => Line::styled(
                format!(
                    "{} Chargement de {}…",
                    spinner(app.spinner),
                    obj.qualified()
                ),
                t.dim(),
            ),
        };
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), area);
        return;
    }

    let input_line = matches!(
        app.overlay,
        Some(Overlay::DataSearch { .. } | Overlay::FreeWhere(_))
    );
    let [summary_area, grid_area, input_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(u16::from(input_line)),
    ])
    .areas(area);

    // Summary.
    let d = &app.data;
    let mut first = vec![
        Span::styled(
            obj.qualified(),
            Style::new()
                .fg(t.kind_color(obj.kind))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", obj.kind.label()), t.dim()),
    ];
    if let Some(pos) = app.data_position() {
        first.push(Span::styled(format!("  {pos}"), Style::new().fg(t.fg)));
    }
    if d.loading.is_some() {
        first.push(Span::styled(
            format!("  {} chargement…", spinner(app.spinner)),
            Style::new().fg(t.info),
        ));
    } else if let Some(e) = d.last_elapsed {
        first.push(Span::styled(format!("  {}", fmt_duration(e)), t.dim()));
    }
    if d.search.is_some() {
        first.push(Span::styled(
            format!("  recherche : {} occurrence(s)", d.match_count()),
            Style::new().fg(t.matched),
        ));
    }
    let mut second: Vec<Span> = Vec::new();
    let query = d.describe_query();
    if !query.is_empty() {
        second.push(Span::styled(query.join(" · "), Style::new().fg(t.accent)));
        second.push(Span::raw("   "));
    }
    if let Some(reason) = &d.read_only_reason {
        second.push(Span::styled(reason.clone(), Style::new().fg(t.warning)));
    } else {
        second.push(Span::styled(
            format!("clé : {}", d.key_columns.join(", ")),
            t.dim(),
        ));
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(first), Line::from(second)]),
        summary_area,
    );

    if let Some(err) = &d.error {
        let text = vec![
            Line::styled(format!("Erreur : {err}"), Style::new().fg(t.error)),
            Line::styled(
                "R relance la requête ; F / f / C modifient ou effacent les filtres.",
                t.dim(),
            ),
        ];
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), grid_area);
    } else {
        let focused = app.focus == Focus::Center;
        let mut grid = app.data.grid;
        let data = &app.data;
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
        table_view::draw(frame, grid_area, data, &mut grid, &t, &opts);
        app.data.grid = grid;
    }

    // Input line.
    let (prefix, input) = match &app.overlay {
        Some(Overlay::DataSearch { input, .. }) => ("/", input),
        Some(Overlay::FreeWhere(input)) => ("WHERE ", input),
        _ => return,
    };
    let line = Line::from(vec![
        Span::styled(
            prefix,
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(input.text().to_string(), Style::new().fg(t.fg)),
    ]);
    frame.render_widget(line, input_area);
    let x = input_area.x + prefix.len() as u16 + input.cursor_width() as u16;
    frame.set_cursor_position((x.min(input_area.right().saturating_sub(1)), input_area.y));
}
