#!/usr/bin/env bash
# Regenerates docs/screenshots/*.png from the real binary.
# Needs: tmux, python3, node + playwright (chromium), the demo DB (docker compose up -d).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
HERE="$ROOT/scripts/screenshots"
WORK=$(mktemp -d)
trap 'tmux -f "$WORK/tmux.conf" kill-server 2>/dev/null || true; rm -rf "$WORK"' EXIT

cargo build --release --manifest-path "$ROOT/Cargo.toml" -q
BIN="$ROOT/target/release/pgtui"
mkdir -p "$WORK/config/pgtui"
cat > "$WORK/config/pgtui/config.toml" <<'TOML'
[general]
default_connection = "local"
[[connections]]
name = "local"
user = "postgres"
database = "mydb"
color = "green"
[[connections]]
name = "demo"
port = 5433
user = "pgtui"
database = "demo"
password_cmd = "printf pgtui"
color = "cyan"
[[connections]]
name = "via-tunnel"
host = "db.interne"
user = "pgtui"
database = "demo"
password_cmd = "printf pgtui"
tunnel_cmd = "python3 -I ROOT/tests/fixtures/forward.py {port} 127.0.0.1 5433"
[[connections]]
name = "prod-demo"
port = 5433
user = "pgtui"
database = "demo"
password_cmd = "printf pgtui"
read_only = true
color = "red"
TOML
sed -i "s|ROOT|$ROOT|" "$WORK/config/pgtui/config.toml"
cat > "$WORK/tmux.conf" <<'TMUX'
set -g default-terminal "tmux-256color"
set -as terminal-features ",*:RGB"
set -g status off
set -sg escape-time 0
TMUX

start() { # args, width, height
  tmux -f "$WORK/tmux.conf" kill-server 2>/dev/null || true
  # Restarting tmux right after kill-server can race: retry.
  for _ in 1 2 3 4 5; do
    sleep 0.5
    tmux -f "$WORK/tmux.conf" new-session -d -s pg -x "$2" -y "$3" \
      "env XDG_CONFIG_HOME=${CONFIG:-$WORK/config} XDG_STATE_HOME=$WORK/state XDG_DATA_HOME=$WORK/data COLORTERM=truecolor $BIN $1; sleep 120" && break
  done
  sleep 1.5
}
keys() { for k in "$@"; do tmux send-keys -t pg -- "$k"; sleep 0.15; done; sleep 0.6; }
# Literal text; `;` is sent apart (tmux reads a trailing `;` as a separator).
text() {
  local rest="$1" part
  while [[ "$rest" == *";"* ]]; do
    part="${rest%%;*}"; rest="${rest#*;}"
    [ -n "$part" ] && tmux send-keys -t pg -l -- "$part"
    tmux send-keys -t pg '\;'
  done
  [ -n "$rest" ] && tmux send-keys -t pg -l -- "$rest"
  sleep 0.4
}
shot() { # name, title, [palette]
  tmux capture-pane -e -N -p -t pg > "$WORK/$1.ans"
  python3 -I "$HERE/ansi2html.py" "$WORK/$1.ans" "$WORK/$1.html" "$2" "${3:-tokyonight}"
}
# The same screen in several terminal palettes (theme "terminal").
palettes() { # name, title
  tmux capture-pane -e -N -p -t pg > "$WORK/$1.ans"
  for p in tokyonight onelight gruvbox; do
    python3 -I "$HERE/ansi2html.py" "$WORK/$1.ans" "$WORK/$1-$p.html" "$2 — terminal $p" "$p"
  done
}

start "" 110 18; keys C-n; shot 01-selecteur "pgtui — page des connexions"
start "" 130 24; keys j Enter; sleep 1; keys G Enter; sleep 1.5; shot 19-multi-connexions "pgtui — plusieurs connexions ouvertes"
start "--connection demo" 140 32
keys / o r d e r s Enter Enter; sleep 0.5; shot 02-schema-colonnes "pgtui — Schema : colonnes"
keys ']' ']' ']' ']'; shot 03-relations "pgtui — Schema : relations"
keys ']'; shot 04-ddl "pgtui — Schema : DDL"
keys ']'; shot 05-infos "pgtui — Schema : infos"
keys C-h / c u s; shot 06-filtre "pgtui — filtre de l'arbre"
keys Enter '?'; shot 07-aide "pgtui — aide"
start "--connection prod-demo" 140 24
keys / p r o d u c t s Enter Enter; sleep 0.5; keys ']' ']'; shot 08-prod "pgtui — connexion de production"

