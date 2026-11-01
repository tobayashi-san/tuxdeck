// Bottom panel: property editor for the selected slot (title, image, settings / property inspector).
import {
  S, api, enc, h, debounce, instanceAt, actionInfo, renderUrl, pagePath, profileById,
  slotLabel, modelName, uuidv4, compatibleModel, slotAppearance, HOTKEY_SWITCH,
} from './store.js';
import { toast } from './dialogs.js';
import { pickIcon } from './iconpacks.js';
import { t, tn } from './i18n.js';
import {
  makeHotkey, unusedHotkey, isUnusedHotkey, hotkeyLabel, MODIFIER_CODES,
} from './keycodes.js';

let root = null;
let hooks = {};
let cur = { key: null, teardown: [] };

/**
 * hooks: {press(sel), remove(sel), changed()}
 */
export function initEditor(el, h) {
  root = el;
  hooks = h;
}

function editorKey() {
  const sel = S.selected;
  if (!sel || !S.view.page) return 'none';
  const inst = instanceAt(sel.controller, sel.coord);
  return [S.view.profile, S.view.page, sel.controller, sel.coord, inst?.UUID ?? '', inst?.ActionID ?? '', S.selectedState].join('|');
}

function teardown() {
  for (const fn of cur.teardown) {
    try { fn(); } catch (e) { console.error(e); }
  }
  cur.teardown = [];
}

/** Rebuild the editor if the selection/instance changed; otherwise only refresh the preview. */
export function renderEditor(force = false) {
  if (!root) return;
  const key = editorKey();
  if (key === cur.key && !force) {
    refreshEditorPreview();
    updateEditorChrome();
    return;
  }
  teardown();
  cur.key = key;
  cur.target = null;
  cur.setImage = null;
  root.replaceChildren();
  const sel = S.selected;
  if (!sel || !S.view.page) {
    root.append(h('div.editor-empty', null,
      h('div.editor-empty-icon', null, '⌨'),
      h('div', null, t('Select a key to edit it.')),
      h('div.muted', null, t('Drag actions from the list on the right onto a key.'))));
    return;
  }
  const inst = instanceAt(sel.controller, sel.coord);
  if (!inst) {
    root.append(h('div.editor-empty', null,
      h('div.editor-empty-title', null, slotLabel(sel.controller, sel.coord)),
      h('div', null, t('This spot is empty.')),
      h('div.muted', null, t('Drag an action from the list on the right here (or double-click an action).'))));
    return;
  }
  buildEditor(sel, inst);
}

/** Preview of the slot in the state that is being edited. */
function previewUrl(sel, size) {
  return `${renderUrl(sel.controller, sel.coord, size)}&state=${S.selectedState || 0}`;
}

export function refreshEditorPreview() {
  const img = root?.querySelector('.ed-preview img');
  const sel = S.selected;
  if (img && sel) img.src = previewUrl(sel, sel.controller === 'Neo' && sel.coord === '1,0' ? 248 : 144);
}

/** Flush pending (debounced) writes, e.g. before page navigation. */
export function flushEditor() {
  for (const fn of cur.flushers || []) fn();
}

/** "Test press" needs a device that shows this page. */
export function canTestPress() {
  return !!S.view.live && !S.serviceDown;
}

/** Enable/disable what depends on the connection without rebuilding the editor. */
function updateEditorChrome() {
  const press = root?.querySelector('.ed-press');
  if (press) {
    press.disabled = !canTestPress();
    press.title = press.disabled ? t('Connect the device to test the action') : t('Trigger the key on the device');
  }
  const badge = root?.querySelector('.ed-unsaved');
  if (badge) badge.hidden = !unsaved.some((w) => sameTarget(w.target, cur.target));
}

// ---- writes ----------------------------------------------------------------------------------
// Slot changes that failed because the service was not reachable. They are kept (in order) and
// sent again when the connection is back (retryUnsaved), so nothing typed meanwhile is lost.
const unsaved = [];
let retrying = null;

const sameTarget = (a, b) => !!a && !!b && a.profile === b.profile && a.page === b.page
  && a.controller === b.controller && a.coord === b.coord;

async function writeSlot(target, body) {
  const w = {
    target, path: pagePath(target.profile, target.page) + '/slot',
    body: { controller: target.controller, coord: target.coord, ...body },
  };
  // Older unsaved changes go first, otherwise they would overwrite this one later.
  if (unsaved.length) {
    unsaved.push(w);
    updateEditorChrome();
    return retryUnsaved();
  }
  try {
    await api(w.path, { method: 'PATCH', body: w.body });
    hooks.changed?.(target);
  } catch (e) {
    if (e.status) {
      toast(e.message, 'error');
      return;
    }
    unsaved.push(w);
    updateEditorChrome();
    toast(t('The service is not reachable. The change is kept and saved as soon as the connection is back.'), 'warn', 6000);
  }
}

