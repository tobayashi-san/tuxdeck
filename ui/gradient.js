// Key background gradients: CSS `linear-gradient(…)` / `radial-gradient(…)` strings (the
// format of cssgradient.io), edited in a small dialog. The service draws the same string
// (render::css_paint).

import { h, icon } from './store.js';
import { modal } from './dialogs.js';
import { t } from './i18n.js';

export const GRADIENT_PRESETS = [
  'linear-gradient(135deg, #3b82f6 0%, #8b5cf6 100%)',
  'linear-gradient(135deg, #f97316 0%, #e11d48 100%)',
  'linear-gradient(135deg, #10b981 0%, #0ea5e9 100%)',
  'linear-gradient(135deg, #f59e0b 0%, #ef4444 100%)',
  'linear-gradient(135deg, #6366f1 0%, #ec4899 100%)',
  'linear-gradient(180deg, #334155 0%, #0f172a 100%)',
  'linear-gradient(135deg, #22d3ee 0%, #a3e635 100%)',
  'radial-gradient(circle, #4f46e5 0%, #111827 100%)',
];

export const isGradient = (v) => /^\s*(linear|radial)-gradient\(/i.test(v || '');

/** Split at top-level commas (not inside rgba(…)). */
function splitTop(s) {
  const out = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '(') depth++;
    else if (s[i] === ')') depth--;
    else if (s[i] === ',' && depth === 0) {
      out.push(s.slice(start, i).trim());
      start = i + 1;
    }
  }
  out.push(s.slice(start).trim());
  return out;
}

const SIDES = {
  'to top': 0, 'to right': 90, 'to bottom': 180, 'to left': 270, 'to top right': 45, 'to right top': 45,
  'to bottom right': 135, 'to right bottom': 135, 'to bottom left': 225, 'to left bottom': 225, 'to top left': 315, 'to left top': 315,
};

/** {type, angle, stops: [{color, pos}]} of a CSS gradient, or null. */
export function parseGradient(css) {
  let c = String(css || '').trim().replace(/;$/, '').replace(/^background(-image)?:\s*/i, '');
  const m = /^(linear|radial)-gradient\((.*)\)$/is.exec(c);
  if (!m) return null;
  const type = m[1].toLowerCase();
  const parts = splitTop(m[2]);
  let angle = 180;
  const first = parts[0].toLowerCase();
  if (type === 'linear') {
    const deg = /^(-?[\d.]+)deg$/.exec(first);
    if (deg) { angle = Number(deg[1]); parts.shift(); } else if (first in SIDES) { angle = SIDES[first]; parts.shift(); }
  } else if (/^(circle|ellipse|closest|farthest|at )/.test(first)) {
    parts.shift();
  }
  if (parts.length < 2 || parts.length > 16) return null;
  const stops = parts.map((p, i) => {
    const s = /^(.*\S)\s+(-?[\d.]+)%$/.exec(p);
    return s ? { color: s[1], pos: Number(s[2]) } : { color: p, pos: Math.round((i / (parts.length - 1)) * 100) };
  });
  // Only colours (a profile could bring e.g. url(…) here, which the browser would load).
  if (!Number.isFinite(angle) || !stops.every((x) => isColor(x.color) && Number.isFinite(x.pos))) return null;
  return { type, angle: ((angle % 360) + 360) % 360, stops };
}

/** A CSS colour: #hex, rgb()/rgba()/hsl()/hsla() with numbers, or a colour name. */
function isColor(c) {
  return /^#[0-9a-f]{3,8}$/i.test(c) || /^(rgba?|hsla?)\(\s*[-\d.%\s,/deg]+\)$/i.test(c) || /^[a-z]{3,20}$/i.test(c);
}

/** A gradient TuxDeck understands, written anew from its parts (for CSS); else null. */
export function safeGradient(v) {
  const g = isGradient(v) ? parseGradient(v) : null;
  return g ? gradientCss(g) : null;
}

export function gradientCss(g) {
  const stops = [...g.stops].sort((a, b) => a.pos - b.pos).map((s) => `${s.color} ${Math.round(s.pos)}%`).join(', ');
  return g.type === 'radial' ? `radial-gradient(circle, ${stops})` : `linear-gradient(${Math.round(g.angle)}deg, ${stops})`;
}

