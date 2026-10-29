# Generates the README art in docs/images (hero, devices, social preview) from the bundled
# font and icon pack:  nix-shell -p python3Packages.fonttools --run "python3 tools/readme-art.py ."
# social-preview.png (uploaded in the repo settings): rsvg-convert -w 1280 -h 640 docs/images/social-preview.svg
import re, sys
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.transformPen import TransformPen

REPO = sys.argv[1]
font = TTFont(f'{REPO}/assets/fonts/DejaVuSans-Bold.ttf')
gs = font.getGlyphSet(); cmap = font.getBestCmap(); upm = font['head'].unitsPerEm

def glyph(ch, cx, cy, size):
    """Path of one character, centred on (cx, cy), cap height ~size."""
    g = gs[cmap[ord(ch)]]
    b = BoundsPen(gs); g.draw(b); x0, y0, x1, y1 = b.bounds
    s = size / (y1 - y0)
    pen = SVGPathPen(gs)
    tx = cx - (x0 + x1) / 2 * s; ty = cy + (y0 + y1) / 2 * s
    g.draw(TransformPen(pen, (s, 0, 0, -s, tx, ty)))
    return pen.getCommands()

def text_path(txt, x, y, size, anchor='start', spacing=0.0):
    """Path of a word (baseline y), size = font size in px."""
    s = size / upm; hmtx = font['hmtx']
    widths = [hmtx[cmap[ord(c)]][0] * s + spacing for c in txt]
    total = sum(widths) - spacing
    cx = x - (total / 2 if anchor == 'middle' else total if anchor == 'end' else 0)
    out = []
    for c, w in zip(txt, widths):
        if c != ' ':
            pen = SVGPathPen(gs); gs[cmap[ord(c)]].draw(TransformPen(pen, (s, 0, 0, -s, cx, y))); out.append(pen.getCommands())
        cx += w
    return ' '.join(out)

def icon(name, x, y, size, color):
    src = open(f'{REPO}/assets/iconpack/icons/{name}.svg').read()
    inner = re.search(r'<svg[^>]*>(.*)</svg>', src, re.S).group(1)
    inner = inner.replace('#eceff4', color)
    return f'<svg x="{x}" y="{y}" width="{size}" height="{size}" viewBox="0 0 144 144">{inner}</svg>'

GRAD = {  # starter-icon colours
    'teal': ('#5eead4', '#0d9488'), 'green': ('#34d399', '#059669'), 'blue': ('#60a5fa', '#2563eb'),
    'indigo': ('#818cf8', '#4f46e5'), 'fuchsia': ('#e879f9', '#c026d3'), 'amber': ('#fbbf24', '#d97706'),
    'rose': ('#fb7185', '#e11d48'), 'orange': ('#fb923c', '#ea580c'), 'slate': ('#94a3b8', '#475569'),
}
def defs():
    d = ''.join(f'<linearGradient id="{k}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient>' for k, (a, b) in GRAD.items())
    d += '<linearGradient id="shine" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".22"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/></linearGradient>'
    d += '<linearGradient id="body" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#23262e"/><stop offset="1" stop-color="#121418"/></linearGradient>'
    return d

# ------------------------------------------------------------------ hero: a 7x3 deck spelling TUXDECK
K, G, P = 96, 18, 34
cols, rows = 7, 3
pw, ph = cols * K + (cols - 1) * G + 2 * P, rows * K + (rows - 1) * G + 2 * P
W, H = pw + 80, ph + 70
ox, oy = 40, 24
word = [('T', 'teal'), ('U', 'green'), ('X', 'teal'), ('D', 'blue'), ('E', 'indigo'), ('C', 'fuchsia'), ('K', 'amber')]
top = ['previous', 'play', 'next', None, 'volume-down', 'volume-up', 'mute']
bottom = ['folder', 'terminal', None, 'globe', 'clock', 'sleep', 'brightness']
keys = []
def kx(c): return ox + P + c * (K + G)
def ky(r): return oy + P + r * (K + G)
for c, name in enumerate(top):
    keys.append(f'<g><rect x="{kx(c)}" y="{ky(0)}" width="{K}" height="{K}" rx="20" fill="#0b0c0f" stroke="#2b2f38"/>' + (icon(name, kx(c) + 18, ky(0) + 18, K - 36, '#4b5263') if name else '') + '</g>')
