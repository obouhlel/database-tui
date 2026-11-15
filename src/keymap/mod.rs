//! Keymap: key sequences -> actions, per context, with user overrides.
//!
//! Resolution looks at the focused context first, then `global`. Multi-key
//! sequences (`gg`, `ctrl+w h`) are handled by [`KeyResolver`]; when a key is
//! both a complete binding and the prefix of a longer one, the shorter one
//! fires after [`SEQUENCE_TIMEOUT`].

mod key;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

pub use key::{KeyChord, display_seq};

use crate::config::KeyList;

pub const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    Global,
    Picker,
    Tree,
    Center,
    /// Center panel, Schema tab.
    Schema,
    /// Center panel, Data tab.
    Data,
    /// SQL tab, editor (only keys with Ctrl / Alt or function keys while
    /// typing; everything the editor ignores in vim normal mode).
    SqlEditor,
    /// SQL tab, results (on top of the Data tab keys).
    SqlResults,
}

impl Context {
    pub const ALL: [Context; 8] = [
        Context::Global,
        Context::Picker,
        Context::Tree,
        Context::Center,
        Context::Schema,
        Context::Data,
        Context::SqlEditor,
        Context::SqlResults,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Context::Global => "global",
            Context::Picker => "picker",
            Context::Tree => "tree",
            Context::Center => "center",
            Context::Schema => "schema",
            Context::Data => "data",
            Context::SqlEditor => "sql_editor",
            Context::SqlResults => "sql_results",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Context::Global => "Global",
            Context::Picker => "Sélecteur de connexion",
            Context::Tree => "Arbre des objets",
            Context::Center => "Panneau central",
            Context::Schema => "Onglet Schema",
            Context::Data => "Onglet Data",
            Context::SqlEditor => "Onglet SQL : éditeur",
            Context::SqlResults => "Onglet SQL : résultats",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

macro_rules! actions {
    ($($variant:ident => $name:literal, $desc:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant,)* }

        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant,)*];

            /// Name used in `[keymap]`.
            pub fn name(self) -> &'static str {
                match self { $(Action::$variant => $name,)* }
            }

            pub fn description(self) -> &'static str {
                match self { $(Action::$variant => $desc,)* }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Action::$variant),)* _ => None }
            }
        }
    };
}

