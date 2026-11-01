// Pages and folders: navigation, breadcrumb, page bar.

import { OPENCHILD, S, api, currentDevice, enc, h, icon, liveDevice, pagePath, profileById } from './store.js';
import { confirmDialog, contextMenu, toast } from './dialogs.js';
import { flushEditor } from './editor.js';
import { t } from './i18n.js';
import { loadState, syncView } from './data.js';
import { el } from './dom.js';
import { sameId } from './events.js';

export async function goToPage(pageId, { push = false, reset = false } = {}) {
  if (!pageId || pageId === S.view.page) return;
  flushEditor();
  if (reset) S.navStack = [];
  if (push && S.view.page) S.navStack.push(S.view.page);
  S.expectPage = pageId;
  const live = S.view.live ? liveDevice() : null;
  if (live) {
    try {
      await api(`/api/devices/${enc(live.id)}`, { method: 'POST', body: { page: pageId } });
    } catch (e) {
      toast(e.message, 'error');
      if (push) S.navStack.pop();
      return;
    }
    await loadState();
  } else {
    S.localPage = pageId;
    await syncView();
  }
}

async function getPageData(profile, page) {
  const key = `${profile}/${page}`;
  if (S.pageCache.has(key)) return S.pageCache.get(key);
  try {
    const data = await api(pagePath(profile, page));
    S.pageCache.set(key, data);
    return data;
  } catch {
    return null;
  }
}

export function folderTarget(inst) {
  const id = inst?.Settings?.ProfileUUID;
  if (!id) return null;
  const prof = profileById(S.view.profile);
  const all = [...(prof?.folders || []), ...(prof?.pages || [])];
  return all.find((p) => p.toLowerCase() === String(id).toLowerCase()) || String(id).toLowerCase();
}

function findOpenchildIn(page, folderId) {
  for (const c of page?.controllers || []) {
    for (const inst of Object.values(c.Actions || {})) {
      if (inst?.UUID === OPENCHILD && String(inst.Settings?.ProfileUUID || '').toLowerCase() === folderId.toLowerCase()) return inst;
    }
  }
  return null;
}

/**
 * After a reload, or when the hardware opened a folder by itself, the UI does not know how it
 * got into the folder: rebuild the folder stack from the pages' `parent` links, one request per
 * level. The parents' data is also what the breadcrumb takes the folder names from.
 */
export async function resolveFolderChain(profileId, pageId, data) {
  const pages = profileById(profileId)?.pages || [];
  if (!data?.parent || pages.includes(pageId)) return;
  if (!sameId(S.navStack[S.navStack.length - 1], data.parent)) {
    const chain = [];
    for (let p = data.parent; p && !chain.includes(p) && chain.length < 32;) {
      chain.unshift(p);
      const d = await getPageData(profileId, p);
      p = pages.includes(p) ? null : d?.parent;
    }
    S.navStack = chain;
    if (!S.view.live && S.view.page === pageId) S.view.folderDepth = Math.max(1, chain.length);
  }
  await Promise.all(S.navStack.map((p) => getPageData(profileId, p)));
}

/** Locate the page containing the openchild action that leads to folderId. */
async function findParent(profileId, folderId) {
  const prof = profileById(profileId);
  if (!prof) return null;
  const candidates = [...(prof.pages || []), ...(prof.folders || [])].filter((p) => p !== folderId);
  for (const p of candidates) {
    const data = await getPageData(profileId, p);
    if (findOpenchildIn(data, folderId)) return p;
  }
  return null;
}

export async function goBack() {
  if (!S.view.folderDepth) return;
  const prof = profileById(S.view.profile);
  let parent = S.navStack.length ? S.navStack[S.navStack.length - 1] : null;
  if (!parent && S.page?.id === S.view.page) parent = S.page.parent;
  if (!parent) parent = await findParent(S.view.profile, S.view.page);
  if (!parent) parent = prof?.pages?.[0];
  if (!parent) return;
  if (S.navStack.length) S.navStack.pop();
  const stack = S.navStack.slice();
  await goToPage(parent);
  S.navStack = stack;
}

