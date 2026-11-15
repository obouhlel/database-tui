/// Written to `~/.config/pgtui/config.toml` on first launch.
pub const EXAMPLE_CONFIG: &str = r##"# pgtui configuration
# Généré au premier lancement. Toutes les clés sont optionnelles sauf `name`.

[general]
page_size = 200               # lignes chargées par page dans la vue Data
default_connection = "local"  # connexion présélectionnée dans le sélecteur
keymap = "default"            # preset (vim + flèches) ; surcharges dans [keymap]
theme = "dark"                # "dark" | "light" ; couleurs surchargeables dans [theme]
mouse = false                 # true : capture souris (désactive la sélection native du terminal)
statement_timeout_secs = 0    # timeout des requêtes en secondes, 0 = aucun
show_system_schemas = false   # afficher pg_catalog / information_schema dans l'arbre
log_level = "info"            # logs dans ~/.local/state/pgtui/pgtui.log (ou $PGTUI_LOG)
clipboard = "auto"            # "auto" | "system" | "osc52" (OSC 52 : copie via le terminal, utile en SSH)

[editor]
mode = "classic"              # "classic" (toujours en insertion) | "vim" (modal)
tab_width = 4

[[connections]]
name = "local"
host = "localhost"
port = 5432
user = "postgres"
database = "mydb"
password_env = "PGPASSWORD_LOCAL"      # variable d'environnement
# password_cmd = "pass show db/local"  # ou commande externe (lancée via sh -c)
# password = "..."                     # accepté mais déconseillé (warning)
# Sans mot de passe trouvé : ~/.pgpass, puis saisie interactive.
sslmode = "prefer"                     # disable | prefer | require | verify-ca | verify-full
# sslrootcert = "/path/to/ca.pem"      # CA pour verify-ca / verify-full
read_only = false                      # true : session en default_transaction_read_only
color = "green"                        # couleur du bandeau (rouge => production)
# production = true                    # bandeau permanent + confirmation avant tout commit
# statement_timeout_secs = 30          # surcharge le timeout général

# Base de démo fournie par `docker compose up -d` (voir README).
[[connections]]
name = "demo"
host = "localhost"
port = 5433
user = "pgtui"
database = "demo"
password_env = "PGTUI_DEMO_PASSWORD"   # export PGTUI_DEMO_PASSWORD=pgtui
color = "cyan"

# Exemple de connexion de production en lecture seule :
# [[connections]]
# name = "prod"
# host = "db.example.com"
# user = "readonly"
# database = "shop"
# password_cmd = "pass show db/prod"
# sslmode = "verify-full"
# read_only = true
# color = "red"

# Surcharge des raccourcis : [keymap.<contexte>] <action> = "touche" | ["t1", "t2"]
# Contextes : global, picker, tree, center. `?` dans l'appli liste les actions.
# Notation : "ctrl+p", "alt+enter", "shift+tab", "f5", "space", "gg" (séquence).
# [keymap.global]
# quit = ["q", "ctrl+q"]
# [keymap.tree]
# expand = ["l", "right"]

# Surcharge des couleurs du thème (noms, "#rrggbb" ou index ANSI) :
# [theme]
# accent = "#7aa2f7"
# selection_bg = "#283457"
"##;
