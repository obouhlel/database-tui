//! `?` overlay: every binding, generated from the live keymap so that user
//! overrides are reflected.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use super::centered;
use crate::app::App;
use crate::keymap::Context;

pub fn draw(frame: &mut Frame, app: &App, scroll: u16, area: Rect) {
    let t = &app.theme;
    let rect = centered(area, 92, area.height.saturating_sub(2));
    let block = Block::bordered()
        .title(Span::styled(
            " Aide — raccourcis ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" j/k défiler · Esc fermer ", t.dim()))
        .border_style(t.border(true))
        .style(t.base());

    let lines = help_lines(app);
    let inner_h = block.inner(rect).height;
    let max_scroll = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(inner_h);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll.min(max_scroll), 0)),
        rect,
    );
}

pub fn help_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let header = Style::new().fg(t.accent).add_modifier(Modifier::BOLD);
    let key_style = Style::new().fg(t.matched);
    let desc_style = Style::new().fg(t.fg);

    // Current context first, then global, then the others.
    let mut order = app.contexts();
    order.extend(
        Context::ALL
            .iter()
            .filter(|c| !order.contains(c))
            .copied()
            .collect::<Vec<_>>(),
    );

    let mut lines = Vec::new();
    for ctx in order {
        let bindings = app.keymap.bindings_of(ctx);
        if bindings.is_empty() {
            continue;
        }
        lines.push(Line::styled(
            format!("{} [{}]", ctx.title(), ctx.name()),
            header,
        ));
        for (action, keys) in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<26} ", keys.join(", ")), key_style),
                Span::styled(action.description().to_string(), desc_style),
                Span::styled(format!("  ({})", action.name()), t.dim()),
            ]));
        }
        lines.push(Line::default());
    }

    lines.push(Line::styled("Saisie de texte (filtre, commande)", header));
    for (k, d) in [
        ("Enter / Esc", "Valider / annuler"),
        ("Ctrl+a / Ctrl+e", "Début / fin de ligne"),
        ("Ctrl+w / Ctrl+u", "Effacer le mot / jusqu'au début"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<26} "), key_style),
            Span::styled(d, desc_style),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled("Commandes (:)", header));
    for (c, d) in App::command_names() {
        lines.push(Line::from(vec![
            Span::styled(format!("  :{c:<25} "), key_style),
            Span::styled(*d, desc_style),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Les raccourcis se surchargent dans config.toml, section [keymap.<contexte>].",
        t.dim(),
    ));
    lines
}