/** Send the kept changes; stops at the first one that still cannot be delivered. */
export function retryUnsaved() {
  if (!retrying) {
    retrying = (async () => {
      // Continue only after `retrying` is assigned: with nothing kept the function would
      // otherwise finish synchronously, clear `retrying` before the assignment and leave it
      // set to a settled promise, so that later kept changes were never sent again.
      await null;
      let saved = 0;
      const changed = [];
      while (unsaved.length) {
        const w = unsaved[0];
        try {
          await api(w.path, { method: 'PATCH', body: w.body });
          saved++;
          if (!changed.some((x) => sameTarget(x, w.target))) changed.push(w.target);
        } catch (e) {
          if (!e.status) break; // still not reachable: next reconnect
          toast(e.message, 'error');
        }
        unsaved.shift();
      }
      retrying = null;
      updateEditorChrome();
      for (const target of changed) hooks.changed?.(target);
      if (saved && !unsaved.length) toast(tn(saved, 'Saved {n} kept change', 'Saved {n} kept changes'), 'info', 3000);
    })();
  }
  return retrying;
}

addEventListener('beforeunload', (e) => {
  if (unsaved.length) e.preventDefault();
});

/** Image file dropped onto a key or the preview: set it like "Image…" (selected slot). */
export async function dropImage(file) {
  try {
    await cur.setImage?.(file);
  } catch (e) {
    toast(e.message || String(e), 'error');
  }
}

