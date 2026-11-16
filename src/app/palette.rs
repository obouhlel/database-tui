//! Command palette (`:` / `Ctrl+p`): the command line, with fuzzy
//! suggestions of commands and of the actions of the current context
//! (with their keys). `Up` / `Down` choose, `Tab` completes, `Enter` runs
//! the chosen suggestion, else the typed command.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use super::{App, Effect};
use crate::keymap::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteTarget {
    /// A `:` command (its name; `arg`: it takes an argument).
    Command {
        name: &'static str,
        arg: bool,
    },
    Action(Action),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    pub label: String,
    pub description: String,
    /// Keys of an action, for display.
    pub keys: String,
    pub target: PaletteTarget,
}

/// `(name, takes an argument, description)`
pub const COMMANDS: &[(&str, bool, &str)] = &[
    ("quit", false, "Quitter (:q, :q! sans confirmation)"),
    ("help", false, "Aide complète"),
    ("reload", false, "Recharger les catalogues"),
    (
        "connections",
        false,
        "Page des connexions (serveurs détectés)",
    ),
    ("connect", true, "Ouvrir une connexion dans l'arbre"),
    ("reconnect", false, "Rouvrir la connexion courante"),
    ("disconnect", false, "Fermer la connexion courante"),
    ("save", true, "Enregistrer la requête de l'onglet SQL"),
    ("open", true, "Ouvrir une requête enregistrée"),
    (
        "new",
        false,
        "Nouveau script SQL (les autres sont conservés)",
    ),
    ("close", false, "Fermer le script SQL courant"),
    ("clear", false, "Vider le script SQL courant (annulable)"),
    ("history", false, "Rechercher dans l'historique SQL"),
    ("schema", false, "Onglet Schema"),
    ("data", false, "Onglet Data"),
    ("sql", false, "Onglet SQL"),
];

/// Every word accepted as a command (aliases included).
pub fn is_command(word: &str) -> bool {
    COMMANDS.iter().any(|(n, _, _)| *n == word)
        || matches!(
            word,
            "q" | "q!"
                | "quit!"
                | "qa"
                | "qa!"
                | "h"
                | "refresh"
                | "conn"
                | "c"
                | "w"
                | "e"
                | "enew"
        )
}

impl App {
    /// Suggestions for the text of the command line, best first.
    pub fn palette_matches(&self, query: &str) -> Vec<PaletteEntry> {
        let mut entries: Vec<PaletteEntry> = COMMANDS
            .iter()
            .map(|(name, arg, desc)| PaletteEntry {
                label: if *arg {
                    format!("{name} <nom>")
                } else {
                    name.to_string()
                },
                description: desc.to_string(),
                keys: String::new(),
                target: PaletteTarget::Command { name, arg: *arg },
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for ctx in self.contexts() {
            for (action, keys) in self.keymap.bindings_of(ctx) {
                if seen.insert(action) {
                    entries.push(PaletteEntry {
                        label: action.name().to_string(),
                        description: action.description().to_string(),
                        keys: keys.join(" · "),
                        target: PaletteTarget::Action(action),
                    });
                }
            }
        }
        let q = query.split_whitespace().next().unwrap_or("");
        if q.is_empty() {
            return entries;
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize, PaletteEntry)> = entries
            .into_iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let hay = format!("{} {}", e.label, e.description);
                let s = pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher)?;
                // Commands whose name starts with the query come first.
                let bonus = if e.label.starts_with(q) { 1000 } else { 0 };
                Some((s + bonus, i, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, e)| e).collect()
    }

    /// `Enter` in the palette.
    pub(super) fn run_palette(&mut self, line: &str, cursor: Option<usize>) -> Vec<Effect> {
        let first = line.split_whitespace().next().unwrap_or("");
        if first.is_empty() {
            return Vec::new();
        }
        // Without a chosen suggestion, the text is the command: a typo
        // never runs something else.
        let Some(cursor) = cursor else {
            return self.run_command(line);
        };
        let matches = self.palette_matches(line);
        match matches
            .get(cursor.min(matches.len().saturating_sub(1)))
            .map(|e| e.target.clone())
        {
            Some(PaletteTarget::Command { name, .. }) => {
                let rest: Vec<&str> = line.split_whitespace().skip(1).collect();
                let cmd = if rest.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} {}", rest.join(" "))
                };
                self.run_command(&cmd)
            }
            Some(PaletteTarget::Action(a)) => self.dispatch(a),
            None => self.run_command(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_commands() {
        assert!(is_command("q!") && is_command("w") && is_command("connect"));
        assert!(!is_command("explain_analyze"));
    }
}
