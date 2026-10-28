// Device canvas: slots, selection, keyboard navigation, slot menu and operations.

import { BACKTOPARENT, OPENCHILD, S, actionInfo, api, enc, geometryFor, h, instanceAccepts, instanceAt, isNeoGeometry, pagePath, profileById, renderUrl, slotKey, slotLabel } from './store.js';
import { canTestPress, flushEditor, renderEditor } from './editor.js';
import { confirmDialog, contextMenu, toast } from './dialogs.js';
import { t } from './i18n.js';
import { loadPage, schedulePageReload } from './data.js';
import { $, el } from './dom.js';
import { folderTarget, goBack, goToPage } from './navigation.js';
import { createProfile } from './topbar.js';

export function renderCanvas() {
  const prof = profileById(S.view.profile);
  const dev = el.device;
  // Re-rendering replaces the slot elements: keep the keyboard focus on the same spot.
  const focused = dev.contains(document.activeElement) ? document.activeElement.closest('.slot') : null;
  const refocus = focused && { controller: focused.dataset.controller, coord: focused.dataset.coord };
  if (!prof || !S.view.page) {
    dev.className = 'device empty-device';
    S.geom = null;
    dev.replaceChildren(h('div.no-profile', null,
      h('div', null, S.state.profiles.length ? t('No profile for this device.') : t('No profiles yet.')),
      h('button.btn.primary', { onclick: createProfile }, t('Create profile')),
      h('button.btn', { onclick: () => el.importInput.click() }, t('Import profile…'))));
    return;
  }
  const g = geometryFor(prof);
  S.geom = g;
  const neo = isNeoGeometry(g);
  const encs = g.encoders > 0;
  dev.className = 'device' + (neo ? ' neo' : '') + (encs ? ' plus' : '');
  dev.style.setProperty('--cols', g.cols);
  const grid = h('div.keygrid');
  for (let r = 0; r < g.rows; r++) {
    for (let c = 0; c < g.cols; c++) grid.append(slotEl('Keypad', `${c},${r}`, 'key'));
  }
  const children = [grid];
  if (neo) {
    const bar = slotEl('Neo', '1,0', 'infobar');
    bar.style.gridColumn = g.cols > 2 ? `2 / ${g.cols}` : '1 / -1';
    const left = slotEl('Neo', '0,0', 'touch');
    left.style.gridColumn = '1';
    const right = slotEl('Neo', '2,0', 'touch');
    right.style.gridColumn = String(Math.max(g.cols, 3));
    children.push(h('div.neo-row', null, left, bar, right));
  }
  if (encs) {
    const row = h('div.encoder-row', { style: { gridTemplateColumns: `repeat(${g.encoders}, var(--key))` } });
    for (let i = 0; i < g.encoders; i++) row.append(slotEl('Encoder', `${i},0`, 'encoder'));
    children.push(row);
  }
  dev.replaceChildren(...children);
  layoutDevice();
  updateTabStop(refocus);
  if (refocus) slotNode(refocus)?.focus({ preventScroll: true });
}

function slotNode(sel) {
  return sel && [...el.device.querySelectorAll('.slot')].find((s) => s.dataset.controller === sel.controller && s.dataset.coord === sel.coord);
}

/** The device is one tab stop (last focused, selected or first slot); arrow keys move inside. */
function updateTabStop(prefer) {
  const slots = [...el.device.querySelectorAll('.slot')];
  const stop = slotNode(prefer) || slotNode(S.focusSlot) || slotNode(S.selected) || slots[0];
  for (const s of slots) s.tabIndex = s === stop ? 0 : -1;
}

