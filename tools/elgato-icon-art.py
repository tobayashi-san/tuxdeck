# TuxDeck's icons, made from Elgato's open icon set @elgato/icons (MIT,
# https://github.com/elgatosf/icons): the built-in action icons, the starter keys, the
# sample profiles and the bundled icon pack. Brand logos of the set (logo-*) are left out.
#
#   npm pack @elgato/icons@2.5.1 && tar xzf elgato-icons-2.5.1.tgz
#   python3 tools/elgato-icon-art.py package
import json, os, re, shutil, sys

PKG = sys.argv[1] if len(sys.argv) > 1 else 'package'
REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..')
SVG_DIR = os.path.join(PKG, 'svg', 'l')


def glyph(name, color, size, x, y, extra=''):
    """The icon `name` (24×24) as an SVG group of `size` px at (x, y), filled with `color`."""
    src = open(os.path.join(SVG_DIR, f'{name}.svg')).read()
    inner = re.search(r'<svg[^>]*>(.*)</svg>', src, re.S).group(1).replace('currentColor', color)
    s = size / 24
    return f'<g transform="translate({x:.2f},{y:.2f}) scale({s:.4f})" fill="{color}"{extra}>{inner}</g>'


def filled(name):
    """The filled variant where the set has one."""
    return f'{name}--filled' if os.path.exists(os.path.join(SVG_DIR, f'{name}--filled.svg')) else name


def write(path, body, defs=''):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    d = f'<defs>{defs}</defs>' if defs else ''
    open(path, 'w').write(f'<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">{d}{body}</svg>')


# ---- built-in actions: light glyph on the key's own background
ACTIONS = {
    'keys.logic': 'stream-deck-key',
    'multiactions.delay': 'time-delay',
    'multiactions.random': 'shuffle',
    'multiactions.routine': 'multi-action',
    'multiactions.routine2': 'multi-action-switch',
    'page.goto': 'layers',
    'page.indicator': 'more',
    'page.next': 'chevron-right',
    'page.previous': 'chevron-left',
    'profile.backtoparent': 'folder-back',
    'profile.openchild': 'folder',
    'profile.rotate': 'profile-switch',
    'soundboard.playaudio': 'sound-effect',
    'soundboard.stopaudioplay': 'sound-effect-off',
    'system.close': 'close',
    'system.digitaltime': 'time',
    'system.hotkey': 'hotkey',
    'system.hotkeyswitch': 'keyboard',
    'system.keybrightness': 'brightness',
    'system.multimedia': 'play-pause',
    'system.open': 'folder-open',
    'system.openapp': 'apps',
    'system.pagination': 'carousel',
    'system.sleep': 'night',
    'system.text': 'text-align-left',
    'system.timer': 'timer',
    'system.website': 'globe',
}
for uuid, name in ACTIONS.items():
    write(f'{REPO}/assets/action-icons/com.elgato.streamdeck.{uuid}.svg', glyph(name, '#e6e9ef', 92, 26, 26))
# TuxDeck's own built-in actions
for uuid, name in {'dev.tuxdeck.infobar.text': 'text-set'}.items():
    write(f'{REPO}/assets/action-icons/{uuid}.svg', glyph(name, '#e6e9ef', 92, 26, 26))


# ---- starter keys: white glyph on a coloured tile
def tile(top, bottom):
    defs = (f'<linearGradient id="t" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>'
            '<linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".22"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/></linearGradient>'
            '<filter id="sh" x="-30%" y="-30%" width="160%" height="170%"><feDropShadow dx="0" dy="3" stdDeviation="3" flood-color="#000" flood-opacity=".3"/></filter>')
    body = ('<rect x="4" y="4" width="136" height="136" rx="30" fill="url(#t)"/>'
            '<rect x="4" y="4" width="136" height="136" rx="30" fill="url(#sheen)"/>')
    return defs, body


STARTER = {
    'brightness': ('brightness', '#fbbf24', '#d97706'),
    'home': ('home', '#fb923c', '#ea580c'),
    'lock': ('lock', '#94a3b8', '#475569'),
    'mute': ('volume-muted', '#fb7185', '#e11d48'),
    'next': ('skip-forward', '#5eead4', '#0d9488'),
    'play': ('play-pause', '#34d399', '#059669'),
    'previous': ('skip-back', '#5eead4', '#0d9488'),
    'sleep': ('night', '#818cf8', '#4f46e5'),
    'timer': ('timer', '#e879f9', '#c026d3'),
    'volume-down': ('volume-down', '#60a5fa', '#2563eb'),
    'volume-up': ('volume', '#60a5fa', '#2563eb'),
}
for key, (name, top, bottom) in STARTER.items():
    defs, body = tile(top, bottom)
    write(f'{REPO}/assets/starter/{key}.svg', body + glyph(filled(name), '#ffffff', 80, 32, 32, ' filter="url(#sh)"'), defs)


# ---- sample profiles: a colour world per profile (Work: coloured chips on light tiles)
THEMES = {
    'stream': ('#2b1250', '#0e0718', '#7c3aed'),
    'work': ('#f4f5f8', '#d9dce4', None),
    'blurple': ('#6d72f6', '#4b4fd6', None),
    'green': ('#2fd16b', '#169c49', None),
    'dark': ('#2a2d36', '#15171c', '#3b82f6'),
}
CHIP = {'orange': ('#fb923c', '#ea580c'), 'blue': ('#4f8ff7', '#2563eb'), 'slate': ('#6b7385', '#3f4553'), 'teal': ('#2dd4bf', '#0f9488'),
        'purple': ('#8b5cf6', '#6d28d9'), 'red': ('#f87171', '#dc2626'), 'ink': ('#3a3f4c', '#1d2027')}


