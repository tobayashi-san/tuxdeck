# Icons of the sample profiles (assets/templates/icons): our own drawings in the style of
# the official sample profiles. Run: python3 tools/template-art.py
# official sample profiles: full-bleed tiles, a colour world per profile, soft "clay"
# glyphs with a top light and a drop shadow).
import os, sys
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "templates", "icons")
os.makedirs(OUT, exist_ok=True)

# ---- glyphs: (paths drawn with a thick round stroke, filled shapes) in a 144 box
G = {
 'record':   ('', '<circle cx="72" cy="72" r="30" fill="url(#red)"/><ellipse cx="62" cy="58" rx="12" ry="7" fill="#fff" opacity=".55"/>'),
 'broadcast':('M44 50 a36 36 0 0 0 0 44 M100 50 a36 36 0 0 1 0 44 M32 38 a54 54 0 0 0 0 68 M112 38 a54 54 0 0 1 0 68', '<circle cx="72" cy="72" r="12" fill="url(#red)"/>'),
 'scene':    ('M40 56 h56 a8 8 0 0 1 8 8 v36 a8 8 0 0 1 -8 8 h-56 a8 8 0 0 1 -8 -8 v-36 a8 8 0 0 1 8 -8 Z M48 42 h56 a8 8 0 0 1 8 8 v40', ''),
 'mic':      ('M72 30 a12 12 0 0 1 12 12 v26 a12 12 0 0 1 -24 0 v-26 a12 12 0 0 1 12 -12 Z M48 66 a24 24 0 0 0 48 0 M72 92 v18 M58 112 h28', ''),
 'micoff':   ('M72 30 a12 12 0 0 1 12 12 v26 a12 12 0 0 1 -24 0 v-26 a12 12 0 0 1 12 -12 Z M48 66 a24 24 0 0 0 48 0 M72 92 v18 M58 112 h28', ''),
 'camera':   ('M36 54 h16 l8 -12 h24 l8 12 h16 a6 6 0 0 1 6 6 v40 a6 6 0 0 1 -6 6 h-72 a6 6 0 0 1 -6 -6 v-40 a6 6 0 0 1 6 -6 Z', '<circle cx="72" cy="80" r="14" fill="none" stroke="url(#clay)" stroke-width="10"/>'),
 'webcam':   ('M72 32 a26 26 0 1 1 0 52 a26 26 0 1 1 0 -52 Z M50 112 h44 M72 84 v28', '<circle cx="72" cy="58" r="8" fill="url(#clay)"/>'),
 'chat':     ('M32 40 h56 a8 8 0 0 1 8 8 v28 a8 8 0 0 1 -8 8 h-30 l-14 12 v-12 h-12 a8 8 0 0 1 -8 -8 v-28 a8 8 0 0 1 8 -8 Z M104 60 h4 a8 8 0 0 1 8 8 v26 a8 8 0 0 1 -8 8 h-8 v10 l-12 -10 h-20', ''),
 'window':   ('M30 42 h84 a6 6 0 0 1 6 6 v52 a6 6 0 0 1 -6 6 h-84 a6 6 0 0 1 -6 -6 v-52 a6 6 0 0 1 6 -6 Z M24 58 h96', '<circle cx="72" cy="84" r="9" fill="url(#red)"/>'),
 'folder':   ('', '<path d="M28 48 a8 8 0 0 1 8 -8 h22 l10 10 h40 a8 8 0 0 1 8 8 v44 a8 8 0 0 1 -8 8 h-72 a8 8 0 0 1 -8 -8 Z" fill="url(#clay)"/>'),
 'download': ('M72 32 v52 M50 64 l22 22 l22 -22 M38 108 h68', ''),
 'calc':     ('M44 30 h56 a8 8 0 0 1 8 8 v68 a8 8 0 0 1 -8 8 h-56 a8 8 0 0 1 -8 -8 v-68 a8 8 0 0 1 8 -8 Z M50 46 h44', '<g fill="url(#clay)"><circle cx="54" cy="70" r="5"/><circle cx="72" cy="70" r="5"/><circle cx="90" cy="70" r="5"/><circle cx="54" cy="90" r="5"/><circle cx="72" cy="90" r="5"/><circle cx="90" cy="90" r="5"/></g>'),
 'editor':   ('M88 34 l22 22 l-50 50 h-22 v-22 Z M78 44 l22 22', ''),
 'terminal': ('M30 38 h84 a6 6 0 0 1 6 6 v56 a6 6 0 0 1 -6 6 h-84 a6 6 0 0 1 -6 -6 v-56 a6 6 0 0 1 6 -6 Z M42 62 l14 12 l-14 12 M66 88 h24', ''),
 'copy':     ('M52 50 h44 a6 6 0 0 1 6 6 v50 a6 6 0 0 1 -6 6 h-44 a6 6 0 0 1 -6 -6 v-50 a6 6 0 0 1 6 -6 Z M40 92 v-54 a6 6 0 0 1 6 -6 h40', ''),
 'paste':    ('M46 40 h52 a6 6 0 0 1 6 6 v62 a6 6 0 0 1 -6 6 h-52 a6 6 0 0 1 -6 -6 v-62 a6 6 0 0 1 6 -6 Z M60 34 h24 v14 h-24 Z M56 72 h32 M56 90 h22', ''),
 'lock':     ('M54 66 v-12 a18 18 0 0 1 36 0 v12', '<rect x="42" y="64" width="60" height="46" rx="10" fill="url(#clay)"/>'),
 'timer':    ('M72 44 a32 32 0 1 1 0 64 a32 32 0 1 1 0 -64 Z M72 76 v-16 M62 32 h20 M100 50 l6 -6', ''),
 'globe':    ('M72 32 a40 40 0 1 1 0 80 a40 40 0 1 1 0 -80 Z M32 72 h80 M72 32 c-22 22 -22 58 0 80 M72 32 c22 22 22 58 0 80', ''),
 'mail':     ('M30 44 h84 a6 6 0 0 1 6 6 v44 a6 6 0 0 1 -6 6 h-84 a6 6 0 0 1 -6 -6 v-44 a6 6 0 0 1 6 -6 Z M28 48 l44 30 l44 -30', ''),
 'gamepad':  ('M46 54 h52 a24 24 0 0 1 22 30 l-6 18 a10 10 0 0 1 -16 4 l-12 -12 h-28 l-12 12 a10 10 0 0 1 -16 -4 l-6 -18 a24 24 0 0 1 22 -30 Z M50 68 v16 M42 76 h16', '<g fill="url(#clay)"><circle cx="92" cy="70" r="5"/><circle cx="102" cy="82" r="5"/></g>'),
 'joystick': ('M72 36 v44 M44 84 h56 a8 8 0 0 1 8 8 v12 a6 6 0 0 1 -6 6 h-60 a6 6 0 0 1 -6 -6 v-12 a8 8 0 0 1 8 -8 Z', '<circle cx="72" cy="34" r="12" fill="url(#red)"/>'),
 'headset':  ('M36 84 v-14 a36 36 0 0 1 72 0 v14 M36 80 h10 v26 h-10 Z M98 80 h10 v26 h-10 Z', ''),
 'deafen':   ('M36 84 v-14 a36 36 0 0 1 72 0 v14 M36 80 h10 v26 h-10 Z M98 80 h10 v26 h-10 Z', ''),
 'prev':     ('', '<path d="M98 42 v60 l-40 -30 Z M54 42 v60" fill="url(#clay)" stroke="url(#clay)" stroke-width="10" stroke-linejoin="round" stroke-linecap="round"/>'),
 'play':     ('', '<path d="M56 40 l48 32 l-48 32 Z" fill="url(#clay)" stroke="url(#clay)" stroke-width="10" stroke-linejoin="round"/>'),
 'next':     ('', '<path d="M46 42 v60 l40 -30 Z M90 42 v60" fill="url(#clay)" stroke="url(#clay)" stroke-width="10" stroke-linejoin="round" stroke-linecap="round"/>'),
 'volume':   ('M86 56 a22 22 0 0 1 0 32 M98 44 a38 38 0 0 1 0 56', '<path d="M34 60 h14 l22 -18 v60 l-22 -18 h-14 Z" fill="url(#clay)" stroke="url(#clay)" stroke-width="6" stroke-linejoin="round"/>'),
 'mute':     ('M88 58 l24 28 M112 58 l-24 28', '<path d="M34 60 h14 l22 -18 v60 l-22 -18 h-14 Z" fill="url(#clay)" stroke="url(#clay)" stroke-width="6" stroke-linejoin="round"/>'),
 'shuffle':  ('M30 52 h18 c26 0 30 40 56 40 h10 M30 92 h18 c12 0 18 -8 24 -18 M84 56 c6 -3 12 -4 20 -4 h10 M104 40 l12 12 l-12 12 M104 80 l12 12 l-12 12', ''),
}