function slotEl(controller, coord, kind) {
  const inst = instanceAt(controller, coord);
  const action = inst ? actionInfo(inst.UUID) : null;
  const key = slotKey(controller, coord);
  const sel = S.selected && S.selected.controller === controller && S.selected.coord === coord;
  let tip = slotLabel(controller, coord);
  if (inst) {
    tip += ` – ${action?.name || inst.Name || inst.UUID}`;
    const title = inst.States?.[inst.State || 0]?.Title;
    if (title) tip += ` "${title.replace(/\n/g, ' ')}"`;
    if (inst.UUID === OPENCHILD) tip += '\n' + t('Double-click opens the folder');
    if (action && action.supported === false) tip += '\n' + t('Not supported on Linux');
  } else {
    tip += ' – ' + t('empty');
  }
  const s = h('div.slot', {
    class: [kind, inst ? 'filled' : 'empty', sel ? 'selected' : '', S.pressed.has(key) ? 'pressed' : '',
      inst?.UUID === OPENCHILD ? 'folder' : '', action && action.supported === false ? 'unsupported' : ''].filter(Boolean).join(' '),
    dataset: { controller, coord },
    draggable: !!inst,
    tabIndex: -1, // see updateTabStop
    title: tip,
    role: 'button',
    'aria-label': tip,
  });
  if (kind === 'encoder') {
    const g = S.geom;
    if (g?.screen) {
      s.append(h('div.strip-seg', null, h('img', { src: renderUrl(controller, coord, 144), alt: '', draggable: false })));
    }
    s.append(h('div.dial', null, action?.icon ? h('img', { src: action.icon, alt: '', draggable: false }) : null));
  } else if (inst || controller === 'Neo') {
    s.append(h('img', {
      src: renderUrl(controller, coord, kind === 'infobar' ? 248 : 144), alt: '', draggable: false,
      onerror: (e) => { e.target.style.visibility = 'hidden'; },
    }));
  }
  if (inst?.UUID === OPENCHILD) s.append(h('span.folder-badge', { 'aria-hidden': 'true' }, '📁'));
  return s;
}

function layoutDevice() {
  const g = S.geom;
  if (!g) return;
  const W = el.deviceWrap.clientWidth - 40;
  const H = el.deviceWrap.clientHeight - 24;
  const neo = isNeoGeometry(g);
  let key = 116;
  let gap = 0, pad = 0, barH = 0;
  for (; key > 32; key -= 2) {
    gap = Math.round(key * 0.17);
    pad = Math.round(key * 0.28);
    const w = g.cols * key + (g.cols - 1) * gap + 2 * pad;
    let hh = g.rows * key + (g.rows - 1) * gap + 2 * pad;
    if (neo) {
      const span = Math.max(1, g.cols - 2);
      const barW = span * key + (span - 1) * gap;
      const [sw, sh] = g.screen || [248, 58];
      barH = Math.round(barW * sh / sw);
      hh += gap + barH;
    }
    if (g.encoders > 0) {
      hh += gap + (g.screen ? key / 2 + gap / 2 : 0) + key * 0.62;
    }
    if (w <= W && hh <= H) break;
  }
  const dev = el.device;
  dev.style.setProperty('--key', key + 'px');
  dev.style.setProperty('--gap', gap + 'px');
  dev.style.setProperty('--pad', pad + 'px');
  dev.style.setProperty('--bar-h', barH + 'px');
}
new ResizeObserver(() => layoutDevice()).observe(el.deviceWrap);

export function slotFromEvent(e) {
  const s = e.target.closest?.('.slot');
  if (!s || !el.device.contains(s)) return null;
  return { el: s, controller: s.dataset.controller, coord: s.dataset.coord };
}

export function select(sel) {
  const same = S.selected && sel && S.selected.controller === sel.controller && S.selected.coord === sel.coord;
  if (same) return;
  flushEditor();
  S.selected = sel ? { controller: sel.controller, coord: sel.coord } : null;
  const inst = sel ? instanceAt(sel.controller, sel.coord) : null;
  S.selectedState = inst?.State && inst.States && inst.State < inst.States.length ? inst.State : 0;
  for (const s of el.device.querySelectorAll('.slot')) {
    s.classList.toggle('selected', !!sel && s.dataset.controller === sel.controller && s.dataset.coord === sel.coord);
  }
  renderEditor();
}

el.device.addEventListener('click', (e) => {
  const s = slotFromEvent(e);
  if (s) {
    select(s);
    s.el.focus({ preventScroll: true });
  } else {
    select(null);
  }
});
el.deviceWrap.addEventListener('click', (e) => {
  if (e.target === el.deviceWrap) select(null);
});
el.device.addEventListener('dblclick', (e) => {
  const s = slotFromEvent(e);
  if (s) openSlot(s);
});

/** Folder keys open the folder, "Parent folder" keys go back (double-click / Enter). */
function openSlot(s) {
  const inst = instanceAt(s.controller, s.coord);
  if (inst?.UUID === OPENCHILD) {
    const target = folderTarget(inst);
    if (target) goToPage(target, { push: true });
    else toast(t('Unknown folder target'), 'warn');
  } else if (inst?.UUID === BACKTOPARENT) {
    goBack();
  }
}

