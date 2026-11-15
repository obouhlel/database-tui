//! Small overlays: command line, password prompt, error popup.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::centered;
use crate::app::App;
use crate::app::input::LineInput;
use crate::config::ConnectionConfig;

pub fn draw_command_line(frame: &mut Frame, app: &App, input: &LineInput, area: Rect) {
    let t = &app.theme;
    let line = Line::from(vec![
        Span::styled(":", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(input.text().to_string(), Style::new().fg(t.fg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    let x = area.x + 1 + input.cursor_width() as u16;
    frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
}

pub fn draw_password(
    frame: &mut Frame,
    app: &App,
    conn: &ConnectionConfig,
    input: &LineInput,
    error: Option<&str>,
    area: Rect,
) {
    let t = &app.theme;
    let rect = centered(area, 70, 8);
    let block = Block::bordered()
        .title(Span::styled(
            format!(" Mot de passe — {} ", conn.name),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" Entrée valider · Esc annuler ", t.dim()))
        .border_style(t.border(true))
        .style(t.base());
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let masked = "*".repeat(input.text().chars().count());
    let mut lines = vec![
        Line::styled(conn.target(), t.dim()),
        Line::default(),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(t.accent)),
            Span::raw(masked),
        ]),
    ];
    if let Some(e) = error {
        lines.push(Line::default());
        lines.push(Line::styled(e.to_string(), Style::new().fg(t.error)));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    let x = inner.x + 2 + input.text().chars().count() as u16;
    frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y + 2));
}

pub fn draw_error(frame: &mut Frame, app: &App, title: &str, message: &str, area: Rect) {
    let t = &app.theme;
    let width = 80.min(area.width);
    // Rough wrapped height: enough for the message plus borders.
    let inner_w = width.saturating_sub(2).max(1) as usize;
    let lines: usize = message
        .lines()
        .map(|l| l.chars().count().div_ceil(inner_w).max(1))
        .sum();
    let rect = centered(
        area,
        width,
        (lines as u16).saturating_add(3).min(area.height),
    );
    let block = Block::bordered()
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(t.error).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" Entrée / Esc fermer ", t.dim()))
        .border_style(Style::new().fg(t.error))
        .style(t.base());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub fn draw_login(
    frame: &mut Frame,
    app: &App,
    form: &crate::app::discovery::LoginForm,
    area: Rect,
) {
    use crate::app::discovery::LOGIN_FIELDS;
    let t = &app.theme;
    let rect = centered(area, 70, 11);
    let block = Block::bordered()
        .title(Span::styled(
            format!(" Connexion — {} ", form.server.label()),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " Tab champ suivant · Entrée se connecter · Esc annuler ",
            t.dim(),
        ))
        .border_style(t.border(true))
        .style(t.base());
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let mut lines = vec![
        Line::styled(
            "Toutes les bases accessibles seront listées dans l'arbre.",
            t.dim(),
        ),
        Line::default(),
    ];
    let label_w = LOGIN_FIELDS
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for (i, label) in LOGIN_FIELDS.iter().enumerate() {
        let active = i == form.field;
        let text = if i == 1 {
            "*".repeat(form.fields[i].text().chars().count())
        } else {
            form.fields[i].text().to_string()
        };
        let value_style = if active {
            Style::new().fg(t.fg).bg(t.selection_inactive_bg)
        } else {
            Style::new().fg(t.fg)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:<label_w$}"),
                if active {
                    Style::new().fg(t.accent).add_modifier(Modifier::BOLD)
                } else {
                    t.dim()
                },
            ),
            Span::styled(format!("{text:<40}"), value_style),
        ]));
    }
    if let Some(e) = &form.error {
        lines.push(Line::default());
        lines.push(Line::styled(e.clone(), Style::new().fg(t.error)));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    let field = &form.fields[form.field];
    let x = inner.x + label_w as u16 + field.cursor_width() as u16;
    frame.set_cursor_position((
        x.min(inner.right().saturating_sub(1)),
        inner.y + 2 + form.field as u16,
    ));
}

pub fn draw_save_server(frame: &mut Frame, app: &App, conn: &ConnectionConfig, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 70, 7);
    let block = Block::bordered()
        .title(Span::styled(
            " Enregistrer ce serveur ? ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" o / Entrée : oui · n / Esc : non ", t.dim()))
        .border_style(t.border(true))
        .style(t.base());
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::from(vec![
            Span::styled("Ajouter « ", Style::new().fg(t.fg)),
            Span::styled(
                conn.name.clone(),
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" » ({}) à config.toml ?", conn.target()),
                Style::new().fg(t.fg),
            ),
        ]),
        Line::default(),
        Line::styled(
            "Le mot de passe n'est pas enregistré : il sera redemandé, ou lu depuis ~/.pgpass.",
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