actions! {
    Quit => "quit", "Quitter";
    Help => "help", "Aide complète";
    CommandPalette => "command_palette", "Palette de commandes";
    Interrupt => "interrupt", "Annuler l'opération en cours";
    FocusTree => "focus_tree", "Focus sur l'arbre";
    FocusCenter => "focus_center", "Focus sur le panneau central";
    FocusNext => "focus_next", "Panneau suivant";
    NextTab => "next_tab", "Onglet suivant";
    PrevTab => "prev_tab", "Onglet précédent";
    TabSchema => "tab_schema", "Onglet Schema";
    TabData => "tab_data", "Onglet Data";
    TabSql => "tab_sql", "Onglet SQL";
    Up => "up", "Monter";
    Down => "down", "Descendre";
    Top => "top", "Début";
    Bottom => "bottom", "Fin";
    HalfPageDown => "half_page_down", "Demi-page vers le bas";
    HalfPageUp => "half_page_up", "Demi-page vers le haut";
    PageDown => "page_down", "Page suivante";
    PageUp => "page_up", "Page précédente";
    Expand => "expand", "Déplier";
    Collapse => "collapse", "Replier / parent";
    CollapseAll => "collapse_all", "Tout replier";
    Open => "open", "Ouvrir l'objet";
    Filter => "filter", "Filtrer";
    ClearFilter => "clear_filter", "Effacer le filtre";
    ReloadCatalog => "reload_catalog", "Recharger le catalogue";
    Connect => "connect", "Se connecter";
    ScrollLeft => "scroll_left", "Colonnes vers la gauche";
    ScrollRight => "scroll_right", "Colonnes vers la droite";
    FirstColumn => "first_column", "Première colonne";
    LastColumn => "last_column", "Dernière colonne";
    NextSection => "next_section", "Section suivante";
    PrevSection => "prev_section", "Section précédente";
    FollowLink => "follow_link", "Aller à la table liée";
    JumpBack => "jump_back", "Revenir à l'objet précédent";
    Copy => "copy", "Copier (DDL ou ligne)";
    Refresh => "refresh", "Recharger";
    CellLeft => "cell_left", "Cellule à gauche";
    CellRight => "cell_right", "Cellule à droite";
    NextColumn => "next_column", "Colonne suivante";
    PrevColumn => "prev_column", "Colonne précédente";
    Detail => "detail", "Vue détail de la ligne";
    Visual => "visual", "Sélection de cellules";
    VisualLine => "visual_line", "Sélection de lignes";
    CopyRow => "copy_row", "Copier la ligne (TSV)";
    CopyAs => "copy_as", "Copier la sélection (TSV, CSV, JSON, INSERT)";
    Search => "search", "Rechercher dans les valeurs chargées";
    SearchNext => "search_next", "Occurrence suivante";
    SearchPrev => "search_prev", "Occurrence précédente";
    FilterEditor => "filter_editor", "Filtre par colonne";
    FreeWhere => "free_where", "Filtre SQL libre (WHERE)";
    ClearFilters => "clear_filters", "Effacer les filtres";
    Sort => "sort", "Trier sur la colonne (asc, desc, aucun)";
    ClearSort => "clear_sort", "Effacer le tri";
    NarrowColumn => "narrow_column", "Rétrécir la colonne";
    WidenColumn => "widen_column", "Élargir la colonne";
    AutoWidth => "auto_width", "Largeur automatique";
    HideColumn => "hide_column", "Masquer la colonne";
    ShowColumns => "show_columns", "Réafficher les colonnes masquées";
    Cancel => "cancel", "Quitter la sélection / la recherche";
    ConnectionsPage => "connections_page", "Page des connexions (serveurs détectés)";
    Disconnect => "disconnect", "Fermer la connexion";
    DeleteConnection => "delete_connection", "Supprimer la connexion de la liste";
    EditCell => "edit_cell", "Modifier la cellule";
    InsertRow => "insert_row", "Nouvelle ligne";
    DeleteRow => "delete_row", "Marquer / démarquer la ligne pour suppression";
    Undo => "undo", "Annuler le dernier changement";
    Redo => "redo", "Rétablir le changement";
    ShowChanges => "show_changes", "Changements en attente (diff, SQL généré)";
    ApplyChanges => "apply_changes", "Appliquer les changements (une transaction)";
    DiscardChanges => "discard_changes", "Abandonner tous les changements";
    Export => "export", "Exporter le résultat (CSV, TSV, JSON, INSERT)";
    Explain => "explain", "EXPLAIN de l'instruction sous le curseur";
    ExplainAnalyze => "explain_analyze", "EXPLAIN ANALYZE (exécutée puis annulée)";
    Complete => "complete", "Compléter (mots-clés, tables, colonnes, fonctions)";
    ExecuteStatement => "execute_statement", "Exécuter l'instruction sous le curseur (ou la sélection)";
    ExecuteScript => "execute_script", "Exécuter tout le script";
    HistorySearch => "history_search", "Rechercher dans l'historique";
    FocusResults => "focus_results", "Aller aux résultats";
    FocusEditor => "focus_editor", "Revenir à l'éditeur";
    NextResult => "next_result", "Résultat suivant";
    PrevResult => "prev_result", "Résultat précédent";
}

#[derive(Debug, Clone)]
struct Binding {
    seq: Vec<KeyChord>,
    action: Action,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Context, Vec<Binding>>,
}

/// Result of a lookup for a (partial) sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lookup {
    exact: Option<Action>,
    is_prefix: bool,
}

