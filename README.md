# pgtui

Client PostgreSQL en mode texte (TUI), entièrement pilotable au clavier — une alternative légère à DBeaver pour l'usage quotidien.

> **État : jalon 3 / 6** — connexions, arbre des objets, onglet Schema, onglet Data en lecture (pagination serveur, tri, filtres, recherche, détail, copie).
> L'édition et l'onglet SQL arrivent aux jalons suivants (voir [Feuille de route](#feuille-de-route)).

## Captures

![Onglet Data : recherche](docs/screenshots/10-data-recherche.png)

| | |
|---|---|
| ![Onglet Schema : colonnes](docs/screenshots/02-schema-colonnes.png) | ![DDL](docs/screenshots/04-ddl.png) |
| ![Filtre par colonne](docs/screenshots/11-data-filtre.png) | ![Détail d'une ligne](docs/screenshots/12-data-detail.png) |
| ![Tri et sélection](docs/screenshots/13-data-tri.png) | ![Connexion de production](docs/screenshots/08-prod.png) |

Captures du vrai binaire, régénérables avec `scripts/screenshots/capture.sh` (tmux, Python, Node + Playwright, base de démo lancée).

## Installation

Prérequis : Rust stable ≥ 1.97.

```bash
git clone https://github.com/obouhlel/database-tui
cd database-tui
cargo install --path .
```

Le binaire s'appelle `pgtui`.

## Démarrage rapide avec la base de démo

```bash
docker compose up -d                 # PostgreSQL 18 sur le port 5433, données de démo chargées
export PGTUI_DEMO_PASSWORD=pgtui
pgtui --connection demo              # la connexion "demo" figure dans la config générée
```

Ou sans configuration :

```bash
pgtui --url postgres://pgtui:pgtui@localhost:5433/demo
```

La base de démo (`docker/initdb/`) contient trois schémas (`shop`, `hr`, `audit`), ~350 000 lignes et tous les types que pgtui doit gérer : enum, uuid, jsonb, tableaux, inet, bytea, interval, timestamptz, table partitionnée, vue, vue matérialisée, séquence, fonction, procédure, trigger, table sans clé primaire, table de 42 colonnes, données Unicode (accents, CJK, emoji).

## Lancement

```
pgtui                          # sélecteur de connexion
pgtui --connection <nom>       # connexion directe à une entrée de config.toml
pgtui --url <URL>              # connexion directe à une URL postgres:// ou "host=… user=…"
pgtui --config <fichier>       # autre fichier de configuration
pgtui --check-config           # valide la configuration et quitte
```

Si `DATABASE_URL` est défini, il apparaît comme entrée supplémentaire dans le sélecteur (sans connexion automatique).

## Configuration

Fichier : `~/.config/pgtui/config.toml`, créé avec un exemple commenté au premier lancement.

```toml
[general]
page_size = 200               # lignes par page dans la vue Data
default_connection = "local"  # présélectionnée dans le sélecteur
keymap = "default"            # preset ; surcharges dans [keymap]
theme = "dark"                # "dark" | "light"
mouse = false                 # capture souris (désactive la sélection native du terminal)
statement_timeout_secs = 0    # 0 = aucun timeout
show_system_schemas = false
log_level = "info"            # logs : ~/.local/state/pgtui/pgtui.log (ou $PGTUI_LOG)
clipboard = "auto"            # "auto" | "system" | "osc52"

[editor]
mode = "classic"              # "classic" | "vim"

[[connections]]
name = "local"
host = "localhost"            # ou chemin de socket Unix (/var/run/postgresql)
port = 5432
user = "postgres"
database = "mydb"
password_env = "PGPASSWORD_LOCAL"      # variable d'environnement
# password_cmd = "pass show db/local"  # commande externe (sh -c, 1re ligne de la sortie)
# password = "..."                     # accepté mais déconseillé (avertissement)
sslmode = "prefer"                     # disable | prefer | require | verify-ca | verify-full
# sslrootcert = "/path/ca.pem"         # CA pour verify-ca / verify-full
read_only = false                      # true : session en default_transaction_read_only = on
color = "green"                        # couleur du bandeau ; "red" => production
# production = true                    # bandeau permanent + confirmation avant tout commit
# statement_timeout_secs = 30
```

**Mot de passe** — ordre de résolution : `password_cmd` > `password_env` > `password` > `~/.pgpass` (ou `$PGPASSFILE`). Si rien n'est trouvé, la connexion est tentée sans mot de passe (auth `trust`/`peer`), puis pgtui le demande si le serveur l'exige.

**TLS** — mêmes règles que libpq : `prefer` et `require` chiffrent sans vérifier le certificat, `verify-ca` vérifie la chaîne, `verify-full` vérifie aussi le nom d'hôte. Racines utilisées : `sslrootcert`, sinon le magasin système et les racines Mozilla.

**Production** — une connexion `color = "red"` (ou `production = true`) affiche un bandeau permanent ; à partir du jalon 4, tout commit demandera une confirmation explicite.

**Lecture seule** — `read_only = true` ouvre la session en `default_transaction_read_only = on`, et l'édition sera désactivée dans l'interface.

### Raccourcis personnalisés

```toml
[keymap.global]
quit = ["q", "ctrl+q"]

[keymap.tree]
expand = ["l", "right", "enter"]
```

Contextes : `global`, `picker`, `tree`, `center`, `schema`, `data`. Notation : `ctrl+p`, `alt+enter`, `shift+tab`, `f5`, `space`, `gg` (séquence), `ctrl+w h` (séquence avec modificateur). La liste des actions et leur nom figurent dans l'aide (`?`). Une surcharge remplace les touches par défaut de l'action, et une touche réaffectée est retirée de son ancienne action.

### Thème

```toml
[theme]
accent = "#7aa2f7"
selection_bg = "#283457"
```

Couleurs : noms (`red`, `light-blue`, `orange`…), `#rrggbb` ou index ANSI (`208`).

## Raccourcis

| Contexte | Touches | Action |
|---|---|---|
| Global | `q` · `:q` | Quitter |
| | `?` · `F1` | Aide complète |
| | `:` · `Ctrl+p` | Ligne de commande |
| | `Ctrl+h` · `Ctrl+w h` | Focus sur l'arbre |
| | `Ctrl+l` · `Ctrl+w l` | Focus sur le panneau central |
| | `Ctrl+w w` | Panneau suivant |
| | `Tab` / `Shift+Tab` | Onglet suivant / précédent |
| | `1` `2` `3` · `Alt+1/2/3` | Onglet Schema / Data / SQL |
| | `Ctrl+c` | Annuler l'opération en cours (ne quitte pas) |
| Arbre | `j`/`k` · `↓`/`↑` | Descendre / monter |
| | `gg` / `G` · `Home`/`End` | Début / fin |
| | `Ctrl+d` / `Ctrl+u` | Demi-page |
| | `PgDn`/`PgUp` · `Ctrl+f`/`Ctrl+b` | Page |
| | `l` / `h` · `→`/`←` | Déplier / replier (ou aller au parent) |
| | `zM` · `-` | Tout replier |
| | `Enter` | Ouvrir l'objet / déplier le dossier |
| | `/` | Filtre flou (`schema.table` pour qualifier) |
| | `Esc` | Effacer le filtre |
| | `R` | Recharger le catalogue |
| Sélecteur | `Enter` | Se connecter |
| Onglet Schema | `[` / `]` | Section précédente / suivante (Colonnes, Index, Contraintes, Triggers, Relations, DDL, Infos) |
| | `j`/`k`, `gg`/`G`, `Ctrl+d`/`Ctrl+u` | Naviguer dans la section |
| | `h`/`l` · `←`/`→`, `0` / `$` | Défiler horizontalement (colonnes, ou texte du DDL) |
| | `Enter` | Aller à la table liée (colonne FK, ligne de Relations) |
| | `Ctrl+o` · `Backspace` | Revenir à l'objet précédent |
| | `y` | Copier le DDL complet (section DDL) ou la ligne courante en TSV |
| | `R` | Recharger les détails |
| Onglet Data | `h`/`j`/`k`/`l` · flèches | Déplacer le curseur de cellule |
| | `gg` / `G`, `Ctrl+d`/`Ctrl+u`, `PgUp`/`PgDn` | Début / fin / demi-page / page (chargement paresseux) |
| | `0`/`^` / `$`, `w` / `b` | Première / dernière colonne, colonne suivante / précédente |
| | `Enter` | Vue détail de la ligne (JSON indenté, bytea, textes longs ; `[`/`]` ligne préc./suiv., `y` copie en JSON) |
| | `/`, `n` / `N` | Rechercher dans les valeurs chargées (insensible à la casse sauf majuscule), occurrence suivante / précédente |
| | `f` | Filtre par colonne (`=`, `!=`, `<`, `<=`, `>`, `>=`, `LIKE`, `ILIKE`, `~`, `~*`, `IS NULL`, `IS NOT NULL`, AND / OR) |
| | `F` / `C` | Filtre SQL libre (`WHERE …`) / effacer tous les filtres |
| | `s` / `S` | Trier sur la colonne (asc → desc → aucun, multi-colonnes) / effacer le tri |
| | `v` / `V` | Sélection de cellules / de lignes (`Esc` pour sortir) |
| | `y`, `yy`, `Y` | Copier la cellule (ou la sélection), la ligne, ou la sélection au choix TSV / CSV / JSON / INSERT |
| | `<` / `>` / `=`, `-` / `+` | Rétrécir / élargir / largeur auto, masquer la colonne / tout réafficher |
| | `R`, `Ctrl+c` | Relancer la requête, annuler une requête en cours |

**Presse-papier** — `auto` utilise le presse-papier système (X11, Wayland, macOS, Windows) et bascule sur la séquence OSC 52 (copie via le terminal) en SSH ou si le presse-papier système est indisponible. Sous tmux, OSC 52 demande `set -g set-clipboard on`.

Commandes : `:q`, `:help`, `:reload`, `:connect <nom>`, `:connections`, `:reconnect`, `:schema`, `:data`, `:sql`.

Les terminaux compatibles avec le protocole clavier kitty (kitty, WezTerm, Ghostty, foot, Alacritty récent) distinguent `Ctrl+Enter` d'`Enter` ; pgtui l'active automatiquement. Ailleurs (tmux, gnome-terminal), des touches de repli seront toujours proposées.

## Architecture

```
src/
├── main.rs          CLI, chargement config, logs, runtime tokio
├── run.rs           boucle d'événements unique (clavier, événements DB, tick, signaux)
├── terminal.rs      raw mode / écran alternatif, restauration garantie (Drop + hook de panic)
├── clipboard.rs     presse-papier système + repli OSC 52
├── config/          config.toml (serde), mots de passe, ~/.pgpass
├── sql/             quoting des identifiants/littéraux, lexer SQL (coloration, découpage)
├── db/
│   ├── backend.rs   trait Backend (un moteur = une implémentation)
│   ├── catalog.rs   modèle neutre des objets (schémas, tables, vues…)
│   ├── schema.rs    détails neutres d'un objet (colonnes, index, contraintes, FK, stats)
│   ├── postgres/    tokio-postgres + rustls : connexions, TLS, catalogue, détails, DDL
│   └── worker.rs    tâche qui possède les connexions ; DbCommand → DbEvent
├── app/             état + reducer (aucune I/O) : App::update(Msg) -> Vec<Effect>
├── keymap/          touches → actions par contexte, séquences, surcharges
└── ui/              rendu : table_view (réutilisable), schema_view, sql_view, tree, status_bar, help…
```

- **Séparation état / rendu / I/O** : l'UI ne fait jamais d'appel DB. Le reducer renvoie des `Effect` (commande DB, copie) que la boucle exécute ; le worker répond par des `DbEvent`.
- **Onglet Data** : chaque colonne est lue en texte (`col::text`), ce qui couvre tous les types (y compris PostGIS, ranges, enums) ; l'affichage s'adapte au type (nombres alignés à droite, JSON indenté, bytea en hexadécimal tronqué, `NULL` distinct de la chaîne vide). Les valeurs des filtres partent en paramètres `$n` convertis côté serveur (`CAST($1::text AS numeric(12,2))`), jamais concaténées. Pagination `LIMIT/OFFSET` triée sur la clé, fenêtre glissante de 10 pages en mémoire, comptage borné à 1 000 000 lignes pour rester rapide sur les grosses tables.
- **DDL reconstruit depuis le catalogue** (`pg_get_constraintdef`, `pg_get_indexdef`, `pg_get_viewdef`, `pg_get_functiondef`…) : pas de dépendance à `pg_dump`. Un test d'intégration recrée les tables de démo à partir du DDL généré et vérifie que la structure relue est identique. Non inclus : droits (`GRANT`), propriétaire (`ALTER … OWNER`), politiques RLS, options des séquences d'identité.
- **Deux connexions par session** : `session` (requêtes utilisateur, écritures, transactions) et `catalog` (métadonnées, toujours en lecture seule), pour que l'arbre et l'autocomplétion ne soient jamais bloqués par une requête longue.
- **Moteurs futurs** : ajouter un module implémentant `db::Backend` (MySQL, SQLite…) et une branche dans `db::connect`.
- **Pourquoi `tokio-postgres` plutôt que `sqlx`** : requêtes 100 % dynamiques (pas d'intérêt pour les macros vérifiées à la compilation), annulation native (`CancelToken`), paramètres envoyés au format texte pour laisser Postgres analyser les valeurs saisies — jamais de concaténation.

## Tests

```bash
cargo test                                     # tests unitaires
docker compose up -d
PGTUI_TEST_DATABASE_URL=postgres://pgtui:pgtui@localhost:5433/demo cargo test   # + intégration
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Sans `PGTUI_TEST_DATABASE_URL`, les tests d'intégration sont ignorés avec un message.

## Feuille de route

- [x] **Jalon 1** — config, connexions (TLS, mots de passe, lecture seule, prod), arbre des objets, barre de statut, aide, restauration du terminal
- [x] **Jalon 2** — onglet Schema : colonnes, index, contraintes, triggers, FK entrantes/sortantes (navigation), DDL coloré et copiable, statistiques
- [x] **Jalon 3** — onglet Data en lecture : tableau virtualisé, pagination serveur, tri, recherche, filtres, vue détail, copie
- [ ] **Jalon 4** — édition, staging, vue diff, commit transactionnel, détection de conflits
- [ ] **Jalon 5** — onglet SQL : éditeur, exécution, résultats, historique, annulation, transactions manuelles
- [ ] **Jalon 6** — autocomplétion, EXPLAIN, export, finitions

## Licence

Domaine public — [The Unlicense](LICENSE). Vous pouvez copier, modifier, distribuer et utiliser ce code librement, sans condition.