start "--connection demo" 150 30
keys / c u s t o m e r s Enter Enter 2; sleep 1; shot 09-data "pgtui — Data : customers"
keys j l l l; keys / Z o é Enter; sleep 0.3; shot 10-data-recherche "pgtui — Data : recherche"
keys Escape f; keys BTab I; keys Tab; keys '%' z o '%'; sleep 0.3; shot 11-data-filtre "pgtui — Data : filtre par colonne"
keys Escape; keys g g j Enter; keys j j j j j j j j j j j j j j; sleep 0.3; shot 12-data-detail "pgtui — Data : détail d'une ligne"
keys Escape; keys C-h / o r d e r s Enter Enter; sleep 1
keys '$' h s s; sleep 1; keys V j j j; shot 13-data-tri "pgtui — Data : tri décroissant et sélection"

start "--connection via-tunnel" 130 22
keys / c u s t Enter Enter 2; sleep 1.5; shot 17-tunnel "pgtui — connexion par tunnel (tunnel_cmd)"
keys C-h 1; sleep 0.8; keys C-l; sleep 0.3
start "--connection demo" 130 26
keys / o r d e r s Enter Enter 2; sleep 1; keys l l l s s; sleep 1; keys / p a i d Enter; sleep 0.5
palettes 18-theme "pgtui — thème terminal"

# SQL tab scenes (milestone 5).
start "--connection demo" 140 30
keys 3
text "select c.country, count(*) as clients, round(avg(o.total), 2) as panier_moyen"; keys Enter
text "from shop.customers c join shop.orders o on o.customer_id = c.id"; keys Enter
text "group by c.country order by clients desc;"; keys Enter Enter
text "select id, emial from shop.customers;"
keys Up Up C-e; sleep 1.5; shot 30-sql-resultat "pgtui — onglet SQL : résultat"
keys Down Down C-e; sleep 1; shot 31-sql-erreur "pgtui — onglet SQL : erreur et position"
keys C-r; text "count"; shot 32-sql-historique "pgtui — onglet SQL : historique (Ctrl+r)"; keys Escape
keys C-a; text "begin;"; keys Enter; text "update shop.products set price = price * 1.1;"; keys Enter
text "select name, price from shop.products order by price desc limit 5;"
keys F5; sleep 0.5; shot 33-sql-garde "pgtui — onglet SQL : UPDATE sans WHERE"
keys o; sleep 1.5; shot 34-sql-transaction "pgtui — onglet SQL : transaction manuelle"
keys C-a; text "rollback"; keys C-e; sleep 0.5

# Milestone 6: EXPLAIN, completion, editable results, palette, export.
start "--connection demo" 140 34
keys 3
text "select c.country, sum(o.total) from shop.customers c"; keys Enter
text "join shop.orders o on o.customer_id = c.id"; keys Enter
text "where o.status = 'paid' group by c.country order by 2 desc"
keys F7; sleep 2; shot 35-explain "pgtui — EXPLAIN ANALYZE"
start "--connection demo" 140 28
keys 3; text "select o. from shop.orders o join shop.customers c on c.id = o.customer_id"
keys Home Right Right Right Right Right Right Right Right Right C-Space; sleep 1; keys Escape C-Space; sleep 0.5
shot 36-completion "pgtui — autocomplétion (Ctrl+Space)"; keys Escape
keys C-a; text "select id, sku, name, price, stock from shop.products order by id limit 8"; keys C-e; sleep 1
keys C-j j l l l e C-u; text "199.90"; keys Enter
shot 37-resultat-modifiable "pgtui — résultat SQL modifiable"
keys u; keys i C-p; text "exp"; shot 38-palette "pgtui — palette de commandes"; keys Escape
start "--connection demo" 140 26
keys / c u s t o m e r s Enter Enter 2; sleep 1; keys E; shot 39-export "pgtui — export"; keys Escape