fn defaults() -> Vec<(Context, Action, &'static [&'static str])> {
    use Action::*;
    use Context::*;
    let mut v: Vec<(Context, Action, &'static [&'static str])> = vec![
        (Global, Quit, &["q"]),
        (Global, Help, &["?", "f1"]),
        (Global, CommandPalette, &[":", "ctrl+p"]),
        (Global, Interrupt, &["ctrl+c"]),
        (Global, ConnectionsPage, &["ctrl+n"]),
        (Global, FocusTree, &["ctrl+h", "ctrl+w h"]),
        (Global, FocusCenter, &["ctrl+l", "ctrl+w l"]),
        (Global, FocusNext, &["ctrl+w w", "ctrl+w ctrl+w"]),
        (Global, NextTab, &["tab"]),
        (Global, PrevTab, &["shift+tab"]),
        (Global, TabSchema, &["1", "alt+1"]),
        (Global, TabData, &["2", "alt+2"]),
        (Global, TabSql, &["3", "alt+3"]),
        (Tree, Expand, &["l", "right"]),
        (Tree, Collapse, &["h", "left"]),
        (Tree, CollapseAll, &["zM", "-"]),
        (Tree, Open, &["enter"]),
        (Tree, Filter, &["/"]),
        (Tree, ClearFilter, &["esc"]),
        (Tree, ReloadCatalog, &["R"]),
        (Tree, Disconnect, &["X"]),
        (Tree, DeleteConnection, &["dd"]),
        (Picker, Connect, &["enter"]),
        (Picker, Refresh, &["R"]),
        (Picker, Cancel, &["esc"]),
        (Schema, ScrollLeft, &["h", "left"]),
        (Schema, ScrollRight, &["l", "right"]),
        (Schema, FirstColumn, &["0"]),
        (Schema, LastColumn, &["$"]),
        (Schema, NextSection, &["]"]),
        (Schema, PrevSection, &["["]),
        (Schema, FollowLink, &["enter"]),
        (Schema, JumpBack, &["ctrl+o", "backspace"]),
        (Schema, Copy, &["y"]),
        (Schema, Refresh, &["R"]),
        (Data, CellLeft, &["h", "left"]),
        (Data, CellRight, &["l", "right"]),
        (Data, NextColumn, &["w"]),
        (Data, PrevColumn, &["b"]),
        (Data, FirstColumn, &["0", "^"]),
        (Data, LastColumn, &["$"]),
        (Data, Detail, &["enter"]),
        (Data, Visual, &["v"]),
        (Data, VisualLine, &["V"]),
        (Data, Copy, &["y"]),
        (Data, CopyRow, &["yy"]),
        (Data, CopyAs, &["Y"]),
        (Data, Search, &["/"]),
        (Data, SearchNext, &["n"]),
        (Data, SearchPrev, &["N"]),
        (Data, FilterEditor, &["f"]),
        (Data, FreeWhere, &["F"]),
        (Data, ClearFilters, &["C"]),
        (Data, Sort, &["s"]),
        (Data, ClearSort, &["S"]),
        (Data, NarrowColumn, &["<"]),
        (Data, WidenColumn, &[">"]),
        (Data, AutoWidth, &["="]),
        (Data, HideColumn, &["-"]),
        (Data, ShowColumns, &["+"]),
        (Data, Refresh, &["R"]),
        (Data, Export, &["E"]),
        (Data, EditCell, &["e", "i"]),
        (Data, InsertRow, &["o"]),
        (Data, DeleteRow, &["dd"]),
        (Data, Undo, &["u"]),
        (Data, Redo, &["ctrl+r"]),
        (Data, ShowChanges, &["D"]),
        (Data, ApplyChanges, &["ctrl+s"]),
        (Data, DiscardChanges, &["ctrl+z", "X"]),
        (
            SqlEditor,
            ExecuteStatement,
            &["ctrl+enter", "alt+enter", "ctrl+e"],
        ),
        (SqlEditor, ExecuteScript, &["ctrl+shift+enter", "f5"]),
        (SqlEditor, HistorySearch, &["ctrl+r"]),
        (SqlEditor, Complete, &["ctrl+space", "ctrl+n"]),
        (SqlEditor, Explain, &["f6"]),
        (SqlEditor, ExplainAnalyze, &["f7"]),
        (SqlResults, Explain, &["f6"]),
        (SqlResults, ExplainAnalyze, &["f7"]),
        (SqlEditor, FocusResults, &["ctrl+j"]),
        (SqlResults, FocusEditor, &["i", "ctrl+k"]),
        (SqlResults, NextResult, &["]"]),
        (SqlResults, PrevResult, &["["]),
        (SqlResults, ExecuteStatement, &["ctrl+e", "alt+enter"]),
        (SqlResults, ExecuteScript, &["f5"]),
        (SqlResults, HistorySearch, &["ctrl+r"]),
        (Data, Cancel, &["esc"]),
    ];
    for ctx in [Picker, Tree, Center, Schema, Data] {
        v.extend([
            (ctx, Up, &["k", "up"][..]),
            (ctx, Down, &["j", "down"][..]),
            (ctx, Top, &["gg", "home"][..]),
            (ctx, Bottom, &["G", "end"][..]),
            (ctx, HalfPageDown, &["ctrl+d"][..]),
            (ctx, HalfPageUp, &["ctrl+u"][..]),
            (ctx, PageDown, &["pagedown", "ctrl+f"][..]),
            (ctx, PageUp, &["pageup", "ctrl+b"][..]),
        ]);
    }
    v
}

