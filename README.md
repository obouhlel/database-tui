# pgtui

Client PostgreSQL en mode texte (TUI), entièrement pilotable au clavier — une alternative légère à DBeaver pour l'usage quotidien.

> **État : jalon 4 / 6** — arbre des connexions (plusieurs ouvertes à la fois), onglet Schema, onglet Data (pagination serveur, tri, filtres, recherche, détail, copie) et édition avec staging, diff, commit transactionnel et détection de conflits.
> L'onglet SQL arrive au jalon suivant (voir [Feuille de route](#feuille-de-route)).

## Captures

![Onglet Data : recherche](docs/screenshots/10-data-recherche.png)

| | |
|---|---|
| ![Onglet Schema : colonnes](docs/screenshots/02-schema-colonnes.png) | ![DDL](docs/screenshots/04-ddl.png) |
| ![Filtre par colonne](docs/screenshots/11-data-filtre.png) | ![Détail d'une ligne](docs/screenshots/12-data-detail.png) |
| ![Tri et sélection](docs/screenshots/13-data-tri.png) | ![Connexion de production](docs/screenshots/08-prod.png) |
| ![Changements en attente](docs/screenshots/22-edition-grille.png) | ![Diff avant / après](docs/screenshots/23-diff.png) |
| ![SQL généré](docs/screenshots/24-sql-genere.png) | ![Conflit détecté](docs/screenshots/26-conflit.png) |
| ![Plusieurs connexions ouvertes](docs/screenshots/19-multi-connexions.png) | ![Page des connexions](docs/screenshots/01-selecteur.png) |

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

## Détection des serveurs locaux

Au démarrage, pgtui cherche les serveurs PostgreSQL de la machine : port 5432 sur `127.0.0.1` et `::1`, et sockets Unix dans `/var/run/postgresql`, `/run/postgresql` et `/tmp`. Chaque candidat reçoit le paquet d'ouverture `SSLRequest` : un serveur PostgreSQL répond `S` ou `N`, ce qui l'identifie sans s'authentifier. Rien n'est scanné sur le réseau.

Les serveurs trouvés apparaissent dans la page des connexions (`Ctrl+n`), sous les connexions configurées. `Entrée` ouvre un formulaire **utilisateur / mot de passe / base** (base `postgres` par défaut). En cas d'erreur, le formulaire revient avec le message du serveur. Une fois connecté, pgtui propose d'ajouter le serveur à `config.toml`, **sans le mot de passe**. `R` relance la détection.

```toml
[general]
discover = true            # false pour désactiver
discover_ports = [5432]    # ex. [5432, 5433]
```

| ![Serveurs détectés](docs/screenshots/14-detection.png) | ![Toutes les bases](docs/screenshots/16-bases.png) |
|---|---|

## Arbre des connexions

pgtui démarre directement sur l'écran principal : l'arbre à gauche, les données à droite. La racine de l'arbre liste **les connexions** (config.toml, `--url`, `DATABASE_URL`, serveurs ajoutés depuis la détection). `Entrée` ou `l` sur une connexion la connecte et la déplie ; plusieurs connexions peuvent être ouvertes en même temps, chacune avec son état (« connexion… », version du serveur, « erreur », « déconnecté »). `X` ferme la connexion sous le curseur, `dd` la retire de la liste et de `config.toml` après confirmation (aucune base n'est supprimée sur le serveur). La page des connexions et des serveurs détectés s'ouvre avec `Ctrl+n` ou `:connections`.

## Toutes les bases du serveur

Sous chaque connexion, l'arbre affiche **toutes les bases** du serveur, avec leur taille (`pg_database`, hors modèles). La base de connexion est dépliée ; déplier une autre base ouvre une connexion vers elle, avec les mêmes identifiants, et charge ses schémas. Une base sans le droit `CONNECT` est marquée « accès refusé ». La barre de statut et l'en-tête des onglets indiquent la base de l'objet ouvert.

## Édition des données

Rien n'est écrit en base tant que vous n'avez pas validé : chaque modification est d'abord **mise en attente** (staging), pour la table ouverte.