def sample(out, name, theme, chip=None, accent=None):
    top, bottom, glow = THEMES[theme]
    defs = (f'<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>'
            '<filter id="sh" x="-30%" y="-30%" width="160%" height="170%"><feDropShadow dx="0" dy="4" stdDeviation="3.5" flood-color="#000" flood-opacity=".35"/></filter>')
    body = '<rect width="144" height="144" fill="url(#bg)"/>'
    if glow:
        defs += f'<radialGradient id="gl" cx=".5" cy=".18" r=".75"><stop offset="0" stop-color="{glow}" stop-opacity=".55"/><stop offset="1" stop-color="{glow}" stop-opacity="0"/></radialGradient>'
        body += '<rect width="144" height="144" fill="url(#gl)"/>'
    if chip:
        a, b = CHIP[chip]
        defs += f'<linearGradient id="chip" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient>'
        body += '<rect x="22" y="22" width="100" height="100" rx="24" fill="url(#chip)" filter="url(#sh)"/><rect x="22" y="22" width="100" height="50" rx="24" fill="#fff" opacity=".12"/>'
        body += glyph(filled(name), '#ffffff', 60, 42, 42)
    else:
        body += glyph(filled(name), accent or '#ffffff', 84, 30, 30, ' filter="url(#sh)"')
    write(f'{REPO}/assets/templates/icons/{out}.svg', body, defs)


SAMPLES = [
    ('stream-obs', 'window', 'stream'), ('stream-record', 'record', 'stream', None, '#ff4d6a'), ('stream-live', 'signal', 'stream'),
    ('stream-scene', 'scene-collection', 'stream'), ('stream-mic', 'microphone-off', 'stream'), ('stream-screenshot', 'screenshot', 'stream'),
    ('stream-webcam', 'webcam', 'stream'), ('stream-chat', 'message', 'stream'),
    ('work-files', 'folder', 'work', 'orange'), ('work-downloads', 'download', 'work', 'blue'), ('work-calc', 'grid', 'work', 'ink'),
    ('work-editor', 'edit', 'work', 'teal'), ('work-terminal', 'terminal', 'work', 'ink'), ('work-copy', 'copy', 'work', 'slate'),
    ('work-paste', 'paste', 'work', 'slate'), ('work-lock', 'lock', 'work', 'purple'), ('work-timer', 'timer', 'work', 'red'),
    ('work-web', 'globe', 'work', 'blue'), ('work-mail', 'mail', 'work', 'blue'), ('work-screenshot', 'screenshot', 'work', 'slate'),
    ('play-mic', 'microphone-off', 'blurple'), ('play-deafen', 'headphones-off', 'blurple'), ('play-voice', 'headphones', 'blurple'),
    ('play-chat', 'message', 'blurple'), ('play-prev', 'skip-back', 'green'), ('play-play', 'play-pause', 'green'), ('play-next', 'skip-forward', 'green'),
    ('play-shuffle', 'shuffle', 'green'), ('play-volume', 'volume', 'green'), ('play-mute', 'volume-muted', 'green'), ('play-games', 'game', 'dark'),
    ('play-launcher', 'rocket', 'dark'),
]
for row in SAMPLES:
    sample(*row)


# ---- the bundled icon pack: every icon of the set (outline and filled), no brand logos
meta = open(os.path.join(PKG, 'dist', 'metadata', 'icons.g.js')).read()
names = dict(re.findall(r'^\s*"?([a-z0-9-]+)"?: \{ name: "([A-Za-z0-9]+)"', meta, re.M))
pack = f'{REPO}/assets/iconpack'
shutil.rmtree(f'{pack}/icons', ignore_errors=True)
entries = []
for file in sorted(os.listdir(SVG_DIR)):
    icon_id = file[:-4]
    if icon_id.startswith('logo-'):
        continue
    base = icon_id.replace('--filled', '')
    words = re.sub(r'([a-z])([A-Z0-9])', r'\1 \2', names.get(base, base.title().replace('-', ''))).replace(' Filled', '')
    is_filled = icon_id.endswith('--filled')
    write(f'{pack}/icons/{icon_id}.svg', glyph(icon_id, '#eceff4', 104, 20, 20))
    tags = [w for w in base.split('-') if w and not w.isdigit()] + (['filled'] if is_filled else ['outline'])
    entries.append({'path': f'{icon_id}.svg', 'name': words + (' (filled)' if is_filled else ''), 'tags': tags})
json.dump(entries, open(f'{pack}/icons.json', 'w'), indent=1)
json.dump({'Name': 'Elgato Icons', 'Author': 'Elgato (Corsair Memory Inc.)', 'Version': '2.5.1', 'Icon': 'cover.svg', 'License': 'MIT',
           'Description': 'The open icon set of the Elgato ecosystem (@elgato/icons), shipped with TuxDeck.',
           'URL': 'https://github.com/elgatosf/icons'}, open(f'{pack}/manifest.json', 'w'), indent=1)
defs, body = tile('#4f8ff7', '#2563eb')
write(f'{pack}/cover.svg', body + glyph('grid--filled' if os.path.exists(os.path.join(SVG_DIR, 'grid--filled.svg')) else 'grid', '#ffffff', 80, 32, 32), defs)
shutil.copy(os.path.join(PKG, 'LICENSE'), f'{pack}/LICENSE')
print(f'{len(ACTIONS) + 1} action icons, {len(STARTER)} starter icons, {len(SAMPLES)} sample icons, {len(entries)} pack icons')
