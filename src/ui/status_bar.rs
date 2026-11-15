//! Bottom lines: status bar and contextual key hints.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{hint_spans, line_width, spinner};
use crate::app::{App, Focus, Level, Mode, Screen};
use crate::keymap::{Action, Context};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let bar = Style::new().bg(t.status_bg).fg(t.status_fg);
    frame.buffer_mut().set_style(area, bar);

    let mut left: Vec<Span> = Vec::new();
    match &app.session {
        Some(s) if app.screen == Screen::Main => {
            let color = s.conn.parsed_color().unwrap_or(t.accent);
            left.push(Span::styled(
                format!(" {} ", s.conn.name),
                Style::new()
                    .bg(color)
                    .fg(contrast(color))
                    .add_modifier(Modifier::BOLD),
            ));
            let db = app.current_database().unwrap_or(&s.info.database);
            left.push(Span::raw(format!(" {}@{db} ", s.info.user)));
            if let Some(schema) = app.current_schema() {
                left.push(sep(app));
                left.push(Span::raw(format!(" {schema} ")));
            }
            left.push(sep(app));
            left.push(Span::raw(format!(
                " {} ",
                app.data_position().unwrap_or_else(|| "—".into())
            )));
            if let Some(op) = &app.last_op {
                left.push(sep(app));
                left.push(Span::raw(format!(
                    " {} ({}) ",
                    fmt_duration(op.elapsed),
                    op.label
                )));
            }
            if s.conn.tunnel_cmd.is_some() {
                left.push(badge(" TUNNEL ", t.info));
            }
            if s.conn.read_only {
                left.push(badge(" RO ", t.warning));
            }
            if s.conn.is_production() {
                left.push(badge(" PROD ", t.error));
            }
            if s.lost.is_some() {
                left.push(badge(" DÉCONNECTÉ ", t.error));
            }
        }
        _ => left.push(Span::styled(
            " pgtui ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        )),
    }
    let mode = app.mode();
    let mode_color = match mode {
        Mode::Normal => t.accent,
        Mode::Filter => t.warning,
        Mode::Command => t.success,
        Mode::Search => t.matched,
        Mode::Visual | Mode::VisualLine => t.schema,
    };
    left.insert(0, badge(&format!(" {} ", mode.label()), mode_color));

    // The left part has priority; the message on the right gets what is left.
    let left = Line::from(left);
    let right = right_segment(app);
    let right_w = line_width(&right).min(area.width.saturating_sub(line_width(&left) + 1));
    let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_w)]).areas(area);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right).right_aligned(), r);
}

fn right_segment(app: &App) -> Line<'static> {
    let t = &app.theme;
    if let Some(keys) = app.pending_keys() {
        return Line::styled(
            format!(" {keys}… "),
            Style::new().fg(t.warning).add_modifier(Modifier::BOLD),
        );
    }
    if let Some(busy) = &app.busy {
        return Line::styled(
            format!(
                " {} {} {} ",
                spinner(app.spinner),
                busy.label,
                fmt_duration(busy.since.elapsed())
            ),
            Style::new().fg(t.info),
        );
    }
    if let Some(msg) = &app.status {
        let color = match msg.level {
            Level::Info => t.info,
            Level::Success => t.success,
            Level::Warning => t.warning,
            Level::Error => t.error,
        };
        return Line::styled(format!(" {} ", msg.text), Style::new().fg(color));
    }
    Line::default()
}