/** #rrggbb for a colour input (rgb()/rgba() and short hex are converted; else black). */
function hex(color) {
  const c = String(color).trim();
  if (/^#[0-9a-f]{6}$/i.test(c)) return c.toLowerCase();
  if (/^#[0-9a-f]{3}$/i.test(c)) return '#' + [...c.slice(1)].map((x) => x + x).join('').toLowerCase();
  const m = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/i.exec(c);
  return m ? '#' + m.slice(1, 4).map((x) => Math.min(255, Number(x)).toString(16).padStart(2, '0')).join('') : '#000000';
}

/** Edit a gradient; resolves with the CSS string, or null when cancelled. */
export async function editGradient(current) {
  const g = parseGradient(current) || parseGradient(GRADIENT_PRESETS[0]);
  const preview = h('div.grad-preview');
  const css = h('input.text.grad-css', { type: 'text', spellcheck: 'false', 'aria-label': t('CSS') });
  const note = h('p.field-help');
  const stopsBox = h('div.grad-stops');
  const typeSel = h('select.select', { 'aria-label': t('Type') },
    h('option', { value: 'linear' }, t('Linear')), h('option', { value: 'radial' }, t('Radial')));
  const angle = h('input', { type: 'range', min: 0, max: 359, step: 1, 'aria-label': t('Angle') });
  const angleVal = h('span.tb-value');
  const angleRow = h('label.modal-field', null, h('span', null, t('Angle')), h('div.grad-angle', null, angle, angleVal));

  // Preview, CSS text and angle; the colour rows are only rebuilt when colours are added or
  // removed (rebuilding them while a colour picker is open would close the picker).
  const refresh = (fromCss = false) => {
    const text = gradientCss(g);
    preview.style.background = text;
    if (!fromCss) css.value = text;
    typeSel.value = g.type;
    angle.value = g.angle;
    angleVal.textContent = `${Math.round(g.angle)}°`;
    angleRow.hidden = g.type !== 'linear';
  };
  const update = (fromCss = false) => {
    refresh(fromCss);
    stopsBox.replaceChildren(...g.stops.map((s, i) => {
      const pos = h('span.tb-value', null, `${Math.round(s.pos)}%`);
      return h('div.grad-stop', null,
        h('input.color', { type: 'color', value: hex(s.color), 'aria-label': t('Colour {n}', { n: i + 1 }),
          oninput: (e) => { s.color = e.target.value; refresh(); } }),
        h('input', { type: 'range', min: 0, max: 100, step: 1, value: s.pos, 'aria-label': t('Position {n}', { n: i + 1 }),
          oninput: (e) => { s.pos = Number(e.target.value); pos.textContent = `${Math.round(s.pos)}%`; refresh(); } }),
        pos,
        h('button.icon-btn', { type: 'button', title: t('Remove colour'), disabled: g.stops.length <= 2,
          onclick: () => { g.stops.splice(i, 1); update(); } }, icon('close')));
    }),
    g.stops.length < 6 ? h('button.btn.small', { type: 'button', onclick: () => { g.stops.push({ color: g.stops.at(-1).color, pos: 100 }); update(); } }, '+ ' + t('Colour')) : null);
  };
  typeSel.addEventListener('change', () => { g.type = typeSel.value; refresh(); });
  angle.addEventListener('input', () => { g.angle = Number(angle.value); refresh(); });
  css.addEventListener('input', () => {
    const p = parseGradient(css.value);
    note.textContent = p ? '' : t('Not a gradient TuxDeck understands (linear-gradient or radial-gradient).');
    if (p) { Object.assign(g, p); update(true); }
  });
  update();
  const body = h('div.grad-editor', null,
    preview,
    h('div.grad-controls', null,
      h('label.modal-field', null, h('span', null, t('Type')), typeSel),
      angleRow,
      h('div.modal-field', null, h('span', null, t('Colours')), stopsBox)),
    h('label.modal-field.grad-css-row', null, h('span', null, t('CSS (e.g. from cssgradient.io)')), css),
    note);
  const r = await modal(t('Background gradient'), body, [
    { label: t('Cancel'), value: 'cancel' },
    { label: t('Apply'), value: 'ok', primary: true },
  ]);
  return r === 'ok' ? gradientCss(g) : null;
}