function buildEditor(sel, inst) {
  const target = { profile: S.view.profile, page: S.view.page, controller: sel.controller, coord: sel.coord };
  const action = actionInfo(inst.UUID);
  const states = Array.isArray(inst.States) && inst.States.length ? inst.States : [{}];
  if (S.selectedState >= states.length) S.selectedState = 0;
  const stateIdx = S.selectedState;
  const myKey = cur.key;

  // ---- write helpers (bound to this target) -------------------------------------------
  cur.target = target;
  const patch = (body) => writeSlot(target, body);
  let pendingStyle = {};
  const flushStyle = () => {
    if (!Object.keys(pendingStyle).length) return;
    const style = pendingStyle;
    pendingStyle = {};
    patch({ state: stateIdx, style });
  };
  const styleDebounced = debounce(flushStyle, 300);
  const setStyle = (p, immediate) => {
    inst.States = inst.States || [{}];
    while (inst.States.length <= stateIdx) inst.States.push({});
    const st = inst.States[stateIdx];
    for (const [k, v] of Object.entries(p)) {
      if (v === null) delete st[k];
      else st[k] = v;
    }
    Object.assign(pendingStyle, p);
    if (immediate) { styleDebounced.cancel(); flushStyle(); } else styleDebounced();
  };
  // Send only what changed since the last write: another tab (or the plugin) may have
  // changed other settings in the meantime.
  let sentSettings = structuredClone(inst.Settings || {});
  const settingsDebounced = debounce((settings) => {
    const merge = {};
    for (const [k, v] of Object.entries(settings)) {
      if (JSON.stringify(v) !== JSON.stringify(sentSettings[k])) merge[k] = v;
    }
    const remove = Object.keys(sentSettings).filter((k) => !(k in settings));
    sentSettings = settings;
    if (Object.keys(merge).length || remove.length) patch({ settings_merge: merge, settings_remove: remove });
  }, 300);
  const setSettings = (settings) => {
    inst.Settings = settings;
    settingsDebounced(structuredClone(settings));
  };
  cur.flushers = [() => { styleDebounced.cancel(); flushStyle(); }, () => settingsDebounced.flush()];
  cur.teardown.push(...cur.flushers);

  // ---- header ------------------------------------------------------------------------
  const name = action?.name || inst.Name || inst.UUID || t('Action');
  const header = h('div.ed-header', null,
    action?.icon ? h('img.ed-icon', { src: action.icon, alt: '', draggable: false }) : h('div.ed-icon.placeholder'),
    h('div.ed-title', null,
      h('div.ed-name', null, name),
      h('div.ed-sub', null,
        [action?.category, slotLabel(sel.controller, sel.coord)].filter(Boolean).join(' · '),
        action && action.supported === false ? h('span.badge.warn', null, t('Not supported on Linux')) : null,
        !action ? h('span.badge.warn', null, t('Plugin not installed')) : null)),
    states.length > 1 ? h('div.state-tabs', { role: 'tablist' }, states.map((_, i) =>
      h('button.state-tab', {
        role: 'tab', class: i === stateIdx ? 'active' : null,
        'aria-selected': String(i === stateIdx),
        onclick: () => { S.selectedState = i; renderEditor(); },
      }, t('State {n}', { n: i + 1 })))) : null,
    h('div.spacer'),
    h('span.badge.warn.ed-unsaved', { hidden: true, title: t('The service is not reachable. The change is saved as soon as the connection is back.') }, t('Not saved yet')),
    h('button.btn.ed-press', { onclick: () => hooks.press?.(sel) }, '▶ ' + t('Test press')),
    h('button.btn.danger-outline', { title: t('Remove the action (Del)'), onclick: () => hooks.remove?.(sel) }, t('Delete')));

  // ---- image column --------------------------------------------------------------------
  const isBar = sel.controller === 'Neo' && sel.coord === '1,0';
  const isTouch = sel.controller === 'Neo' && !isBar;
  const looks = slotAppearance(sel.controller, sel.coord, inst);
  const setImage = async (f) => patch({ state: stateIdx, image: await imageFileToDataUrl(f) });
  cur.setImage = looks.image ? setImage : null;
  const fileInput = h('input', {
    type: 'file', hidden: true,
    accept: '.png,.jpg,.jpeg,.gif,.svg,.webp,image/png,image/jpeg,image/gif,image/svg+xml,image/webp',
    onchange: async () => {
      const f = fileInput.files[0];
      fileInput.value = '';
      if (!f) return;
      try {
        await setImage(f);
      } catch (e) {
        toast(e.message || String(e), 'error');
      }
    },
  });
  const preview = h('img', { src: previewUrl(sel, isBar ? 248 : 144), alt: '', draggable: false });
  let imgNote = null;
  if (isTouch) imgNote = t('Touch points have no display: they light up and trigger the action.');
  else if (isBar && !looks.title) imgNote = t('The info bar draws this action itself; choose its look in the settings.');
  else if (isBar) imgNote = t('The info bar shows the title as text (or what the plugin draws), no image.');
  const imgCol = !looks.image ? h('div.ed-col.ed-imagecol', null,
    h('div.ed-preview.static', { class: isBar ? 'bar' : isTouch ? 'touch' : null }, preview),
    imgNote ? h('div.field-help.ed-note', null, imgNote) : null) : h('div.ed-col.ed-imagecol', null,
    h('div.ed-preview', {
      class: isBar ? 'bar' : isTouch ? 'touch' : null, title: t('Click to choose your own image (or drop an image file here)'),
      dataset: { hint: t('Change image') },
      onclick: () => fileInput.click(),
    }, preview),
    h('div.ed-imgbtns', null,
      h('button.btn.small', { onclick: () => fileInput.click() }, t('Image…')),
      h('button.btn.small', {
        title: t('Choose an icon from an installed icon pack'),
        onclick: async () => {
          try {
            const url = await pickIcon(imageFileToDataUrl);
            if (url) await patch({ state: stateIdx, image: url });
          } catch (e) {
            toast(e.message || String(e), 'error');
          }
        },
      }, t('Icon…')),
      h('button.btn.small', { title: t('Use the default image of the action'), onclick: () => patch({ state: stateIdx, image: '' }) }, t('Default'))),
    sel.controller === 'Keypad' ? backgroundRow(states[stateIdx] || {}, setStyle) : null,
    fileInput);

  // ---- title column --------------------------------------------------------------------
  const st = states[stateIdx] || {};
  const titleArea = h('textarea.text', {
    rows: 3, spellcheck: 'false', placeholder: t('Title'), value: st.Title ?? '',
    oninput: () => setStyle({ Title: titleArea.value }, false),
    onblur: () => { styleDebounced.cancel(); flushStyle(); },
  });
  const sizes = [];
  for (let s = 6; s <= 32; s++) sizes.push(s);
  const fontSize = Number(st.FontSize ?? 12);
  if (!sizes.includes(fontSize)) sizes.push(fontSize), sizes.sort((a, b) => a - b);
  const showTitle = st.ShowTitle ?? true;
  const titleCol = !looks.title ? null : !looks.titleStyle ? h('div.ed-col.ed-titlecol', null,
    h('label.field', null, h('span.field-label', null, t('Title')), titleArea)) : h('div.ed-col.ed-titlecol', null,
    h('label.field', null, h('span.field-label', null, t('Title')), titleArea),
    h('div.title-tools', null,
      h('label.check', { title: t('Show title') },
        h('input', { type: 'checkbox', checked: showTitle, onchange: (e) => setStyle({ ShowTitle: e.target.checked }, true) }),
        t('Show title')),
      h('input.color', {
        type: 'color', title: t('Title color'), value: normColor(st.TitleColor, '#ffffff'),
        onchange: (e) => setStyle({ TitleColor: e.target.value }, true),
      })),
    h('div.title-tools', null,
      h('select.select', {
        title: t('Alignment'), onchange: (e) => setStyle({ TitleAlignment: e.target.value }, true),
      }, [['top', t('Top')], ['middle', t('Middle')], ['bottom', t('Bottom')]].map(([v, l]) =>
        h('option', { value: v, selected: (st.TitleAlignment || 'bottom') === v }, l))),
      h('select.select.narrow', {
        title: t('Font size'), onchange: (e) => setStyle({ FontSize: Number(e.target.value) }, true),
      }, sizes.map((s) => h('option', { value: String(s), selected: s === fontSize }, `${s} pt`)))),
    h('div.title-tools', null,
      h('select.select', {
        title: t('Font style'), onchange: (e) => setStyle({ FontStyle: e.target.value }, true),
      }, [['', t('Regular')], ['Bold', t('Bold')], ['Italic', t('Italic')], ['Bold Italic', t('Bold italic')]].map(([v, l]) =>
        h('option', { value: v, selected: (st.FontStyle || '') === v || (v === '' && st.FontStyle === 'Regular') }, l))),
      h('label.toggle-btn', { title: t('Underline') },
        h('input', { type: 'checkbox', checked: !!st.FontUnderline, onchange: (e) => setStyle({ FontUnderline: e.target.checked }, true) }),
        h('span', { style: { textDecoration: 'underline' } }, 'U'))));

  // ---- settings column -----------------------------------------------------------------
  const settingsBox = h('div.ed-settings');
  const settingsCol = h('div.ed-col.ed-settingscol', null, settingsBox);
  const form = formFields(action);
  if (action?.pi) {
    buildPI(settingsBox, target, myKey);
  } else if (form.length) {
    const settings = structuredClone(inst.Settings || {});
    const rebuild = () => {
      settingsBox.replaceChildren(h('div.form', null,
        form.map((f) => buildField(f, settings, () => setSettings(settings), rebuild, form))));
    };
    rebuild();
  } else {
    settingsBox.append(h('div.muted.center', null, action ? t('This action has no settings.') : t('No settings available.')));
  }

  root.append(header, h('div.ed-body', { class: titleCol ? null : 'no-title' }, imgCol, titleCol, settingsCol));
  updateEditorChrome();
}