# Editing scenes (milestone 4): a scratch table, recreated on each run.
export PGPASSWORD=pgtui
PSQL=(psql -h localhost -p 5433 -U pgtui -d demo -qX -v ON_ERROR_STOP=1)
"${PSQL[@]}" <<'SQL'
DROP SCHEMA IF EXISTS atelier CASCADE;
CREATE SCHEMA atelier;
CREATE TABLE atelier.produits (id serial PRIMARY KEY, nom text NOT NULL, prix numeric(8,2),
  stock integer DEFAULT 0, actif boolean DEFAULT true, attributs jsonb);
INSERT INTO atelier.produits (nom, prix, stock, actif, attributs) VALUES
 ('Clavier', 49.90, 12, true, '{"couleur": "noir", "dispo": ["FR", "BE"]}'),
 ('Souris', 19.50, 40, true, NULL),
 ('Écran 27"', 289.00, 3, true, '{"taille": 27}'),
 ('Câble USB-C', 9.99, 150, true, NULL),
 ('Casque', 79.00, 0, false, '{"sans_fil": true}');
SQL
mkdir -p "$WORK/config-edit/pgtui"
cat > "$WORK/config-edit/pgtui/config.toml" <<'TOML'
[[connections]]
name = "demo"
port = 5433
user = "pgtui"
database = "demo"
password_cmd = "printf pgtui"
color = "cyan"
[[connections]]
name = "prod"
port = 5433
user = "pgtui"
database = "demo"
password_cmd = "printf pgtui"
color = "red"
TOML
export CONFIG="$WORK/config-edit/"
start "--connection demo" 140 24
keys / p r o d u i t s Enter Enter 2; sleep 1
keys l e C-u; text "Clavier mécanique"; keys Enter
keys j l e C-u; text "abc"; keys Enter; shot 20-edition-validation "pgtui — validation du type avant staging"
keys Escape G d d g g o; text "Webcam"; keys Tab; text "59.00"; keys Tab; text "8"; keys Enter
keys j '$' e Down End BSpace BSpace BSpace BSpace BSpace BSpace BSpace; text '"blanc",'
shot 21-edition-json "pgtui — édition d'une valeur JSON"
keys C-s j; shot 22-edition-grille "pgtui — changements en attente dans la grille"
keys D; shot 23-diff "pgtui — changements en attente : avant / après"
keys Tab; shot 24-sql-genere "pgtui — changements en attente : SQL généré"
keys C-s; shot 25-confirmation "pgtui — confirmation avant écriture"
keys o; sleep 1
keys g g j 0 l l e C-u; text "17.90"; keys Enter
"${PSQL[@]}" -c "UPDATE atelier.produits SET prix = 21.00 WHERE id = 2"
keys C-s o; sleep 1; shot 26-conflit "pgtui — conflit détecté : rien n'est écrit"
keys Escape; shot 28-conflit-grille "pgtui — cellule en conflit et valeur actuelle"
start "--connection prod" 140 24
keys / p r o d u i t s Enter Enter 2; sleep 1
keys j j d d C-s; text "pr"; shot 27-confirmation-prod "pgtui — confirmation sur une connexion de production"
unset CONFIG
"${PSQL[@]}" -c "DROP SCHEMA atelier CASCADE"

# Discovery scenes: need a server on 127.0.0.1:5432 whose `postgres` user has
# the password $PGTUI_SHOT_PASSWORD (default: secret).
if (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; then
  PW="${PGTUI_SHOT_PASSWORD:-secret}"
  start "" 120 24
  keys C-n G; shot 14-detection "pgtui — serveurs détectés"
  keys Enter; keys "$PW"; shot 15-connexion "pgtui — connexion à un serveur détecté"
  keys Enter; sleep 1.5; keys n; keys G l; sleep 1; shot 16-bases "pgtui — toutes les bases du serveur"
fi

mkdir -p "$ROOT/docs/screenshots"
node "$HERE/render.js" "$ROOT/docs/screenshots" "$WORK"/[0-9]*.html