function folderName(folderId) {
  for (const data of S.pageCache.values()) {
    if (data?.profile && data.profile !== S.view.profile) continue;
    const inst = findOpenchildIn(data, folderId);
    if (inst) return (inst.States?.[0]?.Title || '').replace(/\s+/g, ' ').trim() || t('Folder');
  }
  return t('Folder');
}

export function renderCrumbs() {
  const prof = profileById(S.view.profile);
  const parts = [];
  if (S.view.folderDepth > 0) {
    parts.push(h('button.btn.small.back-btn', { title: t('Back to the parent folder'), onclick: goBack }, '← ' + t('Back')));
  }
  if (prof) {
    parts.push(h('span.crumb', null, prof.name));
    const pages = prof.pages || [];
    const idx = pages.indexOf(S.view.page);
    const root = idx >= 0 ? idx : pages.indexOf(S.navStack[0]);
    if (root >= 0) parts.push(h('span.crumb-sep', null, icon('right')), h('span.crumb', null, t('Page {n}', { n: root + 1 })));
    if (S.view.folderDepth > 0) {
      for (const p of S.navStack.slice(1)) parts.push(h('span.crumb-sep', null, icon('right')), h('span.crumb', null, folderName(p)));
      parts.push(h('span.crumb-sep', null, icon('right')), h('span.crumb.current', null, icon('folder'), ' ' + folderName(S.view.page)));
    }
  }
  const d = currentDevice();
  if (prof && !S.view.live) {
    parts.push(h('span.spacer'), h('span.badge', { title: t('Changes are saved directly to the profile') },
      d ? t('Device not connected – offline editing') : t('No device – offline editing')));
  }
  el.crumbs.replaceChildren(...parts);
}