pub fn draw_hints(frame: &mut Frame, app: &App, area: Rect) {
    let km = &app.keymap;
    let hint = |ctx: Context, a: Action| km.hint(ctx, a).unwrap_or_else(|| "?".into());
    let items: Vec<(String, &str)> = if app.tree.filter_editing {
        vec![
            ("Enter".into(), "valider"),
            ("Esc".into(), "effacer"),
            ("Up/Down".into(), "naviguer"),
        ]
    } else {
        match (app.screen, app.focus) {
            (Screen::Picker, _) => vec![
                (hint(Context::Picker, Action::Connect), "se connecter"),
                (
                    format!(
                        "{}/{}",
                        hint(Context::Picker, Action::Down),
                        hint(Context::Picker, Action::Up)
                    ),
                    "naviguer",
                ),
                (hint(Context::Global, Action::CommandPalette), "commande"),
                (hint(Context::Global, Action::Help), "aide"),
                (hint(Context::Global, Action::Quit), "quitter"),
            ],
            (Screen::Main, Focus::Tree) => vec![
                (hint(Context::Tree, Action::Open), "ouvrir"),
                (
                    format!(
                        "{}/{}",
                        hint(Context::Tree, Action::Collapse),
                        hint(Context::Tree, Action::Expand)
                    ),
                    "replier/déplier",
                ),
                (hint(Context::Tree, Action::Filter), "filtrer"),
                ("1/2/3".into(), "onglets"),
                (hint(Context::Global, Action::FocusCenter), "panneau"),
                (hint(Context::Tree, Action::ReloadCatalog), "recharger"),
                (hint(Context::Global, Action::CommandPalette), "commande"),
                (hint(Context::Global, Action::Help), "aide"),
                (hint(Context::Global, Action::Quit), "quitter"),
            ],
            (Screen::Main, Focus::Center)
                if app.tab == crate::app::Tab::Data && app.data.visual.is_some() =>
            {
                vec![
                    (hint(Context::Data, Action::Copy), "copier (TSV)"),
                    (hint(Context::Data, Action::CopyAs), "copier comme…"),
                    (hint(Context::Data, Action::Cancel), "annuler la sélection"),
                ]
            }
            (Screen::Main, Focus::Center) if app.tab == crate::app::Tab::Data => vec![
                (hint(Context::Data, Action::Detail), "détail"),
                (hint(Context::Data, Action::Search), "chercher"),
                (hint(Context::Data, Action::FilterEditor), "filtrer"),
                (hint(Context::Data, Action::FreeWhere), "WHERE"),
                (hint(Context::Data, Action::Sort), "trier"),
                (hint(Context::Data, Action::Copy), "copier"),
                (hint(Context::Data, Action::Visual), "sélection"),
                (hint(Context::Data, Action::HideColumn), "masquer col."),
                (hint(Context::Global, Action::Help), "aide"),
                (hint(Context::Global, Action::Quit), "quitter"),
            ],
            (Screen::Main, Focus::Center) if app.tab == crate::app::Tab::Schema => vec![
                (
                    format!(
                        "{}/{}",
                        hint(Context::Schema, Action::PrevSection),
                        hint(Context::Schema, Action::NextSection)
                    ),
                    "sections",
                ),
                (hint(Context::Schema, Action::FollowLink), "table liée"),
                (hint(Context::Schema, Action::JumpBack), "retour"),
                (hint(Context::Schema, Action::Copy), "copier"),
                (
                    format!(
                        "{}/{}",
                        hint(Context::Schema, Action::ScrollLeft),
                        hint(Context::Schema, Action::ScrollRight)
                    ),
                    "défiler",
                ),
                (hint(Context::Schema, Action::Refresh), "recharger"),
                (hint(Context::Global, Action::FocusTree), "arbre"),
                (hint(Context::Global, Action::Help), "aide"),
                (hint(Context::Global, Action::Quit), "quitter"),
            ],
            (Screen::Main, Focus::Center) => vec![
                (hint(Context::Global, Action::NextTab), "onglet suivant"),
                ("1/2/3".into(), "onglets"),
                (hint(Context::Global, Action::FocusTree), "arbre"),
                (hint(Context::Global, Action::CommandPalette), "commande"),
                (hint(Context::Global, Action::Help), "aide"),
                (hint(Context::Global, Action::Quit), "quitter"),
            ],
        }
    };
    frame.render_widget(Paragraph::new(Line::from(hint_spans(app, &items))), area);
}

fn sep(app: &App) -> Span<'static> {
    Span::styled("│", Style::new().fg(app.theme.dim))
}

fn badge(text: &str, color: Color) -> Span<'static> {
    Span::styled(
        text.to_string(),
        Style::new()
            .bg(color)
            .fg(contrast(color))
            .add_modifier(Modifier::BOLD),
    )
}

/// Black or white text, whichever reads better on `bg`.
fn contrast(bg: Color) -> Color {
    let (r, g, b) = match bg {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black | Color::Red | Color::Blue | Color::Magenta | Color::DarkGray => {
            return Color::White;
        }
        Color::Indexed(i) if i < 8 && i != 3 && i != 7 => return Color::White,
        _ => return Color::Black,
    };
    let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    if luma > 140.0 {
        Color::Black
    } else {
        Color::White
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1.0 {
        format!("{:.0} µs", d.as_secs_f64() * 1e6)
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 60_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        let s = d.as_secs();
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_micros(250)), "250 µs");
        assert_eq!(fmt_duration(Duration::from_millis(42)), "42 ms");
        assert_eq!(fmt_duration(Duration::from_millis(1500)), "1.50 s");
        assert_eq!(fmt_duration(Duration::from_secs(125)), "2m05s");
    }

    #[test]
    fn contrast_colors() {
        assert_eq!(contrast(Color::Rgb(255, 255, 255)), Color::Black);
        assert_eq!(contrast(Color::Rgb(10, 10, 10)), Color::White);
        assert_eq!(contrast(Color::Red), Color::White);
        assert_eq!(contrast(Color::Green), Color::Black);
    }
}
