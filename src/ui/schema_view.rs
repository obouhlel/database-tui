//! Schema tab: header (owner, comment, size), section bar, then the current
//! section as a table or as highlighted DDL.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{spinner, sql_view, table_view};
use crate::app::schema_view::Section;
use crate::app::{App, DetailsState, Focus, format};
use crate::db::catalog::ObjectRef;

pub fn draw(frame: &mut Frame, app: &mut App, obj: &ObjectRef, area: Rect) {
    let t = app.theme.clone();
    let state = app.current_details().cloned();
    let showing = app.schema.object.as_ref() == Some(obj) && app.schema.details.is_some();

    if !showing {
        let text = match &state {
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
    let Some(details) = app.schema.details.clone() else {
        return;
    };

    // Header.
    let mut header = vec![Line::from(vec![
        Span::styled(
            obj.qualified()
                + &obj
                    .signature
                    .as_ref()
                    .map(|s| format!("({s})"))
                    .unwrap_or_default(),
            Style::new()
                .fg(t.kind_color(obj.kind))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", obj.kind.label()), t.dim()),
        Span::styled(
            details
                .owner
                .as_ref()
                .map(|o| format!(" · propriétaire {o}"))
                .unwrap_or_default(),
            t.dim(),
        ),
        Span::styled(
            match &state {
                Some(DetailsState::Loading) => format!("  {} actualisation…", spinner(app.spinner)),
                Some(DetailsState::Failed(e)) => format!("  erreur : {e}"),
                _ => String::new(),
            },
            Style::new().fg(t.warning),
        ),
    ])];
    if let Some(s) = &details.stats {
        let rows = s.estimated_rows.map_or_else(
            || "lignes : inconnu (jamais analysée)".to_string(),
            |n| format!("{} lignes (estimation)", format::count(n)),
        );
        header.push(Line::styled(
            format!(
                "{rows} · {} (données {}, index {}, TOAST {})",
                format::bytes(s.total_bytes),
                format::bytes(s.table_bytes),
                format::bytes(s.index_bytes),
                format::bytes(s.toast_bytes)
            ),
            Style::new().fg(t.fg),
        ));
    }
    if let Some(c) = &details.comment {
        header.push(Line::styled(
            c.clone(),
            Style::new().fg(t.fg).add_modifier(Modifier::ITALIC),
        ));
    }

    let [header_area, bar_area, body_area] = Layout::vertical([
        Constraint::Length(header.len() as u16),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(header), header_area);

    // Section bar.
    let mut spans = Vec::new();
    for (i, s) in app.schema.sections.iter().enumerate() {
        let label = match app.schema.count(*s) {
            Some(n) => format!(" {} {n} ", s.title()),
            None => format!(" {} ", s.title()),
        };
        let style = if i == app.schema.section {
            Style::new()
                .fg(t.tab_active_fg)
                .bg(t.accent)
                .add_modifier(Modifier::BOLD)
        } else if app.schema.count(*s) == Some(0) {
            Style::new().fg(t.dim)
        } else {
            Style::new().fg(t.fg)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    let bar_hint = app.keymap.hint(
        crate::keymap::Context::Schema,
        crate::keymap::Action::PrevSection,
    );
    let next_hint = app.keymap.hint(
        crate::keymap::Context::Schema,
        crate::keymap::Action::NextSection,
    );
    if let (Some(p), Some(n)) = (bar_hint, next_hint) {
        spans.push(Span::styled(format!(" {p} {n}"), t.dim()));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            height: 1,
            ..bar_area
        },
    );

    let focused = app.focus == Focus::Center;
    match app.schema.current_section() {
        Some(Section::Ddl) => {
            app.schema.ddl.viewport = body_area.height as usize;
            let max = app
                .schema
                .ddl_lines
                .saturating_sub(body_area.height as usize);
            app.schema.ddl.scroll = app.schema.ddl.scroll.min(max);
            sql_view::draw(
                frame,
                body_area,
                &details.ddl,
                app.schema.ddl.scroll,
                app.schema.ddl.hscroll,
                &t,
            );
        }
        Some(_) => {
            if let Some(table) = app.schema.table_mut() {
                table_view::draw(frame, body_area, &table.grid, &mut table.state, &t, focused);
            }
        }
        None => {}
    }
}
