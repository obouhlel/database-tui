//! Colours. Every widget takes its colours from here; nothing is hard-coded.

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};

use crate::config::parse_color;
use crate::db::catalog::ObjectKind;

macro_rules! theme {
    ($($field:ident),* $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Theme { $(pub $field: Color,)* }

        impl Theme {
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field),)*];

            fn field_mut(&mut self, name: &str) -> Option<&mut Color> {
                match name { $(stringify!($field) => Some(&mut self.$field),)* _ => None }
            }
        }
    };
}

theme! {
    bg, fg, dim, accent, border, border_focus,
    selection_bg, selection_fg, selection_inactive_bg,
    tab_active_fg, tab_active_bg,
    status_bg, status_fg,
    error, warning, success, info,
    matched,
    schema, table, view, matview, sequence, function,
    null, prod_banner_bg, prod_banner_fg,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            dim: Color::Rgb(0x6b, 0x72, 0x8e),
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            border_focus: Color::Rgb(0x7a, 0xa2, 0xf7),
            selection_bg: Color::Rgb(0x28, 0x34, 0x57),
            selection_fg: Color::Rgb(0xff, 0xff, 0xff),
            selection_inactive_bg: Color::Rgb(0x23, 0x26, 0x38),
            tab_active_fg: Color::Rgb(0x1a, 0x1b, 0x26),
            tab_active_bg: Color::Rgb(0x7a, 0xa2, 0xf7),
            status_bg: Color::Rgb(0x1f, 0x23, 0x35),
            status_fg: Color::Rgb(0xa9, 0xb1, 0xd6),
            error: Color::Rgb(0xf7, 0x76, 0x8e),
            warning: Color::Rgb(0xe0, 0xaf, 0x68),
            success: Color::Rgb(0x9e, 0xce, 0x6a),
            info: Color::Rgb(0x7d, 0xcf, 0xff),
            matched: Color::Rgb(0xff, 0x9e, 0x64),
            schema: Color::Rgb(0xbb, 0x9a, 0xf7),
            table: Color::Rgb(0x7a, 0xa2, 0xf7),
            view: Color::Rgb(0x73, 0xda, 0xca),
            matview: Color::Rgb(0x2a, 0xc3, 0xde),
            sequence: Color::Rgb(0xe0, 0xaf, 0x68),
            function: Color::Rgb(0x9e, 0xce, 0x6a),
            null: Color::Rgb(0x56, 0x5f, 0x89),
            prod_banner_bg: Color::Rgb(0xc5, 0x3b, 0x53),
            prod_banner_fg: Color::Rgb(0xff, 0xff, 0xff),
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Rgb(0x34, 0x3b, 0x58),
            dim: Color::Rgb(0x84, 0x8c, 0xb5),
            accent: Color::Rgb(0x2e, 0x7d, 0xe9),
            border: Color::Rgb(0xa8, 0xae, 0xcb),
            border_focus: Color::Rgb(0x2e, 0x7d, 0xe9),
            selection_bg: Color::Rgb(0xb7, 0xc1, 0xe3),
            selection_fg: Color::Rgb(0x10, 0x14, 0x24),
            selection_inactive_bg: Color::Rgb(0xd5, 0xd9, 0xe8),
            tab_active_fg: Color::Rgb(0xff, 0xff, 0xff),
            tab_active_bg: Color::Rgb(0x2e, 0x7d, 0xe9),
            status_bg: Color::Rgb(0xd5, 0xd6, 0xdb),
            status_fg: Color::Rgb(0x34, 0x3b, 0x58),
            error: Color::Rgb(0xc6, 0x43, 0x43),
            warning: Color::Rgb(0x8c, 0x6c, 0x3e),
            success: Color::Rgb(0x48, 0x5e, 0x30),
            info: Color::Rgb(0x16, 0x67, 0x75),
            matched: Color::Rgb(0xb1, 0x5c, 0x00),
            schema: Color::Rgb(0x5a, 0x4a, 0x78),
            table: Color::Rgb(0x2e, 0x7d, 0xe9),
            view: Color::Rgb(0x0f, 0x4b, 0x6e),
            matview: Color::Rgb(0x16, 0x67, 0x75),
            sequence: Color::Rgb(0x8c, 0x6c, 0x3e),
            function: Color::Rgb(0x48, 0x5e, 0x30),
            null: Color::Rgb(0x9a, 0xa0, 0xbd),
            prod_banner_bg: Color::Rgb(0xc6, 0x43, 0x43),
            prod_banner_fg: Color::Rgb(0xff, 0xff, 0xff),
        }
    }

    /// Base theme by name, with `[theme]` overrides. Returns warnings for
    /// unknown names or colours.
    pub fn from_config(name: &str, overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut theme = if name == "light" {
            Self::light()
        } else {
            Self::dark()
        };
        let mut warnings = Vec::new();
        for (key, value) in overrides {
            match (theme.field_mut(key), parse_color(value)) {
                (Some(slot), Ok(c)) => *slot = c,
                (None, _) => warnings.push(format!(
                    "[theme] {key}: unknown colour name (known: {})",
                    Self::FIELDS.join(", ")
                )),
                (_, Err(e)) => warnings.push(format!("[theme] {key}: {e}")),
            }
        }
        (theme, warnings)
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        Style::new().fg(self.dim)
    }

    pub fn border(&self, focused: bool) -> Style {
        Style::new().fg(if focused {
            self.border_focus
        } else {
            self.border
        })
    }

    pub fn selection(&self, focused: bool) -> Style {
        if focused {
            Style::new()
                .bg(self.selection_bg)
                .fg(self.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(self.selection_inactive_bg)
        }
    }

    pub fn kind_color(&self, kind: ObjectKind) -> Color {
        match kind {
            ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => {
                self.table
            }
            ObjectKind::View => self.view,
            ObjectKind::MaterializedView => self.matview,
            ObjectKind::Sequence => self.sequence,
            ObjectKind::Function | ObjectKind::Procedure => self.function,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides() {
        let (t, w) = Theme::from_config(
            "light",
            &BTreeMap::from([
                ("accent".to_string(), "#010203".to_string()),
                ("nope".to_string(), "red".to_string()),
                ("error".to_string(), "blurple".to_string()),
            ]),
        );
        assert_eq!(t.accent, Color::Rgb(1, 2, 3));
        assert_eq!(t.fg, Theme::light().fg);
        assert_eq!(w.len(), 2);
    }
}
