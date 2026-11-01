// Loading state, catalogue and pages; deciding what the canvas shows.

import { S, api, compatibleModel, currentDevice, debounce, liveDevice, pagePath, profileById } from './store.js';
import { flushEditor, renderEditor } from './editor.js';
import { t } from './i18n.js';
import { toast } from './dialogs.js';
import { renderCanvas } from './canvas.js';
import { renderCatalogue } from './catalogue.js';
import { renderCrumbs, renderPagebar, resolveFolderChain } from './navigation.js';
import { renderTopbar } from './topbar.js';

S.pressed = new Set();
S.userPickedDevice = false;
S.expectPage = null;


/** Run fn serially; calls during a run cause exactly one re-run afterwards. */
function coalesced(fn) {
  let running = null;
  let again = false;
  return function run() {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          try {
            await fn();
          } catch (e) {
            console.error(e);
            toast(e.message || String(e), 'error');
          }
        } while (again);
      } finally {
        running = null;
      }
    })();
    return running;
  };
}

export const loadState = coalesced(async () => {
  const st = await api('/api/state');
  S.state = {
    devices: Array.isArray(st?.devices) ? st.devices : [],
    profiles: Array.isArray(st?.profiles) ? st.profiles : [],
    settings: st?.settings || {},
  };
  pickDevice();
  await syncView();
});

export const loadCatalogue = coalesced(async () => {
  const cat = await api('/api/actions');
  S.catalogue = Array.isArray(cat) ? cat : [];
  S.actionMap = new Map();
  for (const g of S.catalogue) {
    for (const a of g.actions || []) {
      a.category = g.category;
      if (!S.actionMap.has(a.uuid)) S.actionMap.set(a.uuid, a);
    }
  }
  renderCatalogue();
  if (S.page) {
    renderCanvas();
    renderEditor(true);
  }
});

let pageSeq = 0;
export async function loadPage() {
  const seq = ++pageSeq;
  const { profile, page } = S.view;
  if (!profile || !page) {
    S.page = null;
    renderAll();
    return;
  }
  try {
    const data = await api(pagePath(profile, page));
    if (seq !== pageSeq) return;
    S.page = data || { id: page, profile, controllers: [] };
    S.pageCache.set(`${profile}/${page}`, S.page);
    await resolveFolderChain(profile, page, S.page);
    if (seq !== pageSeq) return;
  } catch (e) {
    if (seq !== pageSeq) return;
    // Keep showing what we had (e.g. the daemon is restarting) instead of an empty page.
    const known = S.page?.id === page && S.page?.profile === profile ? S.page : S.pageCache.get(`${profile}/${page}`);
    S.page = known || { id: page, profile, controllers: [] };
    toast(t('Could not load the page: {msg}', { msg: e.message }), 'error');
  }
  S.v++;
  renderAll();
}
export const schedulePageReload = debounce(() => loadPage(), 120);

/** A drag ended: show what changed meanwhile. */
export function dragEnded() {
  if (!renderAfterDrag) return;
  renderAfterDrag = false;
  renderAll();
}

function pickDevice() {
  const devs = S.state.devices;
  let d = devs.find((x) => x.id === S.deviceId);
  if (!d || (!S.userPickedDevice && !d.connected && devs.some((x) => x.connected))) {
    d = devs.find((x) => x.connected) || d || devs[0] || null;
  }
  S.deviceId = d ? d.id : null;
}

export function visibleProfiles() {
  const d = currentDevice();
  const ps = S.state.profiles;
  return d ? ps.filter((p) => compatibleModel(d.model, p.model)) : ps;
}

function computeView() {
  const live = liveDevice();
  if (live) {
    const prof = profileById(live.profile);
    const page = live.page || prof?.pages?.[0] || prof?.default_page || null;
    return { profile: live.profile, page, live: true, folderDepth: live.folder_depth || 0 };
  }
  const list = visibleProfiles();
  let prof = list.find((p) => p.id === S.localProfile) || list[0] || null;
  if (!prof) {
    S.localProfile = null;
    S.localPage = null;
    return { profile: null, page: null, live: false, folderDepth: 0 };
  }
  if (prof.id !== S.localProfile) {
    S.localProfile = prof.id;
    S.localPage = null;
  }
  const pages = prof.pages || [];
  const known = (pg) => pages.includes(pg) || (prof.folders || []).includes(pg);
  if (!S.localPage || !known(S.localPage)) {
    S.localPage = pages[0] || prof.default_page || null;
    S.navStack = [];
  }
  const isFolder = !pages.includes(S.localPage);
  return {
    profile: prof.id, page: S.localPage, live: false,
    folderDepth: isFolder ? Math.max(1, S.navStack.length) : 0,
  };
}

export async function syncView() {
  const v = computeView();
  const changed = v.profile !== S.view.profile || v.page !== S.view.page;
  if (changed) {
    flushEditor();
    // Hardware navigated somewhere we did not ask for → our folder stack is meaningless.
    if (v.page !== S.expectPage && v.profile === S.view.profile && S.view.page) S.navStack = [];
    if (v.profile !== S.view.profile) S.navStack = [];
    if (!v.folderDepth) S.navStack = [];
    S.expectPage = null;
  }
  S.view = v;
  if (v.profile) localStorage.setItem('sd.profile', v.profile);
  if (S.deviceId) localStorage.setItem('sd.device', S.deviceId);
  renderTopbar();
  if (changed) {
    S.selected = null;
    S.selectedState = 0;
    await loadPage();
  } else {
    renderAll();
  }
}

let catalogueModel;
let renderAfterDrag = false;
function renderAll() {
  // Redrawing the keys while one is dragged would remove the dragged element (and with
  // it the end of the drag): wait for the drop.
  if (S.drag) {
    renderAfterDrag = true;
    return;
  }
  renderCanvas();
  renderCrumbs();
  renderPagebar();
  renderEditor();
  // The catalogue depends on the model of the shown profile.
  const model = profileById(S.view.profile)?.model;
  if (model !== catalogueModel) {
    catalogueModel = model;
    renderCatalogue();
  }
}