/** Form fields of an action; the hotkey switch always shows its two state rows. */
function formFields(action) {
  const form = action?.form || [];
  return action?.uuid === HOTKEY_SWITCH ? form.map((f) => (f.type === 'hotkey' ? { ...f, rows: 2 } : f)) : form;
}

/** Key background colour: preset swatches, a custom picker and "none" (black). */
function backgroundRow(st, setStyle) {
  const presets = ['#1e1e1e', '#2563eb', '#059669', '#e11d48', '#d97706', '#7c3aed', '#0d9488', '#475569'];
  const cur = (st.Background || '').toLowerCase();
  return h('div.ed-bg', null,
    h('span.field-label', null, t('Background')),
    h('div.bg-swatches', null,
      h('button.bg-swatch.bg-none', { class: cur ? null : 'active', title: t('No background (black)'), onclick: () => setStyle({ Background: null }, true) }),
      presets.map((c) => h('button.bg-swatch', {
        class: cur === c ? 'active' : null, style: { background: c }, title: c,
        onclick: () => setStyle({ Background: c }, true),
      })),
      h('input.color.bg-custom', {
        type: 'color', title: t('Custom colour'), value: normColor(st.Background, '#1e1e1e'),
        oninput: (e) => setStyle({ Background: e.target.value }, true),
      })));
}

function normColor(c, fallback) {
  if (typeof c !== 'string') return fallback;
  if (/^#[0-9a-f]{6}$/i.test(c)) return c.toLowerCase();
  if (/^#[0-9a-f]{3}$/i.test(c)) return '#' + [...c.slice(1)].map((x) => x + x).join('').toLowerCase();
  return fallback;
}

// ---- image file → data URL (raster images downscaled to max 288 px) --------------------------
function readAsDataURL(file) {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(r.result);
    r.onerror = () => reject(new Error(t('Could not read the file')));
    r.readAsDataURL(file);
  });
}

function loadImage(url) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error(t('Could not load the image')));
    img.src = url;
  });
}

const EXT_MIME = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp' };

export async function imageFileToDataUrl(file) {
  const ext = (file.name.split('.').pop() || '').toLowerCase();
  const mime = Object.values(EXT_MIME).includes(file.type) ? file.type : EXT_MIME[ext];
  if (!mime) throw new Error(t('Unsupported image format (PNG, JPG, GIF, SVG or WebP)'));
  let url = await readAsDataURL(file);
  // Ensure correct MIME prefix (some systems report an empty type)
  url = url.replace(/^data:[^;,]*/, 'data:' + mime);
  if (mime === 'image/svg+xml') return url;
  const img = await loadImage(url);
  const w = img.naturalWidth, hgt = img.naturalHeight;
  const max = Math.max(w, hgt);
  if (max <= 288) return url;
  const scale = 288 / max;
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(w * scale));
  canvas.height = Math.max(1, Math.round(hgt * scale));
  const ctx = canvas.getContext('2d');
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL('image/png');
}