- `e` (ou `i`) modifie la cellule : éditeur sur place, ou fenêtre multi-ligne pour les textes longs et le JSON (indenté à l'ouverture). `Entrée` valide, `Tab` / `Shift+Tab` valide et passe à la colonne suivante / précédente, `Esc` annule, `Ctrl+n` met `NULL`, `Ctrl+d` met `DEFAULT`, `Alt+Entrée` passe en multi-ligne (où `Ctrl+s` valide).
- La valeur est vérifiée selon le type avant d'être mise en attente (entiers et leurs bornes, décimaux, réels, booléens, UUID, dates, heures, horodatages, JSON, tableaux) ; l'erreur s'affiche sous la cellule. Le serveur reste juge : chaque valeur lui est envoyée en paramètre et convertie par `CAST`.
- `o` ajoute une ligne (en haut de la grille, toutes colonnes à `DEFAULT`, curseur sur la première colonne sans valeur par défaut), `dd` marque la ligne (ou la sélection `V`) pour suppression ou la démarque, et retire une ligne ajoutée. `u` annule le dernier changement, `Ctrl+r` le rétablit.
- Dans la grille, les cellules modifiées, les lignes ajoutées et les lignes supprimées ont chacune leur couleur ; sous le curseur, l'en-tête affiche l'ancienne valeur (« avant : … »). La barre de statut compte les changements en attente.
- `D` ouvre la vue des changements : tableau **avant → après** groupé par type (UPDATE, INSERT, DELETE), et onglet **SQL généré** (`Tab`) avec les instructions exactes, valeurs en littéraux pour la lecture. `e` permet d'y modifier le SQL avant de l'exécuter (sans détection de conflit dans ce cas).
- `Ctrl+s` applique tout dans **une seule transaction**, après confirmation affichant le nombre de lignes par type. Sur une connexion de production, il faut taper le nom de la connexion. En cas d'erreur, `ROLLBACK` complet, message du serveur indiquant le changement fautif, et les changements restent en attente pour correction.
- **Conflits** : un `UPDATE` vérifie dans son `WHERE` que chaque colonne modifiée a toujours sa valeur lue (`t.col::text = $n`) ; un `DELETE` vérifie la version de ligne (`xmin`). Si une ligne a changé entre-temps, rien n'est écrit, les valeurs actuelles sont rechargées et la cellule en conflit est soulignée, avec la valeur actuellement en base. Rééditer la cellule confirme votre valeur sur cette nouvelle base ; `u` annule le changement.
- `Ctrl+z` ou `X` abandonne tous les changements (avec confirmation ; `u` les restaure).
- L'édition demande une clé primaire, ou une contrainte unique sur des colonnes `NOT NULL` ; sinon, et sur les vues ou les connexions `read_only`, l'en-tête indique pourquoi la table est en lecture seule.
- Le staging est propre à une table : ouvrir un autre objet, fermer la connexion ou quitter avec des changements en attente propose de les appliquer (`a`), de les abandonner (`x`) ou de rester (`Esc`).

## Lancement

```
pgtui                          # arbre des connexions, aucune connexion ouverte
pgtui --connection <nom>       # ouvre directement une entrée de config.toml
pgtui --url <URL>              # ajoute et ouvre une URL postgres:// ou "host=… user=…"
pgtui --config <fichier>       # autre fichier de configuration
pgtui --check-config           # valide la configuration et quitte
```

Si `DATABASE_URL` est défini, il apparaît comme connexion supplémentaire dans l'arbre (sans connexion automatique).

## Configuration

Fichier : `~/.config/pgtui/config.toml`, créé avec un exemple commenté au premier lancement.

```toml
[general]
page_size = 200               # lignes par page dans la vue Data
default_connection = "local"  # sélectionnée dans l'arbre au démarrage
keymap = "default"            # preset ; surcharges dans [keymap]
theme = "terminal"            # "terminal" (couleurs du terminal) | "dark" | "light"
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
# tunnel_cmd = "ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -L {port}:db.interne:5432 bastion"
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

### Tunnel SSH (ou autre)

Pour une base joignable seulement par rebond, `tunnel_cmd` donne la commande qui ouvre le tunnel ; `{port}` y est remplacé par un port local libre choisi par pgtui :

```toml
[[connections]]
name = "prod"
host = "db.interne"          # informatif : la connexion passe par le tunnel
user = "app"
database = "shop"
password_cmd = "pass show db/prod"
tunnel_cmd = "ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -L {port}:db.interne:5432 bastion"
# tunnel_timeout_secs = 15
```

À la connexion, pgtui lance la commande (stdin fermé : elle ne doit rien demander, d'où `BatchMode=yes` avec une clé ou ssh-agent), attend que le port réponde, puis se connecte à `127.0.0.1:{port}`. Toutes les bases du serveur passent par ce tunnel. Si la commande échoue, son message d'erreur (stderr) s'affiche ; si le tunnel tombe, la session est signalée « déconnectée » et `:reconnect` le relance. Le processus est arrêté à la déconnexion et en quittant. Toute commande qui ouvre un port local convient (`kubectl port-forward`, `cloud-sql-proxy`…).

![Connexion par tunnel](docs/screenshots/17-tunnel.png)

### Thème

Par défaut (`theme = "terminal"`), pgtui n'utilise que les couleurs de votre terminal (texte et fond par défaut, 16 couleurs ANSI), comme nvim sans `termguicolors` : il reprend automatiquement le thème du terminal, clair ou sombre. `dark` et `light` sont des thèmes en couleurs exactes (truecolor).

| Terminal Tokyo Night | Terminal One Light | Terminal Gruvbox |
|---|---|---|
| ![](docs/screenshots/18-theme-tokyonight.png) | ![](docs/screenshots/18-theme-onelight.png) | ![](docs/screenshots/18-theme-gruvbox.png) |

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
| | `Ctrl+n` | Page des connexions et des serveurs détectés |
| Arbre | `j`/`k` · `↓`/`↑` | Descendre / monter |
| | `gg` / `G` · `Home`/`End` | Début / fin |
| | `Ctrl+d` / `Ctrl+u` | Demi-page |
| | `PgDn`/`PgUp` · `Ctrl+f`/`Ctrl+b` | Page |
| | `l` / `h` · `→`/`←` | Déplier / replier (ou aller au parent) |
| | `zM` · `-` | Tout replier |
| | `Enter` | Connecter / ouvrir l'objet / déplier le dossier |
| | `X` | Fermer la connexion sous le curseur |
| | `dd` | Retirer la connexion de la liste et de `config.toml` (confirmation) |
| | `/` | Filtre flou (`schema.table` pour qualifier) |
| | `Esc` | Effacer le filtre |
| | `R` | Recharger le catalogue |
| Page connexions | `Enter` | Se connecter (formulaire utilisateur / mot de passe pour un serveur détecté) |
| | `R` | Relancer la détection des serveurs locaux |
| | `Esc` · `q` | Revenir à l'écran principal |
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
| Édition | `e` · `i` | Modifier la cellule (`Entrée` valider, `Tab` suivante, `Esc` annuler, `Ctrl+n` NULL, `Ctrl+d` DEFAULT, `Alt+Entrée` multi-ligne) |
| | `o` | Nouvelle ligne |
| | `dd` | Marquer / démarquer la ligne (ou la sélection) pour suppression |
| | `u` / `Ctrl+r` | Annuler / rétablir le dernier changement |
| | `D` | Changements en attente : avant → après, SQL généré (`Tab`), SQL modifiable (`e`) |
| | `Ctrl+s` | Appliquer en une transaction (confirmation ; nom de la connexion en production) |
| | `Ctrl+z` · `X` | Abandonner tous les changements (confirmation) |

**Presse-papier** — `auto` utilise le presse-papier système (X11, Wayland, macOS, Windows) et bascule sur la séquence OSC 52 (copie via le terminal) en SSH ou si le presse-papier système est indisponible. Sous tmux, OSC 52 demande `set -g set-clipboard on`.

Commandes : `:q`, `:help`, `:reload`, `:connect <nom>`, `:connections`, `:reconnect`, `:disconnect`, `:schema`, `:data`, `:sql`.

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
│   ├── changes.rs   changements en attente (UPDATE / INSERT / DELETE) et résultat de leur application
│   ├── postgres/    tokio-postgres + rustls : connexions, TLS, catalogue, détails, DDL
│   ├── discover.rs  détection des serveurs locaux (SSLRequest)
│   └── worker.rs    tâche qui possède les connexions (une par base, ouvertes à la demande) ; DbCommand → DbEvent
├── app/             état + reducer (aucune I/O) : App::update(Msg) -> Vec<Effect>
├── keymap/          touches → actions par contexte, séquences, surcharges
└── ui/              rendu : table_view (réutilisable), schema_view, sql_view, tree, status_bar, help…
```

- **Séparation état / rendu / I/O** : l'UI ne fait jamais d'appel DB. Le reducer renvoie des `Effect` (commande DB, copie) que la boucle exécute ; le worker répond par des `DbEvent`.
- **Onglet Data** : chaque colonne est lue en texte (`col::text`), ce qui couvre tous les types (y compris PostGIS, ranges, enums) ; l'affichage s'adapte au type (nombres alignés à droite, JSON indenté, bytea en hexadécimal tronqué, `NULL` distinct de la chaîne vide). Les valeurs des filtres partent en paramètres `$n` convertis côté serveur (`CAST($1::text AS numeric(12,2))`), jamais concaténées. Pagination `LIMIT/OFFSET` triée sur la clé, fenêtre glissante de 10 pages en mémoire, comptage borné à 1 000 000 lignes pour rester rapide sur les grosses tables.
- **DDL reconstruit depuis le catalogue** (`pg_get_constraintdef`, `pg_get_indexdef`, `pg_get_viewdef`, `pg_get_functiondef`…) : pas de dépendance à `pg_dump`. Un test d'intégration recrée les tables de démo à partir du DDL généré et vérifie que la structure relue est identique. Non inclus : droits (`GRANT`), propriétaire (`ALTER … OWNER`), politiques RLS, options des séquences d'identité.
- **Staging** (`app/staging.rs`) : les lignes sont identifiées par leur clé, donc les changements survivent au tri, aux filtres et à la pagination. Annuler / rétablir conserve des instantanés de l'état (petit : seules les lignes touchées). Le moteur traduit un `ChangeSet` neutre (`db/changes.rs`) en SQL paramétré (`db/postgres/changes.rs`) ; l'application tient le verrou de la connexion `session` pendant la transaction, pour qu'aucune lecture de page ne s'y glisse.
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
- [x] **Jalon 4** — édition, staging, vue diff, commit transactionnel, détection de conflits
- [ ] **Jalon 5** — onglet SQL : éditeur, exécution, résultats, historique, annulation, transactions manuelles
- [ ] **Jalon 6** — autocomplétion, EXPLAIN, export, finitions

## Licence

Domaine public — [The Unlicense](LICENSE). Vous pouvez copier, modifier, distribuer et utiliser ce code librement, sans condition.