for c, name in enumerate(bottom):
    keys.append(f'<g><rect x="{kx(c)}" y="{ky(2)}" width="{K}" height="{K}" rx="20" fill="#0b0c0f" stroke="#2b2f38"/>' + (icon(name, kx(c) + 18, ky(2) + 18, K - 36, '#4b5263') if name else '') + '</g>')
for c, (ch, col) in enumerate(word):
    x, y = kx(c), ky(1)
    keys.append(f'<g class="k k{c}" style="transform-origin:{x + K / 2}px {y + K / 2}px">'
                f'<rect x="{x}" y="{y}" width="{K}" height="{K}" rx="20" fill="url(#{col})"/>'
                f'<rect x="{x}" y="{y}" width="{K}" height="{K}" rx="20" fill="url(#shine)"/>'
                f'<rect x="{x + .75}" y="{y + .75}" width="{K - 1.5}" height="{K - 1.5}" rx="19.5" fill="none" stroke="#fff" stroke-opacity=".2" stroke-width="1.5"/>'
                f'<path d="{glyph(ch, x + K / 2, y + K / 2, 44)}" fill="#fff"/></g>')
css = ''.join(f'.k{c}{{animation-delay:{c * 0.12:.2f}s}}' for c in range(7))
hero = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="A Stream Deck whose keys spell TuxDeck">
<title>TuxDeck</title>
<style>
.k{{animation:press 7s ease-in-out infinite}}
{css}
@keyframes press{{0%,8%,100%{{transform:scale(1);filter:none}}3%{{transform:scale(.92);filter:brightness(1.25)}}}}
@media (prefers-reduced-motion:reduce){{.k{{animation:none}}}}
</style>
<defs>{defs()}<filter id="drop" x="-10%" y="-10%" width="120%" height="140%"><feDropShadow dx="0" dy="14" stdDeviation="14" flood-color="#000" flood-opacity=".35"/></filter></defs>
<rect x="{ox}" y="{oy}" width="{pw}" height="{ph}" rx="38" fill="url(#body)" stroke="#3a3f4b" filter="url(#drop)"/>
<rect x="{ox + 1}" y="{oy + 1}" width="{pw - 2}" height="{ph - 2}" rx="37" fill="none" stroke="#fff" stroke-opacity=".06"/>
{''.join(keys)}
</svg>'''
open(f'{REPO}/docs/images/hero.svg', 'w').write(hero)

# ------------------------------------------------------------------ device lineup, to scale
# (name, cols, rows, extra, tested)  extra: 'neo' info bar + touch points, 'plus' strip + dials, 'pedal'
devices = [('Mini', 3, 2, None, False), ('Neo', 4, 2, 'neo', True), ('MK.2', 5, 3, None, True),
           ('+', 4, 2, 'plus', False), ('XL', 8, 4, None, False), ('Pedal', 3, 1, 'pedal', False)]
k, g, p = 26, 7, 12
parts = []; x = 20; base = 210
labels = []
for name, c, r, extra, tested in devices:
    w = c * k + (c - 1) * g + 2 * p
    h = r * k + (r - 1) * g + 2 * p
    if extra == 'neo': h += 26
    if extra == 'plus': h += 30 + 34
    if extra == 'pedal':
        w, h = 3 * 34 + 2 * 6 + 2 * 10, 70
    y = base - h
    parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12" fill="url(#body)" stroke="#3a3f4b"/>')
    if extra == 'pedal':
        for i in range(3):
            parts.append(f'<rect x="{x + 10 + i * 40}" y="{y + 10}" width="34" height="{h - 20}" rx="9" fill="#0b0c0f" stroke="#2b2f38"/>')
    else:
        palette = ['teal', 'green', 'teal', 'rose', 'blue', 'blue', 'indigo', 'amber', 'orange', 'slate', 'fuchsia']
        n = 0
        for rr in range(r):
            for cc in range(c):
                fill = f'url(#{palette[n % len(palette)]})' if (rr < 2 and cc < 3) or (rr == r - 1 and r > 2 and (cc < 3 or cc >= c - 2)) or r == 2 else '#0b0c0f'
                if r == 2 and c == 4 and n >= 8: fill = '#0b0c0f'
                parts.append(f'<rect x="{x + p + cc * (k + g)}" y="{y + p + rr * (k + g)}" width="{k}" height="{k}" rx="6" fill="{fill}" stroke="#2b2f38" stroke-width=".8"/>')
                n += 1
        ky0 = y + p + r * (k + g) - g
        if extra == 'neo':
            parts.append(f'<circle cx="{x + p + 6}" cy="{ky0 + 15}" r="4" fill="#4b5263"/><circle cx="{x + w - p - 6}" cy="{ky0 + 15}" r="4" fill="#4b5263"/>')
            parts.append(f'<rect x="{x + p + 16}" y="{ky0 + 7}" width="{w - 2 * p - 32}" height="16" rx="3" fill="#0b0c0f" stroke="#2b2f38"/>')
            parts.append(f'<path d="{text_path("12:30", x + w / 2, ky0 + 19.5, 11, "middle")}" fill="#7dd3fc"/>')
        if extra == 'plus':
            parts.append(f'<rect x="{x + p}" y="{ky0 + 8}" width="{w - 2 * p}" height="22" rx="4" fill="#0b0c0f" stroke="#2b2f38"/>')
            for i in range(4):
                cx = x + p + k / 2 + i * (k + g)
                parts.append(f'<circle cx="{cx}" cy="{ky0 + 50}" r="12" fill="#2b2f38" stroke="#4b5263"/><circle cx="{cx}" cy="{ky0 + 50}" r="5" fill="#1a1d24"/>')
    cx = x + w / 2
    labels.append(f'<path d="{text_path(name, cx, base + 30, 15, "middle")}" class="t"/>')
    if tested:
        labels.append(f'<circle cx="{cx - 33}" cy="{base + 47}" r="3.5" fill="#22c55e"/><path d="{text_path("tested", cx - 25, base + 51, 11)}" class="s"/>')
    x += w + 26
W2 = x - 6
devices_svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W2}" height="270" viewBox="0 0 {W2} 270" role="img" aria-label="Supported Stream Deck models: Mini, Neo, MK.2, Plus, XL and Pedal">
<title>Supported devices</title>
<style>.t{{fill:#57606a}}.s{{fill:#57606a}}@media (prefers-color-scheme:dark){{.t{{fill:#c9d1d9}}.s{{fill:#8b949e}}}}</style>
<defs>{defs()}</defs>
{''.join(parts)}{''.join(labels)}
</svg>'''
open(f'{REPO}/docs/images/devices.svg', 'w').write(devices_svg)
print('ok', W, H, W2)

# ------------------------------------------------------------------ social preview 1280x640 (upload in repo settings)
hero_inner = re.search(r'<defs>.*</svg>', hero, re.S).group(0)[:-6]
social = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">
<rect width="1280" height="640" fill="#0d1014"/>
<circle cx="1180" cy="-40" r="420" fill="#2563eb" opacity=".10"/><circle cx="80" cy="700" r="380" fill="#059669" opacity=".10"/>
<g transform="translate(176,40) scale(1.0)">{hero_inner}</g>
<path d="{text_path("Stream Deck software for Linux", 640, 548, 46, "middle")}" fill="#f0f3f6"/>
<path d="{text_path("plugins · profiles · Neo, MK.2, XL, +, Mini, Pedal", 640, 598, 24, "middle")}" fill="#8b949e"/>
</svg>'''
open(f'{REPO}/docs/images/social-preview.svg', 'w').write(social)
