// Action catalogue (sidebar).

import { MODELS, S, controllersOf, h, profileById, slotAccepts } from './store.js';
import { t } from './i18n.js';
import { toast } from './dialogs.js';
import { placeAction } from './canvas.js';
import { el } from './dom.js';
import { markCandidates } from './dragdrop.js';

const collapsed = new Set(JSON.parse(localStorage.getItem('sd.collapsed') || '[]'));

export function renderCatalogue() {
  const q = el.search.value.trim().toLowerCase();
  // Keep the keyboard focus on the same entry when the list is rebuilt.
  const f = el.catalogue.contains(document.activeElement) ? document.activeElement : null;
  const refocus = f?.dataset.uuid ? `.action-item[data-uuid="${CSS.escape(f.dataset.uuid)}"]`
    : f?.dataset.category ? `.cat-head[data-category="${CSS.escape(f.dataset.category)}"]` : null;
  // Hide actions the shown model has no place for (info bar actions on a Plus etc.).
  const m = MODELS[profileById(S.view.profile)?.model];
  const placeable = (a) => !m || controllersOf(a).some((c) => c === 'Keypad' || (c === 'Neo' && m.kind === 'neo') || (c === 'Encoder' && m.encoders > 0));
  const groups = [];
  for (const g of S.catalogue) {
    const all = (g.actions || []).filter(placeable);
    const acts = all.filter((a) => !q
      || [a.name, a.tooltip, g.category, a.uuid].some((x) => x && String(x).toLowerCase().includes(q)));
    if (!all.length || (!acts.length && q)) continue;
    const isCollapsed = !q && collapsed.has(g.category);
    const list = h('div.cat-list', { hidden: isCollapsed }, acts.map(actionItem));
    const head = h('button.cat-head', {
      'aria-expanded': String(!isCollapsed),
      dataset: { category: g.category },
      onclick: () => {
        if (q) return;
        if (collapsed.has(g.category)) collapsed.delete(g.category);
        else collapsed.add(g.category);
        localStorage.setItem('sd.collapsed', JSON.stringify([...collapsed]));
        renderCatalogue();
      },
    },
    h('span.chev', { class: isCollapsed ? 'closed' : null }, '▾'),
    g.icon ? h('img.cat-icon', { src: g.icon, alt: '', draggable: false }) : h('span.cat-icon'),
    h('span.cat-name', null, g.category),
    h('span.cat-count', null, String(acts.length)));
    groups.push(h('div.cat', null, head, list));
  }
  if (!groups.length) {
    groups.push(h('div.muted.center.pad', null, S.catalogue.length ? t('No matching actions.') : t('No actions available.')));
  }
  el.catalogue.replaceChildren(...groups);
  if (refocus) el.catalogue.querySelector(refocus)?.focus({ preventScroll: true });
}

function actionItem(a) {
  const unsupported = a.supported === false;
  const tip = [a.name, a.tooltip, unsupported ? t('Not supported on Linux') : null].filter(Boolean).join('\n');
  const icon = a.icon ? h('img.act-icon', { src: a.icon, alt: '', draggable: false }) : h('span.act-icon');
  return h('div.action-item', {
    class: unsupported ? 'unsupported' : null,
    draggable: true,
    tabIndex: 0,
    role: 'button',
    title: tip,
    'aria-label': tip,
    dataset: { uuid: a.uuid },
    ondragstart: (e) => {
      S.drag = { kind: 'action', action: a };
      e.dataTransfer.effectAllowed = 'copy';
      e.dataTransfer.setData('text/plain', a.uuid);
      if (icon.tagName === 'IMG' && icon.complete && icon.naturalWidth) e.dataTransfer.setDragImage(icon, 16, 16);
      document.body.classList.add('dragging');
      markCandidates();
      // Narrow window: the action list covers the device, move it out of the way.
      setTimeout(() => setSidebarOpen(false));
    },
    ondblclick: () => placeOnSelected(a),
    onkeydown: (e) => {
      if ((e.key === 'Enter' || e.key === ' ') && !e.ctrlKey && !e.altKey && !e.metaKey) {
        e.preventDefault();
        placeOnSelected(a);
      }
    },
  },
  icon,
  h('div.act-text', null,
    h('div.act-name', null, a.name),
    unsupported ? h('div.act-hint', null, t('Not supported on Linux')) : null));
}

/** Put the action onto the selected slot (double-click / Enter on an action). */
function placeOnSelected(a) {
  const sel = S.selected;
  if (!sel || !S.view.page) {
    toast(t('Select a spot on the device first.'), 'info', 2500);
    return;
  }
  if (!slotAccepts(sel.controller, sel.coord, a)) {
    toast(t('This action does not fit the selected spot.'), 'warn', 3000);
    return;
  }
  placeAction(a, sel);
}

el.search.addEventListener('input', () => renderCatalogue());
el.search.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    el.search.value = '';
    renderCatalogue();
  } else if (e.key === 'ArrowDown') {
    e.preventDefault();
    el.catalogue.querySelector('.cat-head, .action-item')?.focus();
  }
});
// Up/down arrows walk through the category headers and actions.
el.catalogue.addEventListener('keydown', (e) => {
  if ((e.key !== 'ArrowDown' && e.key !== 'ArrowUp') || e.ctrlKey || e.altKey || e.metaKey) return;
  const items = [...el.catalogue.querySelectorAll('.cat-head, .cat-list:not([hidden]) > .action-item')];
  const i = items.indexOf(document.activeElement);
  if (i < 0) return;
  e.preventDefault();
  if (e.key === 'ArrowUp' && i === 0) el.search.focus();
  else items[e.key === 'ArrowDown' ? Math.min(items.length - 1, i + 1) : i - 1]?.focus();
});

// ---- narrow windows: the action list is a drawer -------------------------------------------
export function setSidebarOpen(open) {
  document.body.classList.toggle('sidebar-open', open);
  el.sidebarBtn.setAttribute('aria-expanded', String(open));
}
el.sidebarBtn.addEventListener('click', () => {
  const open = !document.body.classList.contains('sidebar-open');
  setSidebarOpen(open);
  if (open) el.search.focus({ preventScroll: true });
});