impl Default for Keymap {
    fn default() -> Self {
        let mut bindings: HashMap<Context, Vec<Binding>> = HashMap::new();
        for (ctx, action, keys) in defaults() {
            for k in keys {
                let seq = KeyChord::parse_seq(k).expect("default keys are valid");
                bindings
                    .entry(ctx)
                    .or_default()
                    .push(Binding { seq, action });
            }
        }
        Self { bindings }
    }
}

impl Keymap {
    /// Default keymap with the `[keymap]` section applied. Returns warnings
    /// for unknown contexts, actions or keys (those entries are ignored).
    pub fn with_overrides(
        overrides: &BTreeMap<String, BTreeMap<String, KeyList>>,
    ) -> (Self, Vec<String>) {
        let mut km = Self::default();
        let mut warnings = Vec::new();
        for (ctx_name, actions) in overrides {
            let Some(ctx) = Context::from_name(ctx_name) else {
                warnings.push(format!("[keymap.{ctx_name}]: unknown context"));
                continue;
            };
            for (action_name, keys) in actions {
                let Some(action) = Action::from_name(action_name) else {
                    warnings.push(format!("[keymap.{ctx_name}] {action_name}: unknown action"));
                    continue;
                };
                let mut seqs = Vec::new();
                for k in keys.keys() {
                    match KeyChord::parse_seq(k) {
                        Ok(seq) => seqs.push(seq),
                        Err(e) => warnings.push(format!("[keymap.{ctx_name}] {action_name}: {e}")),
                    }
                }
                let list = km.bindings.entry(ctx).or_default();
                // The override replaces the action's keys and steals the keys
                // from whatever they were bound to in this context.
                list.retain(|b| b.action != action && !seqs.contains(&b.seq));
                list.extend(seqs.into_iter().map(|seq| Binding { seq, action }));
            }
        }
        (km, warnings)
    }

    fn lookup(&self, contexts: &[Context], seq: &[KeyChord]) -> Lookup {
        let mut exact = None;
        let mut is_prefix = false;
        for ctx in contexts {
            for b in self.bindings.get(ctx).into_iter().flatten() {
                if b.seq == seq {
                    exact = exact.or(Some(b.action));
                } else if b.seq.len() > seq.len() && b.seq.starts_with(seq) {
                    is_prefix = true;
                }
            }
            if exact.is_some() && !is_prefix {
                break;
            }
        }
        Lookup { exact, is_prefix }
    }

    /// Action of a single key (no sequence), looking in `contexts` in order.
    pub fn single(&self, contexts: &[Context], chord: &KeyChord) -> Option<Action> {
        contexts.iter().find_map(|ctx| {
            self.bindings
                .get(ctx)
                .into_iter()
                .flatten()
                .find(|b| b.seq.len() == 1 && &b.seq[0] == chord)
                .map(|b| b.action)
        })
    }