// ---- property inspector (third-party plugins) ------------------------------------------------
async function buildPI(box, target, myKey) {
  box.replaceChildren(h('div.muted.center', null, t('Loading properties…')));
  let info;
  try {
    info = await api(`/api/pi/${enc(target.profile)}/${enc(target.page)}?controller=${enc(target.controller)}&coord=${enc(target.coord)}`);
  } catch (e) {
    if (cur.key !== myKey) return;
    box.replaceChildren(h('div.muted.center', null, e.status === 404 ? t('This action has no settings.') : t('Could not load the properties: {msg}', { msg: e.message })));
    return;
  }
  if (cur.key !== myKey) return;
  // The inspector runs on the plugin file origin (no access to the UI or its API) and is
  // reached through the bridge script the server adds to its page (src/pi-bridge.js).
  const frame = h('iframe.pi-frame', {
    src: info.url,
    title: t('Properties (Property Inspector)'),
    sandbox: 'allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads',
  });
  const origin = new URL(info.url).origin;
  const onMessage = (e) => {
    if (e.source !== frame.contentWindow || e.origin !== origin) return;
    const m = e.data || {};
    if (m.tuxdeck === 'height' && Number.isFinite(m.px)) {
      frame.style.height = Math.min(Math.max(120, Math.round(m.px)), 20000) + 'px';
    } else if (m.tuxdeck === 'connected') {
      frame.dataset.state = 'connected';
    } else if (m.tuxdeck === 'missing') {
      frame.dataset.state = 'missing';
      console.info('Property Inspector without connectElgatoStreamDeckSocket', info.url);
    } else if (m.tuxdeck === 'error') {
      frame.dataset.state = 'error';
      console.error('Property Inspector error', m.message);
      toast(t('Could not connect the property inspector: {msg}', { msg: String(m.message || '') }), 'warn');
    }
  };
  window.addEventListener('message', onMessage);
  frame.addEventListener('load', () => {
    frame.contentWindow?.postMessage({
      tuxdeck: 'connect',
      args: [info.port, info.uuid, info.registerEvent, JSON.stringify(info.info), JSON.stringify(info.actionInfo)],
    }, origin);
  });
  cur.teardown.push(() => {
    window.removeEventListener('message', onMessage);
    frame.src = 'about:blank';
    frame.remove();
  });
  box.replaceChildren(frame);
}

// ---- schema form -----------------------------------------------------------------------------
function getPath(obj, path) {
  let o = obj;
  for (const k of path.split('.')) {
    if (o == null || typeof o !== 'object') return undefined;
    o = o[k];
  }
  return o;
}

function setPath(obj, path, value) {
  const keys = path.split('.');
  let o = obj;
  for (let i = 0; i < keys.length - 1; i++) {
    if (o[keys[i]] == null || typeof o[keys[i]] !== 'object') o[keys[i]] = {};
    o = o[keys[i]];
  }
  const last = keys[keys.length - 1];
  if (value === undefined) delete o[last];
  else o[last] = value;
}

let fieldSeq = 0;

/** Options of a select field; `options_by` narrows them by the value of `depends_on`. */
function selectOptions(f, settings, allFields) {
  if (f.depends_on && f.options_by) {
    const parent = getPath(settings, f.depends_on) ?? (allFields || []).find((x) => x.key === f.depends_on)?.default;
    const list = f.options_by[parent];
    if (Array.isArray(list)) return list;
  }
  return f.options || [];
}