function slotMenuAt(s) {
  const r = s.el.getBoundingClientRect();
  slotMenu(r.left + r.width / 2, r.top + r.height / 2, s);
}

el.device.addEventListener('contextmenu', (e) => {
  const s = slotFromEvent(e);
  if (!s) return;
  e.preventDefault();
  select(s);
  // Keyboard-triggered (menu key) events may carry no pointer position.
  if (!e.clientX && !e.clientY) slotMenuAt(s);
  else slotMenu(e.clientX, e.clientY, s);
});

el.device.addEventListener('focusin', (e) => {
  const s = slotFromEvent(e);
  if (!s) return;
  S.focusSlot = { controller: s.controller, coord: s.coord };
  updateTabStop(S.focusSlot);
});

// Keyboard: arrows move between slots, Enter/Space select (Enter also opens folders),
// Shift+F10 / menu key open the context menu.
el.device.addEventListener('keydown', (e) => {
  const s = slotFromEvent(e);
  if (!s || e.ctrlKey || e.metaKey || e.altKey) return;
  if (/^Arrow|^(Home|End)$/.test(e.key) && !e.shiftKey) {
    e.preventDefault();
    neighbourSlot(s.el, e.key)?.focus();
  } else if ((e.key === 'Enter' || e.key === ' ') && !e.shiftKey) {
    e.preventDefault();
    select(s);
    if (e.key === 'Enter') openSlot(s);
  } else if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) {
    e.preventDefault();
    select(s);
    slotMenuAt(s);
  }
});

/** Rows of slots as shown (key rows, Neo row, dial row) for arrow key navigation. */
function slotRows() {
  const rows = [];
  const keys = [...el.device.querySelectorAll('.keygrid > .slot')];
  const cols = S.geom?.cols || keys.length || 1;
  for (let i = 0; i < keys.length; i += cols) rows.push(keys.slice(i, i + cols));
  for (const r of el.device.querySelectorAll('.neo-row, .encoder-row')) rows.push([...r.querySelectorAll('.slot')]);
  return rows.filter((r) => r.length);
}

function neighbourSlot(from, key) {
  const rows = slotRows();
  const ri = rows.findIndex((r) => r.includes(from));
  if (ri < 0) return null;
  const row = rows[ri];
  const ci = row.indexOf(from);
  if (key === 'ArrowLeft') return row[ci - 1] || null;
  if (key === 'ArrowRight') return row[ci + 1] || null;
  if (key === 'Home') return row[0];
  if (key === 'End') return row[row.length - 1];
  const next = rows[ri + (key === 'ArrowUp' ? -1 : 1)];
  if (!next) return null;
  // The slot below/above whose centre is closest (the Neo row is laid out differently).
  const centre = (n) => { const r = n.getBoundingClientRect(); return r.left + r.width / 2; };
  const x = centre(from);
  return next.reduce((best, n) => (Math.abs(centre(n) - x) < Math.abs(centre(best) - x) ? n : best));
}

function slotMenu(x, y, s) {
  const inst = instanceAt(s.controller, s.coord);
  const clip = S.clipboard;
  const canPaste = !!clip && instanceAccepts(s.controller, s.coord, clip.instance, clip.controller);
  contextMenu(x, y, [
    { label: t('Copy'), hint: t('Ctrl+C'), action: () => copySlot(s), disabled: !inst },
    { label: t('Paste'), hint: t('Ctrl+V'), action: () => pasteSlot(s), disabled: !canPaste },
    { label: t('Delete'), hint: t('Del'), action: () => deleteSlot(s), disabled: !inst, danger: true },
    '-',
    {
      label: t('Test press'), action: () => pressSlot(s), disabled: !inst || !canTestPress(),
      title: inst && !canTestPress() ? t('Connect the device to test the action') : null,
    },
    inst?.UUID === OPENCHILD ? { label: t('Open folder'), action: () => goToPage(folderTarget(inst), { push: true }) } : null,
  ].filter(Boolean));
}

// ---- slot operations -------------------------------------------------------------------------

