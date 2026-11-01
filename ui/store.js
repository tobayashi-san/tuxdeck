// Shared application state, API helper and small utilities.
import { t } from './i18n.js';

export const OPENCHILD = 'com.elgato.streamdeck.profile.openchild';
export const BACKTOPARENT = 'com.elgato.streamdeck.profile.backtoparent';
export const HOTKEY_SWITCH = 'com.elgato.streamdeck.system.hotkeyswitch';
const INFOBAR_BUILTINS = ['com.elgato.streamdeck.system.digitaltime', 'com.elgato.streamdeck.system.pagination'];

/** Fallback geometry when no (matching) device is known. */
export const MODELS = {
  '20GBJ9901': { name: 'Stream Deck Neo', kind: 'neo', cols: 4, rows: 2, touch_points: 2, screen: [248, 58], encoders: 0 },
  '20GAA9902': { name: 'Stream Deck', kind: 'original', cols: 5, rows: 3, touch_points: 0, screen: null, encoders: 0 },
  '20GBA9901': { name: 'Stream Deck MK.2', kind: 'mk2', cols: 5, rows: 3, touch_points: 0, screen: null, encoders: 0 },
  '20GAT9902': { name: 'Stream Deck XL', kind: 'xl', cols: 8, rows: 4, touch_points: 0, screen: null, encoders: 0 },
  '20GAT9901': { name: 'Stream Deck XL', kind: 'xl', cols: 8, rows: 4, touch_points: 0, screen: null, encoders: 0 },
  '20GBD9901': { name: 'Stream Deck +', kind: 'plus', cols: 4, rows: 2, touch_points: 0, screen: [800, 100], encoders: 4 },
  '20GAI9901': { name: 'Stream Deck Mini', kind: 'mini', cols: 3, rows: 2, touch_points: 0, screen: null, encoders: 0 },
  '20GAI9902': { name: 'Stream Deck Mini MK.2', kind: 'mini', cols: 3, rows: 2, touch_points: 0, screen: null, encoders: 0 },
  '20GAA9901': { name: 'Stream Deck (1st gen)', kind: 'original', cols: 5, rows: 3, touch_points: 0, screen: null, encoders: 0 },
  '20GBF9901': { name: 'Stream Deck Pedal', kind: 'pedal', cols: 3, rows: 1, touch_points: 0, screen: null, encoders: 0 },
};

/** Profiles of model `b` can be shown on a device of model `a` (same layout). */
export function compatibleModel(a, b) {
  if (a === b) return true;
  const x = MODELS[a];
  const y = MODELS[b];
  if (!x || !y || x.cols !== y.cols || x.rows !== y.rows || x.encoders !== y.encoders || x.touch_points !== y.touch_points) return false;
  const fam = (k) => (k === 'mk2' ? 'original' : k);
  return fam(x.kind) === fam(y.kind);
}

export const S = {
  state: { devices: [], profiles: [], settings: {} },
  catalogue: [],
  actionMap: new Map(), // uuid -> action (with .category)
  deviceId: localStorage.getItem('sd.device') || null,
  localProfile: localStorage.getItem('sd.profile') || null,
  localPage: null,
  navStack: [], // parent page ids when navigating into folders from the UI
  view: { profile: null, page: null, live: false, folderDepth: 0 },
  page: null, // GET /api/pages/... of the shown page
  pageCache: new Map(), // "profile/page" -> page data (for parent lookup)
  selected: null, // {controller, coord}
  selectedState: 0,
  v: Date.now() % 1000000, // render cache buster
  clipboard: null, // {profile, page, controller, coord, instance}
  drag: null, // {kind:'action', action} | {kind:'slot', controller, coord}
  wsConnected: false,
  serviceDown: false, // the event socket was lost: device states are unknown
};

export class ApiError extends Error {}

/** The page was opened without the access token (bookmark, other user): explain once. */
function showNoAccess(detail) {
  if (document.querySelector('.no-access')) return;
  document.body.append(h('div.no-access', { role: 'alert' },
    h('div.no-access-box', null,
      h('h2', null, t('No access to the TuxDeck service')),
      h('p', null, detail || t('Open the configuration from the application menu or with the command tuxdeck ui.')))));
}

export async function api(path, { method = 'GET', body, headers, raw = false } = {}) {
  const opts = { method, headers: { ...(headers || {}) } };
  if (body !== undefined) {
    if (raw) opts.body = body;
    else {
      opts.body = JSON.stringify(body);
      opts.headers['Content-Type'] = 'application/json';
    }
  }
  let res;
  try {
    res = await fetch(path, opts);
  } catch (e) {
    throw new ApiError(t('Service not reachable'));
  }
  const text = await res.text();
  let data = null;
  if (text) {
    try { data = JSON.parse(text); } catch { data = text; }
  }
  if (res.status === 401) showNoAccess((data && data.error) || '');
  if (!res.ok) {
    const msg = (data && data.error) || `${method} ${path}: HTTP ${res.status}`;
    const err = new ApiError(msg);
    err.status = res.status;
    throw err;
  }
  return data;
}

export const enc = encodeURIComponent;

export function currentDevice() {
  return S.state.devices.find((d) => d.id === S.deviceId) || null;
}

/** The selected device if it is connected and has a profile → UI follows the hardware. */
export function liveDevice() {
  const d = currentDevice();
  return d && d.connected && d.profile ? d : null;
}

export function profileById(id) {
  return S.state.profiles.find((p) => p.id === id) || null;
}

export function modelName(model) {
  return MODELS[model]?.name || model;
}