function buildField(f, settings, changed, rebuild, allFields) {
  const id = 'f' + (++fieldSeq);
  const stored = getPath(settings, f.key);
  const value = stored === undefined ? f.default : stored;
  const set = (v) => { setPath(settings, f.key, v); changed(); };
  let control;
  let wide = false;

  switch (f.type) {
    case 'textarea':
      wide = true;
      control = h('textarea.text', {
        id, rows: 3, placeholder: f.placeholder || '', value: value ?? '', spellcheck: 'false',
        oninput: (e) => set(e.target.value),
      });
      break;
    case 'number':
      control = h('input.text', {
        id, type: 'number', min: f.min ?? null, max: f.max ?? null, step: f.step ?? 'any',
        placeholder: f.placeholder || '', value: value ?? '',
        oninput: (e) => {
          const v = e.target.value;
          if (v === '') set(undefined);
          else if (!Number.isNaN(Number(v))) set(Number(v));
        },
      });
      for (const k of ['min', 'max', 'step']) if (f[k] != null) control.setAttribute(k, f[k]);
      break;
    case 'checkbox':
      control = h('label.check', null,
        h('input', { id, type: 'checkbox', checked: !!value, onchange: (e) => set(e.target.checked) }),
        f.text || '');
      break;
    case 'color':
      control = h('input.color', {
        id, type: 'color', value: normColor(value, '#000000'),
        oninput: (e) => set(e.target.value),
      });
      break;
    case 'select': {
      const opts = selectOptions(f, settings, allFields);
      const sv = JSON.stringify(value);
      let idx = opts.findIndex((o) => JSON.stringify(o.value) === sv);
      // Fields whose options depend on this one (e.g. theme ← layout).
      const dependents = (allFields || []).filter((x) => x.depends_on === f.key);
      control = h('select.select', {
        id, onchange: (e) => {
          const i = Number(e.target.value);
          if (i < 0) return;
          setPath(settings, f.key, opts[i].value);
          for (const d of dependents) {
            const dOpts = selectOptions(d, settings, allFields);
            const cur = JSON.stringify(getPath(settings, d.key));
            if (dOpts.length && !dOpts.some((o) => JSON.stringify(o.value) === cur)) setPath(settings, d.key, dOpts[0].value);
          }
          changed();
          if (dependents.length) rebuild();
        },
      },
      idx < 0 ? h('option', { value: '-1', selected: true, disabled: true }, value === undefined ? '– ' + t('please choose') + ' –' : `(${String(value)})`) : null,
      opts.map((o, i) => h('option', { value: String(i), selected: i === idx }, o.label ?? String(o.value))));
      break;
    }
    case 'file':
      wide = true;
      control = h('div.file-field', null,
        h('input.text', {
          id, type: 'text', placeholder: f.placeholder || t('/path/to/file'), value: value ?? '', spellcheck: 'false',
          oninput: (e) => set(e.target.value),
        }),
        h('div.field-help', null, t('Local path on the computer running the service.')));
      break;
    case 'page': {
      const profField = allFields.find((x) => x.type === 'profile');
      const profId = (profField && getPath(settings, profField.key)) || S.view.profile;
      const prof = profileById(profId);
      const pages = prof?.pages || [];
      const cur = value ?? '';
      control = h('select.select', { id, onchange: (e) => set(e.target.value || undefined) },
        h('option', { value: '', selected: !cur }, '– ' + t('none') + ' –'),
        pages.map((p, i) => h('option', { value: p, selected: String(cur).toLowerCase() === p.toLowerCase() },
          t('Page {n}', { n: i + 1 }) + (p === S.view.page ? ' ' + t('(current)') : ''))),
        cur && !pages.some((p) => p.toLowerCase() === String(cur).toLowerCase())
          ? h('option', { value: cur, selected: true }, t('Unknown page ({id}…)', { id: String(cur).slice(0, 8) })) : null);
      break;
    }
    case 'profile': {
      const cur = value ?? '';
      const viewModel = profileById(S.view.profile)?.model;
      const fitting = S.state.profiles.filter((p) => !viewModel || compatibleModel(viewModel, p.model));
      control = h('select.select', {
        id, onchange: (e) => {
          set(e.target.value || undefined);
          if (allFields.some((x) => x.type === 'page')) rebuild();
        },
      },
      h('option', { value: '', selected: !cur }, '– ' + t('none') + ' –'),
      // Only profiles the device showing this page can display.
      fitting
        .map((p) => h('option', { value: p.id, selected: String(cur).toUpperCase() === p.id.toUpperCase() },
          `${p.name} (${modelName(p.model)})`)),
      cur && !fitting.some((p) => p.id.toUpperCase() === String(cur).toUpperCase())
        ? h('option', { value: cur, selected: true }, t('Unknown profile')) : null);
      break;
    }
    case 'hotkey':
      wide = true;
      control = hotkeyField(id, value, set, f.rows);
      break;
    case 'actions':
      wide = true;
      control = actionsField(id, value, set, rebuild, f);
      break;
    case 'text':
    default:
      control = h('input.text', {
        id, type: 'text', placeholder: f.placeholder || '', value: value ?? '', spellcheck: 'false',
        oninput: (e) => set(e.target.value),
      });
  }
  return h('div.form-row', { class: wide ? 'wide' : null },
    h('label.field-label', { for: id }, f.label || f.key),
    h('div.form-control', null, control, f.help ? h('div.field-help', null, f.help) : null));
}

/**
 * Up to 4 Elgato hotkey slots (`Settings.Hotkeys`). Positions matter (sequence, or state 1/2 for
 * the hotkey switch), so clearing a slot keeps the others in place; the array is always padded to 4.
 * `rows`: fixed number of rows labelled by state (hotkey switch).
 */
