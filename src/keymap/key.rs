//! Key chords: parsing from config strings, normalisation of terminal events
//! and display.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One key press with its modifiers, normalised so that config strings and
/// terminal events compare equal (`G` == `shift+g`, BackTab == `shift+tab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const MOD_MASK: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT);

impl KeyChord {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & MOD_MASK;
        let code = match code {
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            KeyCode::Char(c) => {
                if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    // ctrl+P / ctrl+shift+p -> ctrl+shift+p
                    if c.is_uppercase() {
                        mods |= KeyModifiers::SHIFT;
                    }
                    let lower = c.to_lowercase().next().unwrap_or(c);
                    if mods.contains(KeyModifiers::SHIFT) && !c.is_alphabetic() {
                        mods -= KeyModifiers::SHIFT;
                    }
                    KeyCode::Char(lower)
                } else {
                    // The character already carries the shift state.
                    let shifted = mods.contains(KeyModifiers::SHIFT);
                    mods -= KeyModifiers::SHIFT;
                    if shifted && c.is_lowercase() {
                        KeyCode::Char(c.to_uppercase().next().unwrap_or(c))
                    } else {
                        KeyCode::Char(c)
                    }
                }
            }
            other => other,
        };
        Self { code, mods }
    }

    pub fn from_event(ev: &KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers)
    }

    pub fn plain(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Parses a sequence: `"ctrl+p"`, `"gg"`, `"ctrl+w h"`, `"shift+tab"`.
    pub fn parse_seq(s: &str) -> Result<Vec<KeyChord>, String> {
        let mut seq = Vec::new();
        for token in s.split_whitespace() {
            if token.contains('+') && token.len() > 1 {
                seq.push(parse_chord(token)?);
            } else if let Some(code) = named_key(token) {
                seq.push(KeyChord::new(code, KeyModifiers::NONE));
            } else {
                // `gg`, `dd`, `zM`: one chord per character.
                seq.extend(token.chars().map(KeyChord::plain));
            }
        }
        if seq.is_empty() {
            return Err("empty key".into());
        }
        Ok(seq)
    }

    pub fn display(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("Ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("Alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push_str("Shift+");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Delete => "Del".into(),
            KeyCode::Insert => "Ins".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        out.push_str(&key);
        out
    }

    fn is_plain_char(&self) -> bool {
        matches!(self.code, KeyCode::Char(c) if c != ' ') && self.mods.is_empty()
    }
}

/// `gg`, `Ctrl+w h`.
pub fn display_seq(seq: &[KeyChord]) -> String {
    if seq.iter().all(KeyChord::is_plain_char) {
        return seq.iter().map(KeyChord::display).collect();
    }
    seq.iter()
        .map(KeyChord::display)
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_chord(token: &str) -> Result<KeyChord, String> {
    // A trailing `+` is the plus key itself (`ctrl++`).
    let (mods_part, key) = if let Some(stripped) = token.strip_suffix("++") {
        (stripped, "+")
    } else {
        token
            .rsplit_once('+')
            .ok_or_else(|| format!("invalid key `{token}`"))?
    };
    let mut mods = KeyModifiers::NONE;
    for m in mods_part.split('+') {
        mods |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
            "alt" | "meta" | "m" | "a" => KeyModifiers::ALT,
            "shift" | "s" => KeyModifiers::SHIFT,
            other => return Err(format!("unknown modifier `{other}` in `{token}`")),
        };
    }
    let code = named_key(key)
        .or_else(|| {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            }
        })
        .ok_or_else(|| format!("unknown key `{key}` in `{token}`"))?;
    Ok(KeyChord::new(code, mods))
}

fn named_key(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "plus" => KeyCode::Char('+'),
        f if f.len() >= 2 && f.starts_with('f') => {
            let n: u8 = f[1..].parse().ok()?;
            if (1..=24).contains(&n) {
                KeyCode::F(n)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        KeyChord::new(code, mods)
    }

    #[test]
    fn parse_simple_and_named() {
        assert_eq!(
            KeyChord::parse_seq("q").unwrap(),
            vec![KeyChord::plain('q')]
        );
        assert_eq!(
            KeyChord::parse_seq("enter").unwrap(),
            vec![k(KeyCode::Enter, KeyModifiers::NONE)]
        );
        assert_eq!(
            KeyChord::parse_seq("F5").unwrap(),
            vec![k(KeyCode::F(5), KeyModifiers::NONE)]
        );
        assert_eq!(
            KeyChord::parse_seq("space").unwrap(),
            vec![KeyChord::plain(' ')]
        );
    }

    #[test]
    fn parse_modifiers() {
        assert_eq!(
            KeyChord::parse_seq("ctrl+p").unwrap(),
            vec![k(KeyCode::Char('p'), KeyModifiers::CONTROL)]
        );
        assert_eq!(
            KeyChord::parse_seq("Ctrl+Shift+Enter").unwrap(),
            vec![k(
                KeyCode::Enter,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )]
        );
        assert_eq!(
            KeyChord::parse_seq("ctrl++").unwrap(),
            vec![k(KeyCode::Char('+'), KeyModifiers::CONTROL)]
        );
        assert!(KeyChord::parse_seq("hyper+x").is_err());
        assert!(KeyChord::parse_seq("ctrl+nope").is_err());
    }

    #[test]
    fn parse_sequences() {
        assert_eq!(
            KeyChord::parse_seq("gg").unwrap(),
            vec![KeyChord::plain('g'); 2]
        );
        assert_eq!(
            KeyChord::parse_seq("ctrl+w h").unwrap(),
            vec![
                k(KeyCode::Char('w'), KeyModifiers::CONTROL),
                KeyChord::plain('h')
            ]
        );
        // A named key wins over splitting into characters.
        assert_eq!(KeyChord::parse_seq("end").unwrap().len(), 1);
    }

    #[test]
    fn shift_normalisation() {
        // Terminals report `G` either as 'G' or as 'G' + SHIFT.
        assert_eq!(
            k(KeyCode::Char('G'), KeyModifiers::SHIFT),
            KeyChord::plain('G')
        );
        assert_eq!(
            KeyChord::parse_seq("shift+g").unwrap(),
            vec![KeyChord::plain('G')]
        );
        assert_eq!(
            k(KeyCode::BackTab, KeyModifiers::SHIFT),
            KeyChord::parse_seq("shift+tab").unwrap()[0]
        );
        assert_eq!(
            k(KeyCode::Char('P'), KeyModifiers::CONTROL),
            KeyChord::parse_seq("ctrl+shift+p").unwrap()[0]
        );
        // `?` typed with shift is just `?`.
        assert_eq!(
            k(KeyCode::Char('?'), KeyModifiers::SHIFT),
            KeyChord::plain('?')
        );
    }

    #[test]
    fn display() {
        assert_eq!(display_seq(&KeyChord::parse_seq("gg").unwrap()), "gg");
        assert_eq!(
            display_seq(&KeyChord::parse_seq("ctrl+w h").unwrap()),
            "Ctrl+w h"
        );
        assert_eq!(
            display_seq(&KeyChord::parse_seq("shift+tab").unwrap()),
            "Shift+Tab"
        );
    }
}
