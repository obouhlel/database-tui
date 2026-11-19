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

pub fn draw_confirm_delete(frame: &mut Frame, app: &App, name: &str, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 64, 7);
    let saved = app.server(name).is_some_and(|s| s.saved);
    let block = Block::bordered()
        .title(Span::styled(
            " Supprimer la connexion ? ",
            Style::new().fg(t.error).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" o : supprimer · n / Esc : annuler ", t.dim()))
        .border_style(Style::new().fg(t.error))
        .style(t.base());
    frame.render_widget(Clear, rect);
    let detail = if saved {
        "Elle sera retirée de la liste et de config.toml."
    } else {
        "Elle sera retirée de la liste (elle n'est pas dans config.toml)."
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Supprimer « ", Style::new().fg(t.fg)),
            Span::styled(
                name.to_string(),
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" » ?", Style::new().fg(t.fg)),
        ]),
        Line::default(),
        Line::styled(detail, t.dim()),
        Line::styled("Rien n'est modifié sur le serveur.", t.dim()),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// Suggestions of the command palette, at the bottom of `area`.
pub fn draw_palette(frame: &mut Frame, app: &App, input: &LineInput, area: Rect) {
    let t = &app.theme;
    let matches = app.palette_matches(input.text());
    if matches.is_empty() {
        return;
    }
    const MAX: usize = 10;
    let n = matches.len().min(MAX);
    let height = n as u16 + 2;
    let width = area.width.min(100);
    let rect = Rect {
        x: area.x,
        y: area.bottom().saturating_sub(height),
        width,
        height: height.min(area.height),
    };
    let cursor = app.palette_cursor.map(|c| c.min(matches.len() - 1));
    let offset = cursor.unwrap_or(0).saturating_sub(MAX - 1);
    let label_w = matches
        .iter()
        .map(|m| m.label.len())
        .max()
        .unwrap_or(4)
        .min(28);
    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(offset)
        .take(MAX)
        .map(|(i, m)| {
            let selected = Some(i) == cursor;
            let base = if selected {
                Style::new().bg(t.selection_bg).fg(t.selection_fg)
            } else {
                Style::new().fg(t.fg)
            };
            let mut spans = vec![
                Span::styled(
                    format!(" {:<label_w$}  ", m.label),
                    base.add_modifier(Modifier::BOLD),
                ),
                Span::styled(m.description.clone(), if selected { base } else { t.dim() }),
            ];
            if !m.keys.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", m.keys),
                    Style::new().fg(t.accent),
                ));
            }
            Line::from(spans)
        })
        .collect();
    let block = Block::bordered()
        .title(Span::styled(
            " Commandes et actions ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " Up/Down choisir · Tab compléter · Entrée exécuter ",
            t.dim(),
        ))
        .border_style(t.border(true))
        .style(t.base());
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Connection tests: one line per connection, then the error of the
/// selected one.
pub fn draw_probes(
    frame: &mut Frame,
    app: &App,
    v: &crate::app::config_edit::ProbeView,
    area: Rect,
) {
    let t = &app.theme;
    let name_w = v
        .rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let target_w = v
        .rows
        .iter()
        .map(|r| r.target.chars().count())
        .max()
        .unwrap_or(0);
    let width = 110.min(area.width);
    let selected_error = v
        .rows
        .get(v.cursor)
        .and_then(|r| r.result.as_ref())
        .and_then(|r| r.as_ref().err())
        // The first line is already on the row: the detail only when there
        // is more, or when it is too long for the row.
        .filter(|e| {
            let room = (width as usize).saturating_sub(2 + 3 + name_w + 2 + target_w + 2 + 1);
            e.lines().count() > 1 || e.chars().count() > room
        });
    let error_lines = selected_error.map_or(0, |e| {
        let w = width.saturating_sub(4).max(1) as usize;
        e.lines()
            .map(|l| l.chars().count().div_ceil(w).max(1))
            .sum::<usize>()
            + 1
    });
    let height = (v.rows.len() + error_lines + 2) as u16 + 2;
    let rect = centered(area, width, height.min(area.height));
    let border = if !v.done() {
        t.accent
    } else if v.failed() > 0 {
        t.error
    } else {
        t.success
    };
    let block = Block::bordered()
        .title(Span::styled(
            format!(" {} ", v.title),
            Style::new().fg(border).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " j/k choisir · Entrée ouvrir · e modifier · a ajouter · r retester · Esc fermer ",
            t.dim(),
        ))
        .border_style(Style::new().fg(border))
        .style(t.base());
    let mut lines = Vec::new();
    for (i, r) in v.rows.iter().enumerate() {
        let (status, style, detail) = match &r.result {
            None => (
                format!("{} ", super::spinner(app.spinner)),
                Style::new().fg(t.info),
                "test en cours…".to_string(),
            ),
            Some(Ok(ok)) => (
                "✓ ".to_string(),
                Style::new().fg(t.success),
                format!(
                    "PostgreSQL {}{} · {} ms",
                    ok.version,
                    if ok.tls { " · TLS" } else { "" },
                    ok.elapsed.as_millis()
                ),
            ),
            Some(Err(e)) => (
                "✗ ".to_string(),
                Style::new().fg(t.error),
                e.lines().next().unwrap_or("").to_string(),
            ),
        };
        // One line per connection: the detail is cut to the room left
        // (the whole error is shown below for the selected row).
        let room = (width as usize).saturating_sub(2 + 3 + name_w + 2 + target_w + 2 + 1);
        let detail = super::table_view::fit(&detail, room).trim_end().to_string();
        let mut name = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
        if i == v.cursor {
            name = name.bg(t.selection_bg).fg(t.selection_fg);
        }
        lines.push(Line::from(vec![
            Span::styled(format!(" {status}"), style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:name_w$}", r.name), name),
            Span::styled(format!("  {:target_w$}  ", r.target), t.dim()),
            Span::styled(detail, style),
        ]));
    }
    if let Some(e) = selected_error {
        lines.push(Line::default());
        for l in e.lines() {
            lines.push(Line::styled(format!(" {l}"), Style::new().fg(t.error)));
        }
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// `config.toml` does not parse after editing.
pub fn draw_config_invalid(frame: &mut Frame, app: &App, message: &str, area: Rect) {
    let t = &app.theme;
    let text = format!(
        "{message}\n\nLe fichier est gardé tel quel ; les connexions actuelles restent en place."
    );
    let width = 90.min(area.width);
    let inner_w = width.saturating_sub(2).max(1) as usize;
    let lines: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(inner_w).max(1))
        .sum();
    let rect = centered(area, width, (lines as u16 + 2).min(area.height));
    let block = Block::bordered()
        .title(Span::styled(
            " config.toml invalide ",
            Style::new().fg(t.error).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " e / Entrée corriger (sur la ligne de l'erreur) · Esc garder les connexions actuelles ",
            t.dim(),
        ))
        .border_style(Style::new().fg(t.error))
        .style(t.base());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        rect,
    );
}