function hotkeyField(id, value, set, rows = 0) {
  const list = [];
  for (let i = 0; i < 4; i++) list.push(Array.isArray(value) && value[i] ? { ...value[i] } : unusedHotkey());
  const mac = S.state.settings?.platform === 'mac';
  const box = h('div.hotkeys');
  let recIdx = -1; // slot being recorded
  let focusIdx = -1; // row whose button gets the focus after the next render
  let pressedMods = new Set();
  let multi = false;
  let shown = [];

  const save = () => set(list.map((x) => ({ ...x })));
  const lastUsed = () => {
    let n = -1;
    list.forEach((x, i) => { if (!isUnusedHotkey(x)) n = i; });
    return n;
  };
  const modsOf = (e, except) => ({
    shift: e.shiftKey && !/^Shift/.test(except),
    ctrl: e.ctrlKey && !/^Control/.test(except),
    alt: e.altKey && !/^Alt/.test(except),
    meta: e.metaKey && !/^(Meta|OS)/.test(except),
  });
  // Ends recording; the focus goes back to the row's Record button.
  const done = () => {
    focusIdx = recIdx;
    stop();
    render();
  };
  const finish = (hk) => {
    list[recIdx] = hk;
    save();
    done();
  };
  const onKeyDown = (e) => {
    // Plain Tab leaves the recorder (keyboard navigation) and moves on from its button;
    // with modifiers it is recorded.
    if (e.code === 'Tab' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
      done();
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    if (e.repeat) return;
    if (MODIFIER_CODES.has(e.code)) {
      pressedMods.add(e.code);
      if (pressedMods.size > 1) multi = true;
      shown[recIdx]?.show(hotkeyLabel(makeHotkey(e.code, modsOf(e, '')), mac) + ' + …');
      return;
    }
    const hk = makeHotkey(e.code, { shift: e.shiftKey, ctrl: e.ctrlKey, alt: e.altKey, meta: e.metaKey });
    if (isUnusedHotkey(hk)) {
      toast(t('This key cannot be used in a hotkey.'), 'warn');
      return;
    }
    finish(hk);
  };
  const onKeyUp = (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (!MODIFIER_CODES.has(e.code)) return;
    if (!multi && pressedMods.has(e.code)) {
      finish(makeHotkey(e.code, modsOf(e, e.code)));
      return;
    }
    pressedMods.delete(e.code);
    if (!pressedMods.size) multi = false;
  };
  const onBlur = () => { stop(); render(); };
  const start = (i) => {
    stop();
    recIdx = i;
    focusIdx = i;
    pressedMods = new Set();
    multi = false;
    addEventListener('keydown', onKeyDown, true);
    addEventListener('keyup', onKeyUp, true);
    addEventListener('blur', onBlur);
    render();
  };
  function stop() {
    if (recIdx < 0) return;
    recIdx = -1;
    removeEventListener('keydown', onKeyDown, true);
    removeEventListener('keyup', onKeyUp, true);
    removeEventListener('blur', onBlur);
  }
  function render() {
    // Show all slots up to the last used one, plus one empty slot to add another combination.
    const count = rows ? Math.min(4, Math.max(rows, lastUsed() + 1))
      : Math.min(4, Math.max(1, lastUsed() + 2, recIdx + 1));
    shown = [];
    const focus = focusIdx;
    focusIdx = -1;
    box.replaceChildren();
    for (let i = 0; i < count; i++) {
      const recording = recIdx === i;
      const display = h('span.hk-display');
      const show = (text) => {
        const label = hotkeyLabel(list[i], mac);
        display.textContent = text ?? (label || (!rows && i > 0 && i > lastUsed() ? t('Another combination…') : t('None')));
        display.classList.toggle('empty', !text && !label);
      };
      show(recording ? t('Press a key combination…') : undefined);
      shown.push({ show });
      const btn = h('button.btn.small', {
        id: i === 0 ? id : null, type: 'button', class: recording ? 'recording' : null,
        onclick: () => { if (recording) done(); else start(i); },
      }, recording ? t('Cancel') : t('Record'));
      box.append(h('div.hotkey', null,
        h('span.hk-num', { class: rows ? 'state' : null }, rows ? t('State {n}', { n: i + 1 }) : `${i + 1}.`),
        display,
        btn,
        h('button.icon-btn.danger', {
          type: 'button', title: t('Remove combination'), disabled: isUnusedHotkey(list[i]),
          onclick: () => { stop(); list[i] = unusedHotkey(); save(); focusIdx = i; render(); },
        }, '×')));
      if (i === focus) btn.focus({ preventScroll: true });
    }
  }
  cur.teardown.push(stop);
  render();
  return box;
}

// ActionIDs of nested multi-action entries whose settings are expanded (survives form rebuilds).
const expandedNested = new Set();

function actionsField(id, value, set, rebuild, f = {}) {
  const list = Array.isArray(value) ? value : [];
  // Key Logic slots hold a single action and only accept actions supported there.
  const max = Number.isFinite(f.max) ? f.max : Infinity;
  const allowed = f.filter === 'keylogic' ? (a) => a.keylogic !== false && a.controllers?.includes('Keypad') : (a) => a.multi;
  const save = () => set(list.slice());
  const items = list.map((inst, i) => {
    const a = actionInfo(inst?.UUID);
    const form = formFields(a).filter((f) => f.type !== 'actions');
    const key = inst?.ActionID || `idx${i}`;
    const open = form.length > 0 && expandedNested.has(key);
    const summary = nestedSummary(inst, a);
    const row = h('li.multi-item', { class: open ? 'open' : null },
      h('div.multi-row', null,
        a?.icon ? h('img', { src: a.icon, alt: '', draggable: false }) : h('span.multi-icon'),
        h('span.multi-name', null, inst?.States?.[0]?.Title || a?.name || inst?.Name || inst?.UUID || '?',
          summary ? h('span.multi-summary', null, ' – ' + summary) : null),
        form.length ? h('button.icon-btn', {
          type: 'button', title: open ? t('Hide settings') : t('Edit settings'),
          class: open ? 'active' : null,
          onclick: () => { if (open) expandedNested.delete(key); else expandedNested.add(key); rebuild(); },
        }, '⚙') : null,
        max > 1 ? h('button.icon-btn', {
          type: 'button', title: t('Move up'), disabled: i === 0,
          onclick: () => { [list[i - 1], list[i]] = [list[i], list[i - 1]]; save(); rebuild(); },
        }, '↑') : null,
        max > 1 ? h('button.icon-btn', {
          type: 'button', title: t('Move down'), disabled: i === list.length - 1,
          onclick: () => { [list[i + 1], list[i]] = [list[i], list[i + 1]]; save(); rebuild(); },
        }, '↓') : null,
        h('button.icon-btn.danger', {
          type: 'button', title: t('Remove'),
          onclick: () => { list.splice(i, 1); expandedNested.delete(key); save(); rebuild(); },
        }, '×')));
    if (open) {
      inst.Settings = inst.Settings && typeof inst.Settings === 'object' ? inst.Settings : {};
      row.append(h('div.form.nested-form', null,
        form.map((f) => buildField(f, inst.Settings, save, rebuild, form))));
    }
    return row;
  });
  const groups = S.catalogue.map((g) => ({ g, acts: g.actions.filter(allowed) })).filter((x) => x.acts.length);
  const add = h('select.select', {
    id,
    onchange: (e) => {
      const a = actionInfo(e.target.value);
      if (!a) return;
      const inst = {
        ActionID: uuidv4(), LinkedTitle: true, Name: a.name, Settings: {}, State: 0,
        States: [{}], UUID: a.uuid,
      };
      // Store form defaults explicitly (the top-level slot gets them from the daemon on creation).
      for (const f of a.form || []) {
        if (f.default !== undefined && f.type !== 'actions') setPath(inst.Settings, f.key, structuredClone(f.default));
      }
      if ((a.form || []).some((f) => f.type !== 'actions')) expandedNested.add(inst.ActionID);
      list.push(inst);
      save();
      rebuild();
    },
  },
  h('option', { value: '', selected: true }, max === 1 ? t('Choose action…') : '+ ' + t('Add action…')),
  groups.map(({ g, acts }) => h('optgroup', { label: g.category },
    acts.map((a) => h('option', { value: a.uuid }, a.name)))));
  return h('div.multi', null,
    list.length ? h('ol.multi-list', null, items) : h('div.muted', null, max === 1 ? t('No action.') : t('No actions yet.')),
    list.length < max ? add : null);
}

/** Short description of a nested action's settings for the collapsed list row. */
function nestedSummary(inst, action) {
  const st = inst?.Settings || {};
  if (Array.isArray(st.Hotkeys)) {
    const mac = S.state.settings?.platform === 'mac';
    return st.Hotkeys.filter((x) => !isUnusedHotkey(x)).map((x) => hotkeyLabel(x, mac)).join(', ');
  }
  for (const f of action?.form || []) {
    const v = getPath(st, f.key);
    if (v == null || v === '' || typeof v === 'object') continue;
    if (f.type === 'select') {
      const o = (f.options || []).find((x) => JSON.stringify(x.value) === JSON.stringify(v));
      return o ? o.label : String(v);
    }
    if (f.type === 'checkbox') continue;
    return f.type === 'number' && /ms\)/.test(f.label || '') ? `${v} ms` : String(v).slice(0, 40);
  }
  return '';
}