    /// Keys bound to `action` in `ctx`, for display.
    pub fn keys_for(&self, ctx: Context, action: Action) -> Vec<String> {
        self.bindings
            .get(&ctx)
            .into_iter()
            .flatten()
            .filter(|b| b.action == action)
            .map(|b| display_seq(&b.seq))
            .collect()
    }

    /// First key bound to `action`, looking in `ctx` then `global`.
    pub fn hint(&self, ctx: Context, action: Action) -> Option<String> {
        self.keys_for(ctx, action)
            .into_iter()
            .next()
            .or_else(|| self.keys_for(Context::Global, action).into_iter().next())
    }

    /// All `(action, keys)` of a context, in [`Action::ALL`] order.
    pub fn bindings_of(&self, ctx: Context) -> Vec<(Action, Vec<String>)> {
        Action::ALL
            .iter()
            .filter_map(|a| {
                let keys = self.keys_for(ctx, *a);
                (!keys.is_empty()).then_some((*a, keys))
            })
            .collect()
    }
}

/// Outcome of feeding a key to the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Action(Action),
    /// Waiting for the next key of a sequence.
    Pending,
    Unbound,
}

/// Accumulates keys until they form a complete binding.
#[derive(Debug, Default)]
pub struct KeyResolver {
    pending: Vec<KeyChord>,
    pending_exact: Option<Action>,
    since: Option<Instant>,
}

impl KeyResolver {
    pub fn feed(&mut self, km: &Keymap, contexts: &[Context], chord: KeyChord) -> Resolved {
        self.feed_at(km, contexts, chord, Instant::now())
    }

    pub fn feed_at(
        &mut self,
        km: &Keymap,
        contexts: &[Context],
        chord: KeyChord,
        now: Instant,
    ) -> Resolved {
        let mut seq = self.pending.clone();
        seq.push(chord);
        let look = km.lookup(contexts, &seq);
        if look.is_prefix {
            self.pending = seq;
            self.pending_exact = look.exact;
            self.since = Some(now);
            return Resolved::Pending;
        }
        if let Some(a) = look.exact {
            self.reset();
            return Resolved::Action(a);
        }
        if !self.pending.is_empty() {
            // `g` then `x`: drop the broken sequence, retry `x` alone.
            self.reset();
            return self.feed_at(km, contexts, chord, now);
        }
        Resolved::Unbound
    }

