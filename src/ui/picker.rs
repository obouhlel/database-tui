//! Connection picker: configured connections, then servers discovered on
//! this machine.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::{centered, spinner};
use crate::app::App;
use crate::app::discovery::PickerItem;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let n_conf = app.connections.len() as u16;
    let n_disc = app.discovered.len().max(1) as u16;
    let width = 96.min(area.width);
    // Warnings wrap: count the lines they will take at this width.
    let inner_w = usize::from(width.saturating_sub(2)).max(1);
    let warn_lines: usize = app
        .warnings
        .iter()
        .map(|w| (w.chars().count() + 4).div_ceil(inner_w))
        .sum();
    let warn_h = if app.warnings.is_empty() {
        0
    } else {
        warn_lines as u16 + 2
    };
    let conf_h = n_conf.max(1) + 1;
    let disc_h = n_disc + 2;
    let height = (conf_h + disc_h + 4 + warn_h).min(area.height);
    let rect = centered(area, width, height);

    let block = Block::bordered()
        .title(Span::styled(
            " pgtui — connexions ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .border_style(t.border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let [conf_area, disc_area, footer, warn_area] = Layout::vertical([
        Constraint::Length(conf_h),
        Constraint::Length(disc_h),
        Constraint::Length(2),
        Constraint::Length(warn_h),
    ])
    .areas(inner);

    let header = Style::new().fg(t.accent).add_modifier(Modifier::BOLD);

    // Configured connections.
    frame.render_widget(
        Line::styled("Connexions configurées", header),
        Rect {
            height: 1,
            ..conf_area
        },
    );
    if app.connections.is_empty() {
        frame.render_widget(
            Line::styled(
                "  aucune — ~/.config/pgtui/config.toml ou pgtui --url",
                t.dim(),
            ),
            Rect {
                y: conf_area.y + 1,
                height: 1,
                ..conf_area
            },
        );
    }
    let name_w = app
        .connections
        .iter()
        .map(|(c, _)| c.name.chars().count())
        .chain(app.discovered.iter().map(|s| s.label().chars().count()))
        .max()
        .unwrap_or(0)
        + 2;
    let target_w = app
        .connections
        .iter()
        .map(|(c, _)| c.target().chars().count())
        .max()
        .unwrap_or(0);

    for i in 0..app.picker_len() {
        let (area_y, line) = match app.picker_item(i) {
            Some(PickerItem::Configured(conn)) => {
                let color = conn.parsed_color().unwrap_or(t.accent);
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<name_w$}", conn.name),
                        Style::new().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{:<target_w$}", conn.target()), t.dim()),
                    Span::styled(format!("  ssl:{}", conn.sslmode.as_str()), t.dim()),
                ];
                if conn.read_only {
                    spans.push(Span::styled(
                        "  RO",
                        Style::new().fg(t.warning).add_modifier(Modifier::BOLD),
                    ));
                }
                if conn.is_production() {
                    spans.push(Span::styled(
                        "  PROD",
                        Style::new().fg(t.error).add_modifier(Modifier::BOLD),
                    ));
                }
                (conf_area.y + 1 + i as u16, Line::from(spans))
            }
            Some(PickerItem::Discovered(s)) => {
                let j = i - app.connections.len();
                let spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<name_w$}", s.label()),
                        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(if s.tls { "TLS disponible" } else { "sans TLS" }, t.dim()),
                    Span::styled("   Entrée : utilisateur et mot de passe", t.dim()),
                ];
                (disc_area.y + 1 + j as u16, Line::from(spans))
            }
            None => continue,
        };
        let row = Rect {
            y: area_y,
            height: 1,
            ..inner
        };
        if area_y >= inner.bottom() {
            continue;
        }
        let mut line = line;
        if i == app.picker_cursor {
            frame.buffer_mut().set_style(row, t.selection(true));
            line = line.patch_style(Style::new().bg(t.selection_bg));
        }
        frame.render_widget(line, row);
    }

    // Discovered servers.
    let ports: Vec<String> = app
        .config
        .general
        .discover_ports
        .iter()
        .map(u16::to_string)
        .collect();
    let title = format!(
        "Serveurs détectés sur cette machine (port {})",
        ports.join(", ")
    );
    frame.render_widget(
        Line::styled(title, header),
        Rect {
            height: 1,
            ..disc_area
        },
    );
    if app.discovered.is_empty() {
        let text = if app.discovering {
            Line::styled(
                format!("  {} recherche…", spinner(app.spinner)),
                Style::new().fg(t.info),
            )
        } else if !app.config.general.discover {
            Line::styled("  découverte désactivée (general.discover)", t.dim())
        } else {
            Line::styled("  aucun serveur trouvé — R pour relancer", t.dim())
        };
        frame.render_widget(
            text,
            Rect {
                y: disc_area.y + 1,
                height: 1,
                ..disc_area
            },
        );
    }

    let footer_text = match &app.connecting {
        Some((conn, since)) => Line::styled(
            format!(
                "{} Connexion à {} ({})… {:.1} s",
                spinner(app.spinner),
                conn.name,
                conn.target(),
                since.elapsed().as_secs_f64()
            ),
            Style::new().fg(t.info),
        ),
        None => Line::styled(
            "Entrée pour se connecter · R relancer la détection",
            t.dim(),
        ),
    };
    frame.render_widget(Paragraph::new(vec![Line::default(), footer_text]), footer);

    if !app.warnings.is_empty() {
        let mut lines = vec![
            Line::default(),
            Line::styled("Avertissements :", Style::new().fg(t.warning)),
        ];
        lines.extend(
            app.warnings
                .iter()
                .map(|w| Line::styled(format!("  - {w}"), Style::new().fg(t.warning))),
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), warn_area);
    }
}