/** A deck's name; two decks with the same name get the end of their id. */
export function deckLabel(d) {
  const name = d.name || modelName(d.model);
  const twin = (S.state?.devices || []).some((x) => x.id !== d.id && (x.name || modelName(x.model)) === name);
  return twin ? `${name} · …${String(d.id).slice(-4)}` : name;
}

export function geometryFor(profile) {
  const d = currentDevice();
  const model = profile?.model;
  if (d && (!model || compatibleModel(d.model, model))) {
    return {
      cols: d.cols || 5, rows: d.rows || 3, touch_points: d.touch_points || 0,
      screen: d.screen || null, encoders: d.encoders || 0, kind: d.kind || '',
    };
  }
  return MODELS[model] || { cols: 5, rows: 3, touch_points: 0, screen: null, encoders: 0, kind: '' };
}

export function isNeoGeometry(g) {
  return g.kind === 'neo' || g.touch_points > 0 || (!!g.screen && !g.encoders);
}

export function instanceAt(controller, coord, page = S.page) {
  const c = page?.controllers?.find((c) => c.Type === controller);
  return c?.Actions?.[coord] || null;
}

export function setInstanceAt(controller, coord, inst) {
  if (!S.page) return;
  S.page.controllers = S.page.controllers || [];
  let c = S.page.controllers.find((c) => c.Type === controller);
  if (!c) S.page.controllers.push((c = { Type: controller, Actions: {} }));
  c.Actions = c.Actions || {};
  if (inst) c.Actions[coord] = inst;
  else delete c.Actions[coord];
}

export function actionInfo(uuid) {
  return S.actionMap.get(uuid) || null;
}

export function controllersOf(action) {
  return action?.controllers?.length ? action.controllers : ['Keypad'];
}

/** Can an action (catalogue entry) be placed onto the given slot? */
export function slotAccepts(controller, coord, action) {
  const ctl = controllersOf(action);
  if (controller === 'Neo') {
    if (coord === '1,0') return ctl.includes('Neo');
    return ctl.includes('Keypad');
  }
  return ctl.includes(controller);
}

/** Can an existing instance (by UUID) live in that slot? Unknown actions: same controller only. */
export function instanceAccepts(controller, coord, inst, fromController) {
  if (!inst) return true;
  const a = actionInfo(inst.UUID);
  if (a) return slotAccepts(controller, coord, a);
  return controller === fromController;
}

/**
 * What the hardware shows of an action's image and title in that slot. Neo touch points are
 * lit buttons without a display; the info bar draws Digital Time / Pagination itself and shows
 * plugin actions only as plain title text (fixed style).
 */
export function slotAppearance(controller, coord, inst) {
  // The Pedal has no displays.
  if (MODELS[profileById(S.view.profile)?.model]?.kind === 'pedal') return { image: false, title: false, titleStyle: false };
  if (controller !== 'Neo') return { image: true, title: true, titleStyle: true };
  const title = coord === '1,0' && !INFOBAR_BUILTINS.includes(inst?.UUID);
  return { image: false, title, titleStyle: false };
}

export function pagePath(profile = S.view.profile, page = S.view.page) {
  return `/api/pages/${enc(profile)}/${enc(page)}`;
}

export function renderUrl(controller, coord, size = 144) {
  return `/api/render/${enc(S.view.profile)}/${enc(S.view.page)}/${enc(controller)}/${enc(coord)}.png?size=${size}&v=${S.v}`;
}

export function slotKey(controller, coord) {
  return `${controller}/${coord}`;
}

export function debounce(fn, ms) {
  let t = null;
  const d = (...args) => {
    clearTimeout(t);
    t = setTimeout(() => { t = null; fn(...args); }, ms);
  };
  d.flush = (...args) => {
    if (t) { clearTimeout(t); t = null; fn(...args); }
  };
  d.cancel = () => { clearTimeout(t); t = null; };
  return d;
}

/** Tiny DOM helper: h('div.cls#id', {attrs/on*}, ...children) */
export function h(tag, props, ...children) {
  const m = tag.match(/^([a-z0-9-]+)?((?:[.#][\w-]+)*)$/i);
  const el = document.createElement(m[1] || 'div');
  for (const part of (m[2] || '').match(/[.#][\w-]+/g) || []) {
    if (part[0] === '.') el.classList.add(part.slice(1));
    else el.id = part.slice(1);
  }
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
      else if (k === 'class') el.className += (el.className ? ' ' : '') + v;
      else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
      else if (k === 'dataset') Object.assign(el.dataset, v);
      else if (k in el && typeof v !== 'string') el[k] = v;
      else if (k === 'value' || k === 'checked' || k === 'disabled' || k === 'selected') el[k] = v;
      else el.setAttribute(k, v === true ? '' : v);
    }
  }
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

export function uuidv4() {
  if (crypto.randomUUID) return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const x = [...b].map((v) => v.toString(16).padStart(2, '0')).join('');
  return `${x.slice(0, 8)}-${x.slice(8, 12)}-${x.slice(12, 16)}-${x.slice(16, 20)}-${x.slice(20)}`;
}

export function slotLabel(controller, coord) {
  const [c, r] = coord.split(',').map(Number);
  if (controller === 'Neo') return [t('Left touch point'), t('Info bar'), t('Right touch point')][c] || `Neo ${coord}`;
  if (controller === 'Encoder') return t('Dial {n}', { n: c + 1 });
  return t('Key {row}/{col}', { row: r + 1, col: c + 1 });
}