    /// Fires the shorter binding of an ambiguous sequence once it timed out.
    pub fn check_timeout(&mut self, now: Instant) -> Option<Action> {
        let since = self.since?;
        if now.duration_since(since) < SEQUENCE_TIMEOUT {
            return None;
        }
        let action = self.pending_exact;
        self.reset();
        action
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Keys typed so far, shown in the status bar.
    pub fn pending_display(&self) -> Option<String> {
        (!self.pending.is_empty()).then(|| display_seq(&self.pending))
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.pending_exact = None;
        self.since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse_seq(s).unwrap()[0]
    }

    fn feed_all(r: &mut KeyResolver, km: &Keymap, ctx: &[Context], keys: &str) -> Resolved {
        let mut last = Resolved::Unbound;
        for c in KeyChord::parse_seq(keys).unwrap() {
            last = r.feed(km, ctx, c);
        }
        last
    }

    const TREE: &[Context] = &[Context::Tree, Context::Global];

    #[test]
    fn single_keys() {
        let km = Keymap::default();
        let mut r = KeyResolver::default();
        assert_eq!(
            r.feed(&km, TREE, chord("j")),
            Resolved::Action(Action::Down)
        );
        assert_eq!(
            r.feed(&km, TREE, chord("q")),
            Resolved::Action(Action::Quit)
        );
        assert_eq!(r.feed(&km, TREE, chord("x")), Resolved::Unbound);
    }

    #[test]
    fn sequences() {
        let km = Keymap::default();
        let mut r = KeyResolver::default();
        assert_eq!(r.feed(&km, TREE, chord("g")), Resolved::Pending);
        assert_eq!(r.pending_display().as_deref(), Some("g"));
        assert_eq!(r.feed(&km, TREE, chord("g")), Resolved::Action(Action::Top));
        assert!(!r.is_pending());
        assert_eq!(
            feed_all(&mut r, &km, TREE, "ctrl+w h"),
            Resolved::Action(Action::FocusTree)
        );
    }

    #[test]
    fn broken_sequence_retries_last_key() {
        let km = Keymap::default();
        let mut r = KeyResolver::default();
        assert_eq!(r.feed(&km, TREE, chord("g")), Resolved::Pending);
        assert_eq!(
            r.feed(&km, TREE, chord("j")),
            Resolved::Action(Action::Down)
        );
    }

    #[test]
    fn ambiguous_prefix_fires_after_timeout() {
        let (km, w) = Keymap::with_overrides(&BTreeMap::from([(
            "tree".to_string(),
            BTreeMap::from([
                ("open".to_string(), KeyList::One("o".into())),
                ("expand".to_string(), KeyList::One("oo".into())),
            ]),
        )]));
        assert!(w.is_empty());
        let mut r = KeyResolver::default();
        let t0 = Instant::now();
        assert_eq!(r.feed_at(&km, TREE, chord("o"), t0), Resolved::Pending);
        assert_eq!(r.check_timeout(t0 + Duration::from_millis(100)), None);
        assert_eq!(r.check_timeout(t0 + SEQUENCE_TIMEOUT), Some(Action::Open));
        assert!(!r.is_pending());
    }

    #[test]
    fn focused_context_wins_over_global() {
        let km = Keymap::default();
        let mut r = KeyResolver::default();
        // Enter is `open` in the tree, `connect` in the picker.
        let enter = KeyChord::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(r.feed(&km, TREE, enter), Resolved::Action(Action::Open));
        assert_eq!(
            r.feed(&km, &[Context::Picker, Context::Global], enter),
            Resolved::Action(Action::Connect)
        );
    }

    #[test]
    fn overrides_replace_and_steal() {
        let (km, w) = Keymap::with_overrides(&BTreeMap::from([
            (
                "global".to_string(),
                BTreeMap::from([(
                    "quit".to_string(),
                    KeyList::Many(vec!["ctrl+q".into(), "Q".into()]),
                )]),
            ),
            (
                "tree".to_string(),
                BTreeMap::from([("filter".to_string(), KeyList::One("j".into()))]),
            ),
            ("nope".to_string(), BTreeMap::new()),
            ("tree2".to_string(), BTreeMap::new()),
        ]));
        assert_eq!(w.len(), 2, "{w:?}");
        let mut r = KeyResolver::default();
        assert_eq!(r.feed(&km, TREE, chord("q")), Resolved::Unbound);
        assert_eq!(
            r.feed(&km, TREE, chord("ctrl+q")),
            Resolved::Action(Action::Quit)
        );
        // `j` was stolen from `down`, which keeps `down` (arrow).
        assert_eq!(
            r.feed(&km, TREE, chord("j")),
            Resolved::Action(Action::Filter)
        );
        assert_eq!(km.keys_for(Context::Tree, Action::Down), vec!["Down"]);
    }

    #[test]
    fn override_warnings() {
        let (_, w) = Keymap::with_overrides(&BTreeMap::from([(
            "tree".to_string(),
            BTreeMap::from([
                ("fly".to_string(), KeyList::One("x".into())),
                ("open".to_string(), KeyList::One("hyper+x".into())),
            ]),
        )]));
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn hints_and_help() {
        let km = Keymap::default();
        assert_eq!(km.hint(Context::Tree, Action::Filter).as_deref(), Some("/"));
        assert_eq!(km.hint(Context::Tree, Action::Help).as_deref(), Some("?"));
        assert!(
            km.bindings_of(Context::Global)
                .iter()
                .any(|(a, _)| *a == Action::Quit)
        );
    }
}
