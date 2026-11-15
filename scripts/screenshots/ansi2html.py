"""Used by capture.sh. Convert `tmux capture-pane -e -p` output (ANSI SGR) to a standalone HTML page
that looks like a terminal window. Usage: ansi2html.py in.ans out.html "title"
"""
import html
import re
import sys
import unicodedata

FG, BG = "#c0caf5", "#1a1b26"
BASE16 = [
    "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
    "#414868", "#ff899d", "#9fe044", "#faba4a", "#8db0ff", "#c7a9ff", "#a4daff", "#c0caf5",
]


def xterm256(n):
    if n < 16:
        return BASE16[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[(n // 6) % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def apply(state, params):
    p = [int(x) if x else 0 for x in params.split(";")] if params else [0]
    i = 0
    while i < len(p):
        c = p[i]
        if c == 0:
            state.clear()
        elif c == 1:
            state["b"] = True
        elif c == 2:
            state["dim"] = True
        elif c == 3:
            state["i"] = True
        elif c == 4:
            state["u"] = True
        elif c == 7:
            state["rev"] = True
        elif c == 22:
            state.pop("b", None); state.pop("dim", None)
        elif c == 23:
            state.pop("i", None)
        elif c == 24:
            state.pop("u", None)
        elif c == 27:
            state.pop("rev", None)
        elif 30 <= c <= 37:
            state["fg"] = BASE16[c - 30]
        elif 90 <= c <= 97:
            state["fg"] = BASE16[c - 90 + 8]
        elif c == 39:
            state.pop("fg", None)
        elif 40 <= c <= 47:
            state["bg"] = BASE16[c - 40]
        elif 100 <= c <= 107:
            state["bg"] = BASE16[c - 100 + 8]
        elif c == 49:
            state.pop("bg", None)
        elif c in (38, 48) and i + 1 < len(p):
            key = "fg" if c == 38 else "bg"
            if p[i + 1] == 5 and i + 2 < len(p):
                state[key] = xterm256(p[i + 2]); i += 2
            elif p[i + 1] == 2 and i + 4 < len(p):
                state[key] = "#%02x%02x%02x" % tuple(p[i + 2:i + 5]); i += 4
        i += 1


def style(state):
    fg, bg = state.get("fg", FG), state.get("bg")
    if state.get("rev"):
        fg, bg = bg or BG, fg
    css = [f"color:{fg}"]
    if bg:
        css.append(f"background:{bg}")
    if state.get("b"):
        css.append("font-weight:700")
    if state.get("dim"):
        css.append("opacity:.6")
    if state.get("i"):
        css.append("font-style:italic")
    if state.get("u"):
        css.append("text-decoration:underline")
    return ";".join(css)


def cells(chunk):
    """Escapes text and pins double-width characters (CJK, emoji) to exactly
    two cells, as the terminal does; fallback fonts are often wider."""
    out = []
    for ch in chunk:
        if unicodedata.east_asian_width(ch) in ("W", "F"):
            out.append(f'<span style="display:inline-block;width:2ch;overflow:hidden;vertical-align:bottom">{html.escape(ch)}</span>')
        else:
            out.append(html.escape(ch))
    return "".join(out)


def convert(text):
    out = []
    for line in text.rstrip("\n").split("\n"):
        state, parts = {}, []
        for chunk in re.split(r"(\x1b\[[0-9;]*m)", line):
            m = re.fullmatch(r"\x1b\[([0-9;]*)m", chunk)
            if m:
                apply(state, m.group(1))
            elif chunk:
                parts.append(f'<span style="{style(state)}">{cells(chunk)}</span>')
        out.append("".join(parts) or " ")
    return "\n".join(out)


def main():
    src, dst, title = sys.argv[1], sys.argv[2], sys.argv[3]
    body = convert(open(src, encoding="utf-8").read())
    page = f"""<!doctype html><html><head><meta charset="utf-8"><style>
body {{ margin:0; background:#0f1018; display:inline-block; padding:24px; }}
.win {{ background:{BG}; border-radius:10px; box-shadow:0 12px 40px rgba(0,0,0,.55); overflow:hidden; }}
.bar {{ height:30px; background:#24283b; display:flex; align-items:center; padding:0 12px; gap:8px;
        font:13px 'DejaVu Sans',sans-serif; color:#a9b1d6; }}
.dot {{ width:12px; height:12px; border-radius:50%; }}
.title {{ flex:1; text-align:center; margin-right:52px; }}
pre {{ margin:0; padding:10px 12px; font-family:'JetBrains Mono','DejaVu Sans Mono','WenQuanYi Zen Hei Mono','Noto Color Emoji',monospace;
       font-size:14px; line-height:1.25; color:{FG}; font-variant-ligatures:none; }}
</style></head><body><div class="win"><div class="bar">
<span class="dot" style="background:#f7768e"></span><span class="dot" style="background:#e0af68"></span>
<span class="dot" style="background:#9ece6a"></span><span class="title">{html.escape(title)}</span></div>
<pre>{body}</pre></div></body></html>"""
    open(dst, "w", encoding="utf-8").write(page)


main()
