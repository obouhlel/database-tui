//! Central panel with the Schema | Data | SQL tabs. Without an object (or
//! for tabs not implemented yet) it shows the object and session details.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::{data_view, schema_view};
use crate::app::{App, Focus, Tab};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme.clone();
    let focused = app.focus == Focus::Center;

    let mut title = vec![Span::raw(" ")];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, tab.title());
        let style = if *tab == app.tab {
            Style::new()
                .fg(t.tab_active_fg)
                .bg(t.tab_active_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.dim)
        };
        title.push(Span::styled(label, style));
        title.push(Span::raw(" "));
    }
    let block = Block::bordered()
        .title(Line::from(title))
        .border_style(t.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.tab == Tab::Schema
        && let Some(obj) = app.current.clone()
    {
        schema_view::draw(frame, app, &obj, inner);
        return;
    }
    if app.tab == Tab::Data
        && let Some(obj) = app.current.clone()
    {
        data_view::draw(frame, app, &obj, inner);
        return;
    }
    if app.tab == Tab::Sql {
        super::sql_tab::draw(frame, app, inner);
        return;
    }

    let label = Style::new().fg(t.dim);
    let value = Style::new().fg(t.fg);
    let mut lines: Vec<Line> = Vec::new();

    match &app.current {
        Some(obj) => {
            let comment = app
                .catalog(&obj.connection, &obj.database)
                .and_then(|c| c.schemas.iter().find(|s| s.name == obj.schema))
                .and_then(|s| {
                    s.objects.iter().find(|o| {
                        o.name == obj.name && o.kind == obj.kind && o.signature == obj.signature
                    })
                })
                .and_then(|o| o.comment.clone());
            let mut name = obj.qualified();
            if let Some(sig) = &obj.signature {
                name = format!("{name}({sig})");
            }
            lines.push(Line::from(vec![
                Span::styled(
                    name,
                    Style::new()
                        .fg(t.kind_color(obj.kind))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", obj.kind.label()), label),
            ]));
            if let Some(c) = comment {
                lines.push(Line::styled(
                    c,
                    Style::new().fg(t.fg).add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::default());
            let pending = match app.tab {
                Tab::Schema => "",
                Tab::Data if !obj.kind.has_rows() => "Cet objet n'a pas de lignes à afficher.",
                Tab::Data | Tab::Sql => "",
            };
            lines.push(Line::styled(pending, label));
        }
        None => {
            lines.push(Line::styled(
                "Sélectionnez un objet dans l'arbre et appuyez sur Entrée.",
                label,
            ));
        }
    }

    if let Some(s) = app.current_server() {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("Connexion {}", s.conn.name),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
        let mut kv = |k: &str, v: String| {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<12}"), label),
                Span::styled(v, value),
            ]));
        };
        let state = match &s.state {
            crate::app::server::ConnState::Disconnected => {
                "non connecté — Entrée ou l pour se connecter".to_string()
            }
            crate::app::server::ConnState::Connecting(_) => "connexion…".to_string(),
            crate::app::server::ConnState::Connected => "connecté".to_string(),
            crate::app::server::ConnState::Failed(e) => format!("échec : {e}"),
            crate::app::server::ConnState::Lost(e) => {
                format!("perdue : {e} — Entrée pour reconnecter")
            }
        };
        kv("État", state);
        kv("Cible", s.conn.target());
        if let Some(cmd) = &s.conn.tunnel_cmd {
            kv("Tunnel", cmd.clone());
        }
        let Some(info) = &s.info else {
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            return;
        };
        kv("Serveur", format!("PostgreSQL {}", info.version));
        kv("Utilisateur", info.user.clone());
        kv("Base", info.database.clone());
        kv(
            "TLS",
            if info.tls {
                format!("oui ({})", s.conn.sslmode.as_str())
            } else {
                "non".into()
            },
        );
        kv(
            "Accès",
            if s.conn.read_only {
                "lecture seule".into()
            } else {
                "lecture / écriture".into()
            },
        );
        let timeout = s.conn.statement_timeout(&app.config.general);
        kv(
            "Timeout",
            if timeout == 0 {
                "aucun".into()
            } else {
                format!("{timeout} s")
            },
        );
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