# Struck-through glyphs: the slash, set off from the glyph by a gap in the tile colour.
CUT = {'micoff': 'M38 34 L106 110', 'deafen': 'M34 34 L110 110'}

THEMES = {
 # background gradient (top, bottom), glow colour
 'stream': ('#2b1250', '#0e0718', '#7c3aed'),
 'stream2': ('#4a1030', '#12070c', '#e11d48'),
 'work':   ('#f4f5f8', '#d9dce4', None),
 'blurple':('#6d72f6', '#4b4fd6', None),
 'green':  ('#2fd16b', '#169c49', None),
 'dark':   ('#2a2d36', '#15171c', '#3b82f6'),
}
CHIP = {  # WORK: coloured rounded chip on a light tile
 'orange': ('#fb923c', '#ea580c'), 'blue': ('#4f8ff7', '#2563eb'), 'slate': ('#6b7385', '#3f4553'),
 'teal': ('#2dd4bf', '#0f9488'), 'purple': ('#8b5cf6', '#6d28d9'), 'red': ('#f87171', '#dc2626'), 'ink': ('#3a3f4c', '#1d2027'),
}

def svg(name, glyph, theme, chip=None):
    top, bottom, glow = THEMES[theme]
    stroke, extra = G[glyph]
    defs = f'''<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>
<linearGradient id="clay" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#d9dde8"/></linearGradient>
<radialGradient id="red" cx=".4" cy=".35" r=".7"><stop offset="0" stop-color="#ff8a8a"/><stop offset=".55" stop-color="#ef2b4b"/><stop offset="1" stop-color="#a3122c"/></radialGradient>
<filter id="sh" x="-30%" y="-30%" width="160%" height="170%"><feDropShadow dx="0" dy="5" stdDeviation="4" flood-color="#000" flood-opacity=".38"/></filter>'''
    body = f'<rect width="144" height="144" fill="url(#bg)"/>'
    if glow:
        defs += f'<radialGradient id="gl" cx=".5" cy=".18" r=".75"><stop offset="0" stop-color="{glow}" stop-opacity=".55"/><stop offset="1" stop-color="{glow}" stop-opacity="0"/></radialGradient>'
        body += '<rect width="144" height="144" fill="url(#gl)"/>'
    scale = 0.86
    if chip:
        a, b = CHIP[chip]
        defs += f'<linearGradient id="chip" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient>'
        body += '<rect x="22" y="22" width="100" height="100" rx="24" fill="url(#chip)" filter="url(#sh)"/><rect x="22" y="22" width="100" height="50" rx="24" fill="#fff" opacity=".12"/>'
        scale = 0.62
    g = ''
    if stroke:
        g += f'<path d="{stroke}" fill="none" stroke="url(#clay)" stroke-width="10" stroke-linecap="round" stroke-linejoin="round"/>'
    g += extra
    slash = ''
    if glyph in CUT:
        # Cut a gap out of the glyph (the tile shows through), then draw the slash.
        defs += f'<mask id="cut" maskUnits="userSpaceOnUse" x="-20" y="-20" width="184" height="184"><rect x="-20" y="-20" width="184" height="184" fill="#fff"/><path d="{CUT[glyph]}" stroke="#000" stroke-width="24" stroke-linecap="round"/></mask>'
        g = f'<g mask="url(#cut)">{g}</g>'
        slash = f'<path d="{CUT[glyph]}" stroke="url(#clay)" stroke-width="10" stroke-linecap="round"/>'
    g += slash
    # Depth: the same glyph a few pixels lower in a darker tone (the "clay" edge).
    edge = g.replace('url(#clay)', '#9aa1b3').replace('url(#red)', '#7a0f22')
    g = f'<g transform="translate(0,4)">{edge}</g>' + g
    t = 72 * (1 - scale)
    body += f'<g transform="translate({t:.2f},{t:.2f}) scale({scale})" filter="url(#sh)">{g}</g>'
    open(f'{OUT}/{name}.svg', 'w').write(f'<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144"><defs>{defs}</defs>{body}</svg>')

