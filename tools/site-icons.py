# The icon sprite of the project website (site/icons.svg), from Elgato's open icon set
# @elgato/icons (MIT): `python3 tools/site-icons.py <npm package dir>`.
import os, re, sys

PKG = sys.argv[1] if len(sys.argv) > 1 else 'package'
REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..')
ICONS = ['download', 'grid', 'stream-deck-mk2', 'code', 'plugin', 'layers', 'volume', 'volume-muted', 'starbursts',
         'chevron-left', 'chevron-right', 'stream-deck-neo', 'folder-download', 'hotkey', 'infobar', 'palette', 'window',
         'shield', 'save', 'keyboard', 'terminal', 'copy', 'checkmark', 'multi-action', 'timer', 'heart', 'question-mark',
         'stream-deck-plus', 'stream-deck-pedal', 'stream-deck-mini', 'external-link', 'sync']
out = []
for name in ICONS:
    src = open(os.path.join(PKG, 'svg', 'l', f'{name}.svg')).read()
    inner = re.search(r'<svg[^>]*>(.*)</svg>', src, re.S).group(1)
    out.append(f'<symbol id="{name}" viewBox="0 0 24 24" fill="currentColor">{inner}</symbol>')
open(os.path.join(REPO, 'site', 'icons.svg'), 'w').write(
    '<svg xmlns="http://www.w3.org/2000/svg"><!-- Elgato Icons (@elgato/icons), MIT, (c) Corsair Memory Inc. -->' + ''.join(out) + '</svg>\n')
print(len(ICONS), 'icons')