export function renderPagebar() {
  const prof = profileById(S.view.profile);
  if (!prof) {
    el.pagebar.replaceChildren();
    return;
  }
  const pages = prof.pages || [];
  let active = pages.indexOf(S.view.page);
  if (active < 0 && S.navStack.length) active = pages.indexOf(S.navStack[0]);
  const tabs = pages.map((p, i) => h('div.page-tab-wrap', {
    dataset: { page: p },
    // Pages are moved by dragging their tab (see pageDrop).
    ondragover: (e) => pageDragOver(e, i),
    ondragleave: (e) => { if (!e.currentTarget.contains(e.relatedTarget)) e.currentTarget.classList.remove('drop-before', 'drop-after'); },
    ondrop: (e) => pageDrop(e, i),
  },
    h('button.page-tab', {
      class: i === active ? (S.view.folderDepth ? 'active inactive-folder' : 'active') : null,
      title: t('Page {n}', { n: i + 1 }) + (i === active && S.view.folderDepth ? ' ' + t('(in folder)') : '') + (pages.length > 1 ? '\n' + t('Drag to move the page (Alt+← / Alt+→)') : ''),
      draggable: pages.length > 1,
      ondragstart: (e) => {
        pageDrag = i;
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('application/x-tuxdeck-page', p);
        e.currentTarget.closest('.page-tab-wrap').classList.add('dragging');
      },
      ondragend: () => {
        pageDrag = null;
        for (const w of el.pagebar.querySelectorAll('.page-tab-wrap')) w.classList.remove('dragging', 'drop-before', 'drop-after');
      },
      onkeydown: (e) => {
        if (!e.altKey || (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight')) return;
        e.preventDefault();
        movePage(i, e.key === 'ArrowLeft' ? i - 1 : i + 1, true);
      },
      onclick: () => (S.view.page === p ? null : goToPage(p, { reset: true })),
      oncontextmenu: (e) => {
        e.preventDefault();
        contextMenu(e.clientX, e.clientY, [
          { label: t('Open'), action: () => goToPage(p, { reset: true }) },
          { label: t('Move left'), hint: t('Alt+←'), action: () => movePage(i, i - 1), disabled: i === 0 },
          { label: t('Move right'), hint: t('Alt+→'), action: () => movePage(i, i + 1), disabled: i === pages.length - 1 },
          { label: t('Delete page…'), action: () => deletePage(p, i), disabled: pages.length <= 1, danger: true },
        ]);
      },
    }, String(i + 1)),
    pages.length > 1 ? h('button.page-del', { title: t('Delete page {n}', { n: i + 1 }), onclick: () => deletePage(p, i) }, icon('close')) : null));
  const prev = active > 0 ? pages[active - 1] : null;
  const next = active >= 0 && active < pages.length - 1 ? pages[active + 1] : null;
  el.pagebar.replaceChildren(
    h('button.icon-btn', { title: t('Previous page'), disabled: !prev, onclick: () => goToPage(prev, { reset: true }) }, icon('left')),
    h('div.page-tabs', null, tabs),
    h('button.icon-btn', { title: t('Next page'), disabled: !next, onclick: () => goToPage(next, { reset: true }) }, icon('right')),
    h('button.btn.small.add-page', { title: t('Add a new page'), onclick: addPage }, '+ ' + t('Page')));
  el.pagebar.querySelector('.page-tab.active')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

// ---- moving pages ----------------------------------------------------------------------------

let pageDrag = null; // index of the page tab being dragged

/** Before or after the tab under the pointer (left or right half). */
function pageDragOver(e, i) {
  if (pageDrag === null) return;
  e.preventDefault();
  e.stopPropagation();
  e.dataTransfer.dropEffect = 'move';
  const r = e.currentTarget.getBoundingClientRect();
  const after = e.clientX > r.left + r.width / 2;
  for (const w of el.pagebar.querySelectorAll('.page-tab-wrap')) w.classList.remove('drop-before', 'drop-after');
  if (i !== pageDrag) e.currentTarget.classList.add(after ? 'drop-after' : 'drop-before');
}

function pageDrop(e, i) {
  if (pageDrag === null) return;
  e.preventDefault();
  e.stopPropagation();
  const r = e.currentTarget.getBoundingClientRect();
  const after = e.clientX > r.left + r.width / 2;
  const from = pageDrag;
  pageDrag = null;
  // Position in the list without the dragged page.
  let to = i + (after ? 1 : 0);
  if (from < to) to--;
  movePage(from, to);
}

/** Move the page at `from` to position `to`; offers to undo it. */
export async function movePage(from, to, keepFocus = false) {
  const prof = profileById(S.view.profile);
  const pages = [...(prof?.pages || [])];
  if (!prof || to < 0 || to >= pages.length || from === to) {
    renderPagebar();
    return;
  }
  const before = [...pages];
  const [moved] = pages.splice(from, 1);
  pages.splice(to, 0, moved);
  if (!(await setPageOrder(prof.id, pages))) return;
  if (keepFocus) el.pagebar.querySelector(`.page-tab-wrap[data-page="${CSS.escape(moved)}"] .page-tab`)?.focus();
  toast(t('Page {from} is now page {to}', { from: from + 1, to: to + 1 }), 'info', 6000, {
    label: t('Undo'),
    run: () => setPageOrder(prof.id, before),
  });
}

async function setPageOrder(profile, pages) {
  try {
    await api(`/api/profiles/${enc(profile)}/pages/order`, { method: 'POST', body: { pages } });
  } catch (e) {
    toast(e.message, 'error');
    return false;
  }
  // Show the new order right away (the service also sends a state event).
  const prof = profileById(profile);
  if (prof) prof.pages = pages;
  renderPagebar();
  await loadState();
  return true;
}

async function addPage() {
  const prof = profileById(S.view.profile);
  if (!prof) return;
  try {
    const r = await api(`/api/profiles/${enc(prof.id)}/pages`, { method: 'POST' });
    await loadState();
    if (r?.id) await goToPage(r.id, { reset: true });
  } catch (e) {
    toast(e.message, 'error');
  }
}

async function deletePage(pageId, idx) {
  const prof = profileById(S.view.profile);
  if (!prof || (prof.pages || []).length <= 1) return;
  const ok = await confirmDialog(t('Delete page'), t('Delete page {n} with all its actions and folders?', { n: idx + 1 }));
  if (!ok) return;
  const wasShown = S.view.page === pageId || S.navStack[0] === pageId;
  const pages = prof.pages;
  const neighbour = pages[idx + 1] || pages[idx - 1];
  try {
    if (wasShown) {
      await goToPage(neighbour, { reset: true });
    }
    await api(`/api/pages/${enc(prof.id)}/${enc(pageId)}`, { method: 'DELETE' });
  } catch (e) {
    toast(e.message, 'error');
  }
  await loadState();
}