ICONS = [
 # stream
 ('stream-obs', 'window', 'stream'), ('stream-record', 'record', 'stream'), ('stream-live', 'broadcast', 'stream'), ('stream-scene', 'scene', 'stream'),
 ('stream-mic', 'micoff', 'stream'), ('stream-screenshot', 'camera', 'stream'), ('stream-webcam', 'webcam', 'stream'), ('stream-chat', 'chat', 'stream'),
 # work
 ('work-files', 'folder', 'work', 'orange'), ('work-downloads', 'download', 'work', 'blue'), ('work-calc', 'calc', 'work', 'ink'), ('work-editor', 'editor', 'work', 'teal'),
 ('work-terminal', 'terminal', 'work', 'ink'), ('work-copy', 'copy', 'work', 'slate'), ('work-paste', 'paste', 'work', 'slate'), ('work-lock', 'lock', 'work', 'purple'),
 ('work-timer', 'timer', 'work', 'red'), ('work-web', 'globe', 'work', 'blue'), ('work-mail', 'mail', 'work', 'blue'), ('work-screenshot', 'camera', 'work', 'slate'),
 # play
 ('play-mic', 'micoff', 'blurple'), ('play-deafen', 'deafen', 'blurple'), ('play-voice', 'headset', 'blurple'), ('play-chat', 'chat', 'blurple'),
 ('play-prev', 'prev', 'green'), ('play-play', 'play', 'green'), ('play-next', 'next', 'green'), ('play-shuffle', 'shuffle', 'green'),
 ('play-volume', 'volume', 'green'), ('play-mute', 'mute', 'green'), ('play-games', 'gamepad', 'dark'), ('play-launcher', 'joystick', 'dark'),
]
for row in ICONS:
    svg(row[0], row[1], row[2], row[3] if len(row) > 3 else None)
print(len(ICONS))
