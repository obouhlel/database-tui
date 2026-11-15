//! System clipboard with an OSC 52 fallback.
//!
//! - `system`: native clipboard through `arboard` (X11 / Wayland / macOS / Windows);
//! - `osc52`: escape sequence interpreted by the terminal itself, which works
//!   over SSH (tmux needs `set -g set-clipboard on`);
//! - `auto`: OSC 52 over SSH, otherwise the system clipboard, falling back to
//!   OSC 52 when it is unavailable.

use std::io::Write;

use anyhow::{Result, anyhow};
use base64::Engine;

use crate::config::ClipboardMode;

pub struct Clipboard {
    mode: ClipboardMode,
    /// Kept alive: on X11 the owner must stay around to serve the content.
    system: Option<arboard::Clipboard>,
}

impl Clipboard {
    pub fn new(mode: ClipboardMode) -> Self {
        let over_ssh =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        let system = match mode {
            ClipboardMode::Osc52 => None,
            ClipboardMode::Auto if over_ssh => None,
            _ => match arboard::Clipboard::new() {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::info!("system clipboard unavailable ({e}), using OSC 52");
                    None
                }
            },
        };
        Self { mode, system }
    }

    /// Copies `text`; returns the method used.
    pub fn copy(&mut self, text: &str) -> Result<&'static str> {
        if let Some(c) = &mut self.system {
            match c.set_text(text.to_owned()) {
                Ok(()) => return Ok("presse-papier système"),
                Err(e) if self.mode == ClipboardMode::System => {
                    return Err(anyhow!("presse-papier système : {e}"));
                }
                Err(e) => tracing::warn!("system clipboard failed ({e}), falling back to OSC 52"),
            }
        } else if self.mode == ClipboardMode::System {
            return Err(anyhow!("presse-papier système indisponible"));
        }
        let mut out = std::io::stdout();
        out.write_all(osc52(text).as_bytes())?;
        out.flush()?;
        Ok("OSC 52")
    }
}

pub fn osc52(text: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn osc52_sequence() {
        assert_eq!(super::osc52("héllo"), "\x1b]52;c;aMOpbGxv\x07");
    }
}
