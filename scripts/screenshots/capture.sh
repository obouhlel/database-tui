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
      "env XDG_CONFIG_HOME=$WORK/config XDG_STATE_HOME=$WORK/state COLORTERM=truecolor $BIN $1; sleep 120" && break
  done
  sleep 1.5
}
keys() { for k in "$@"; do tmux send-keys -t pg -- "$k"; sleep 0.15; done; sleep 0.6; }
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