export async function deleteSlot(s) {
  const inst = instanceAt(s.controller, s.coord);
  if (!inst) return;
  if (inst.UUID === OPENCHILD) {
    const ok = await confirmDialog(t('Delete folder'), t('The folder and all actions in it will be deleted.'));
    if (!ok) return;
  }
  const { profile, page } = S.view;
  try {
    const r = await api(`${pagePath(profile, page)}/slot?controller=${enc(s.controller)}&coord=${enc(s.coord)}`, { method: 'DELETE' });
    // What the service actually removed (our page copy may lag behind right after an edit).
    // Folders were confirmed (and their pages are gone); everything else can be restored.
    const snapshot = r?.removed;
    if (snapshot && snapshot.UUID !== OPENCHILD) {
      toast(t('Action deleted'), 'info', 8000, {
        label: t('Undo'),
        run: async () => {
          try {
            await api(`${pagePath(profile, page)}/paste`, {
              method: 'POST',
              body: { controller: s.controller, coord: s.coord, instance: snapshot, from_profile: profile, from_page: page },
            });
          } catch (e) {
            toast(e.message, 'error');
          }
          afterWrite();
        },
      });
    }
  } catch (e) {
    toast(e.message, 'error');
  }
  afterWrite();
}

export async function pressSlot(s) {
  try {
    await api(`${pagePath()}/press`, { method: 'POST', body: { controller: s.controller, coord: s.coord } });
  } catch (e) {
    toast(e.message, 'error');
  }
}

export function copySlot(s) {
  const inst = instanceAt(s.controller, s.coord);
  if (!inst) return;
  S.clipboard = {
    profile: S.view.profile, page: S.view.page, controller: s.controller, coord: s.coord,
    instance: structuredClone(inst),
  };
  toast(t('Action copied'), 'info', 1500);
}

export async function pasteSlot(s) {
  const clip = S.clipboard;
  if (!clip) return;
  if (!instanceAccepts(s.controller, s.coord, clip.instance, clip.controller)) {
    toast(t('This action cannot be placed here.'), 'warn');
    return;
  }
  if (clip.instance.UUID === OPENCHILD) {
    toast(t('Folders cannot be copied'), 'warn');
    return;
  }
  const { profile, page } = S.view;
  const target = instanceAt(s.controller, s.coord);
  // Pasting onto the key it was copied from (still there) changes nothing.
  if (clip.profile === profile && clip.page === page && clip.controller === s.controller && clip.coord === s.coord
    && target?.ActionID === clip.instance.ActionID) return;
  if (target?.UUID === OPENCHILD && !(await confirmDialog(t('Replace folder'), t('The existing folder and its contents will be deleted.')))) return;
  try {
    // The copied snapshot (not the source key as it is now), with its custom images.
    await api(`${pagePath()}/paste`, {
      method: 'POST',
      body: { controller: s.controller, coord: s.coord, instance: clip.instance, from_profile: clip.profile, from_page: clip.page },
    });
  } catch (e) {
    toast(e.message, 'error');
  }
  select({ controller: s.controller, coord: s.coord });
  afterWrite();
}

export async function placeAction(action, s) {
  const target = instanceAt(s.controller, s.coord);
  if (target?.UUID === OPENCHILD && !(await confirmDialog(t('Replace folder'), t('The existing folder and its contents will be deleted.')))) return;
  try {
    await api(`${pagePath()}/slot`, { method: 'POST', body: { controller: s.controller, coord: s.coord, uuid: action.uuid } });
  } catch (e) {
    toast(e.message, 'error');
    return;
  }
  S.selected = { controller: s.controller, coord: s.coord };
  S.selectedState = 0;
  afterWrite(true);
}

export async function moveSlot(from, to, copy) {
  const target = instanceAt(to.controller, to.coord);
  if (copy && target?.UUID === OPENCHILD && !(await confirmDialog(t('Replace folder'), t('The existing folder and its contents will be deleted.')))) return;
  try {
    await api(`${pagePath()}/move`, {
      method: 'POST',
      body: { from: { controller: from.controller, coord: from.coord }, to: { controller: to.controller, coord: to.coord }, copy: !!copy },
    });
  } catch (e) {
    toast(e.message, 'error');
    return;
  }
  S.selected = { controller: to.controller, coord: to.coord };
  S.selectedState = 0;
  afterWrite(true);
}

/** Reload page + previews after our own write (the daemon also sends a page event). */
export function afterWrite(immediate = false) {
  S.pageCache.delete(`${S.view.profile}/${S.view.page}`);
  if (immediate) {
    schedulePageReload.cancel();
    loadPage();
  } else {
    schedulePageReload();
  }
}
