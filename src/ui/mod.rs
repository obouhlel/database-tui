//! Rendering. Functions here read the [`App`] state and draw it; the only
//! state they write back is layout feedback (the tree's viewport height),
//! which the reducer needs for page-wise movements.

mod center;
mod data_popups;
mod data_view;
mod edit_popups;
mod help;
mod picker;
mod popup;
mod schema_view;
mod sql_view;
mod status_bar;
pub mod table_view;
pub mod theme;
mod tree;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Overlay, Screen};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(tick: usize) -> &'static str {
    SPINNER[tick % SPINNER.len()]
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);

    let banner = app
        .current_server()
        .is_some_and(|s| s.conn.is_production() && s.is_connected() && app.screen == Screen::Main);
    let [banner_area, body, status_area, hints_area] = Layout::vertical([
        Constraint::Length(u16::from(banner)),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if banner {
        draw_prod_banner(frame, app, banner_area);
    }
    match app.screen {
        Screen::Picker => picker::draw(frame, app, body),
        Screen::Main => {
            let tree_width = (body.width / 4)
                .clamp(24, 44)
                .min(body.width.saturating_sub(20));
            let [tree_area, center_area] =
                Layout::horizontal([Constraint::Length(tree_width), Constraint::Fill(1)])
                    .areas(body);
            tree::draw(frame, app, tree_area);
            center::draw(frame, app, center_area);
        }
    }
    status_bar::draw(frame, app, status_area);

    match &app.overlay {
        Some(Overlay::Command(input)) => popup::draw_command_line(frame, app, input, hints_area),
        _ => status_bar::draw_hints(frame, app, hints_area),
    }
    match &app.overlay {
        Some(Overlay::Help { scroll }) => help::draw(frame, app, *scroll, body),
        Some(Overlay::Password { conn, input, error }) => {
            popup::draw_password(frame, app, conn, input, error.as_deref(), body);
        }
        Some(Overlay::Error { title, message }) => {
            popup::draw_error(frame, app, title, message, body)
        }
        Some(Overlay::FilterEditor(e)) => data_popups::draw_filter_editor(frame, app, e, body),
        Some(Overlay::CopyFormat { cursor }) => {
            data_popups::draw_copy_format(frame, app, *cursor, body)
        }
        Some(Overlay::RowDetail { scroll }) => {
            data_popups::draw_row_detail(frame, app, *scroll, body)
        }
        Some(Overlay::Login(form)) => popup::draw_login(frame, app, form, body),
        Some(Overlay::SaveServer(conn)) => popup::draw_save_server(frame, app, conn, body),
        Some(Overlay::ConfirmDelete(name)) => popup::draw_confirm_delete(frame, app, name, body),
        Some(Overlay::CellEditor(ed)) => {
            if matches!(ed.input, crate::app::edit::EditorInput::Multi(_)) {
                edit_popups::draw_multi_editor(frame, app, ed, body);
            }
        }
        Some(Overlay::Changes(v)) => edit_popups::draw_changes(frame, app, v, body),
        Some(Overlay::ConfirmApply {
            counts,
            production,
            script,
            input,
            ..
        }) => edit_popups::draw_confirm_apply(
            frame,
            app,
            counts,
            *production,
            script.is_some(),
            input,
            body,
        ),
        Some(Overlay::ConfirmDiscard) => edit_popups::draw_confirm_discard(frame, app, body),
        Some(Overlay::PendingChanges(after)) => {
            edit_popups::draw_pending_changes(frame, app, after, body)
        }
        // Drawn inside the Data tab.
        Some(Overlay::DataSearch { .. } | Overlay::FreeWhere(_)) => {}
        Some(Overlay::Command(_)) | None => {}
    }
}

fn draw_prod_banner(frame: &mut Frame, app: &App, area: Rect) {
    let Some(s) = app.current_server() else {
        return;
    };
    let t = &app.theme;
    let mut text = format!(" PRODUCTION  {}  {}", s.conn.name, s.conn.target());
    if s.conn.read_only {
        text.push_str("  lecture seule");
    } else {
        text.push_str("  commit sur confirmation explicite");
    }
    let style = Style::new()
        .bg(t.prod_banner_bg)
        .fg(t.prod_banner_fg)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(Paragraph::new(text).style(style), area);
}

/// A rectangle of at most `width` x `height`, centered in `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// `key label  key label ...` hint spans.
pub fn hint_spans<'a>(app: &App, items: &[(String, &'a str)]) -> Vec<Span<'a>> {
    let t = &app.theme;
    let mut spans = Vec::new();
    for (key, label) in items {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!("{label} "), Style::new().fg(t.dim)));
    }
    spans
}

/// Splits `text` into spans, highlighting the characters at `indices`.
pub fn highlighted<'a>(text: &str, indices: &[usize], base: Style, hl: Style) -> Vec<Span<'a>> {
    if indices.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_hl = false;
    for (i, c) in text.chars().enumerate() {
        let is_hl = indices.binary_search(&i).is_ok();
        if is_hl != current_hl && !current.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut current),
                if current_hl { hl } else { base },
            ));
        }
        current_hl = is_hl;
        current.push(c);
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, if current_hl { hl } else { base }));
    }
    spans
}

pub fn line_width(line: &Line) -> u16 {
    u16::try_from(line.width()).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests;
