// Plugins view (overlay over #main): plugin catalogues, installed plugins, Linux checks.
// The catalogues are "sources" (see sources.js); each one provides tabs, a list, details
// and installation.
import { api, enc, h, debounce } from './store.js';
import { toast, confirmDialog, modal } from './dialogs.js';
import { t, tn, locale } from './i18n.js';
import { importFile } from './topbar.js';
import { SOURCES } from './sources.js';

const LIMIT = 24;
const ELGATO_MARKETPLACE = 'https://marketplace.elgato.com';
// Labels are translated with t() when rendered.
const LINUX_NOTE = 'Plugins are tested on Linux automatically when they are installed; see "Linux check" below.';

// Tab id -> source; "installed" is built in.
const TABS = [...SOURCES.flatMap((s) => s.tabs.map((tab) => ({ ...tab, source: s }))), { id: 'installed', label: 'Installed' }];

const M = {
  root: null,
  open: false,
  tab: TABS[0].id,
  search: '',
  ordering: null,
  items: [],
  count: 0,
  loading: false,
  error: null,
  seq: 0,
  detailId: null,
  detailSeq: 0,
  installing: new Set(), // item ids
  checks: {}, // plugin uuid -> full Linux check report (own or published)
  checking: new Set(), // check keys (plugin uuid or source key) in progress
  requested: new Set(), // checks started here (only those get a toast)
};
let hooks = {};
const el = {};

const tabInfo = (id = M.tab) => TABS.find((x) => x.id === id) || TABS[0];
const source = () => tabInfo().source;

// ---------------------------------------------------------------------------------------------
// helpers (also used by the sources)
// ---------------------------------------------------------------------------------------------

export const safeUrl = (u) => (typeof u === 'string' && /^https?:\/\//i.test(u) ? u : null);

/** Remote image – lazy, no referrer; falls back to a letter tile. */
export function remoteImg(urls, name, cls) {
  const fallback = () => h('div', { class: cls + ' img-fallback' }, ((name || '?').trim()[0] || '?').toUpperCase());
  const src = urls.map(safeUrl).find(Boolean);
  if (!src) return fallback();
  const img = h('img', {
    class: cls, alt: '', loading: 'lazy', referrerpolicy: 'no-referrer', decoding: 'async', draggable: false,
  });
  img.addEventListener('error', () => img.replaceWith(fallback()), { once: true });
  img.src = src;
  return img;
}

/** Children list without null/false (DOM append() would print "null"). */
export const kids = (...a) => a.flat(Infinity).filter((x) => x != null && x !== false);

export const spinner = (cls = '') => h('span.spinner', { class: cls || null, 'aria-hidden': 'true' });

export function plainText(s) {
  // Markdown-ish → plain text: normalise line breaks and drop emphasis markers. Never parsed as HTML.
  return String(s || '').replace(/\r\n?/g, '\n').replace(/\*\*|__/g, '').replace(/\n{3,}/g, '\n\n').trim();
}

/** Friendly message; the technical error (proxy, TLS, …) only on request. */
function errorBox(message, detail) {
  return h('div.mk-error-box', null,
    h('span.mk-error', null, message),
    detail ? h('details.mk-error-details', null, h('summary', null, t('Details')), h('code', null, detail)) : null);
}

// ---- Linux check ("works on Linux") ---------------------------------------------------------

const VERDICTS = {
  works: () => ({ cls: 'ok', label: t('Tested on Linux'), text: t('Works on Linux') }),
  issues: () => ({ cls: 'warn', label: t('Issues on Linux'), text: t('Works on Linux, with issues') }),
  fails: () => ({ cls: 'bad', label: t('Fails on Linux'), text: t('Does not work on Linux') }),
  unsupported: () => ({ cls: 'bad', label: t('Not for Linux'), text: t('Does not run on Linux (macOS/Windows program)') }),
};

const CHECK_TITLES = {
  linux_runtime: () => t('Runs on Linux (not a macOS/Windows-only program)'),
  starts: () => t('Starts and connects'),
  actions: () => t('Its actions appear on the keys'),
  input: () => t('Key presses and dials work'),
  responds: () => t('Reacts to its actions'),
  inspector: () => t('Its settings open without errors'),
  log: () => t('No errors in its log'),
};

// Items of all sources carry: id, uuid (plugin UUID, if known), checkKey (key of a check
// without installing), name, org, summary, icon, thumbnail, version, state, isPlugin.
const isChecking = (item) => !!item.state?.checking || M.checking.has(item.uuid) || M.checking.has(item.checkKey);

function linuxBadge(item) {
  if (isChecking(item)) return h('span.mk-badge', null, spinner('small'), t('Testing on Linux…'));
  const c = item.state?.linux_check;
  const v = c && VERDICTS[c.verdict]?.();
  if (!v) return null;
  const when = c.tested_at ? new Date(c.tested_at).toLocaleDateString() : '';
  const by = c.source === 'community' ? t('tested by the TuxDeck plugin test') : t('tested on this computer');
  return h(`span.mk-badge.${v.cls}`, { title: `${t('Version {version}, tested {date}', { version: c.version || '?', date: when })} (${by})` }, v.label);
}

/** Detail panel section: verdict, single checks, "Test on Linux". */
function linuxSection(item) {
  if (item.isPlugin === false) return null;
  const report = M.checks[item.uuid];
  const v = report && VERDICTS[report.verdict]?.();
  const busy = isChecking(item);
  const mark = (ok) => (ok === true ? '✓' : ok === false ? '✗' : '–');
  const src = source();
  const canTest = item.state?.installed || src.canTest?.(item);
  return h('section.mk-section.mk-linux', null,
    h('h3', null, t('Linux check')),
    v ? h(`div.mk-linux-verdict.${v.cls}`, null, v.text,
      h('span.muted', null, ` · ${t('Version {version}, tested {date}', { version: report.version || '?', date: new Date(report.tested_at).toLocaleString() })}`),
      h('div.muted', null, report.community ? t('Result of the TuxDeck plugin test.') : t('Tested on this computer.'))) : null,
    report ? h('ul.mk-linux-checks', null, report.checks.map((c) => h('li', { class: c.ok === true ? 'ok' : c.ok === false ? 'bad' : 'skip' },
      h('span.mk-linux-mark', null, mark(c.ok)),
      h('span', null, CHECK_TITLES[c.id]?.() || c.id, c.detail && c.ok !== true ? h('span.mk-linux-detail', null, c.detail) : null)))) : null,
    !report && !busy ? h('p.mk-note', null, t('Not tested on Linux yet. The test runs the plugin in an isolated test instance (simulated Stream Deck, no access to your files) and checks that it starts, that its actions and settings work and that it reports no errors.')) : null,
    h('div.mk-linux-actions', null,
      h('button.btn', {
        disabled: busy || !canTest,
        title: canTest ? '' : src.canTestHint?.() || '',
        onclick: () => runCheck(item),
      }, busy ? spinner('small') : null, busy ? t('Testing… (about half a minute)') : report ? t('Test again') : t('Test on Linux'))));
}

async function runCheck(item) {
  const installed = item.state?.installed;
  const key = installed ? item.uuid : item.checkKey;
  M.checking.add(key);
  M.requested.add(key);
  refreshItem(item);
  try {
    if (installed) await api(`/api/plugins/${enc(item.uuid)}/linux-check`, { method: 'POST' });
    else await source().check(item);
  } catch (e) {
    M.checking.delete(key);
    refreshItem(item);
    toast(e.message, 'error');
  }
}

function refreshItem(item) {
  updateCard(item);
  if (detail.item && (detail.item.id === item.id)) renderDetail();
}

async function loadChecks() {
  try {
    const r = await api('/api/linux-checks');
    // Published results, overridden by the ones of this computer.
    const community = Object.fromEntries(Object.entries(r?.community || {}).map(([k, v]) => [k, { ...v, community: true }]));
    M.checks = { ...community, ...(r?.results || {}) };
    for (const k of r?.running || []) M.checking.add(k);
  } catch { /* badges come with the list anyway */ }
}

/** `linux-check` event of the daemon: a check started or finished. */
export function handleLinuxCheckEvent(ev) {
  if (ev.running) M.checking.add(ev.key);
  else M.checking.delete(ev.key);
  const r = ev.result;
  // Only checks started here get a toast (not every plugin of a bulk run).
  const mine = !ev.running && (M.requested.delete(ev.key) || (r?.uuid && M.requested.delete(r.uuid)));
  if (r?.uuid) {
    M.checks[r.uuid] = r;
    const name = r.name || r.uuid;
    const v = VERDICTS[r.verdict]?.();
    if (v && mine) toast(t('Linux check of "{name}": {result}', { name, result: v.text }), v.cls === 'ok' ? 'info' : 'warn', 8000);
  } else if (ev.error && mine) {
    toast(t('Linux check failed: {error}', { error: ev.error }), 'error', 8000);
  }
  for (const it of M.items) {
    if (it.checkKey === ev.key || it.uuid === ev.key || (r && it.uuid === r.uuid)) {
      it.state = { ...(it.state || {}), checking: !!ev.running, ...(r ? { linux_check: { verdict: r.verdict, version: r.version, tested_at: r.tested_at } } : {}) };
      refreshItem(it);
    }
  }
  if (M.tab === 'installed' && M.open && !ev.running) loadInstalled();
}

function stateBadge(state) {
  if (!state?.installed) return null;
  if (state.runs_on_linux === false) {
    return h('span.mk-badge.warn', { title: state.reason || t('Does not run on Linux') }, t('Installed – does not run on Linux'));
  }
  return h('span.mk-badge.ok', null, t('Installed'));
}

// ---------------------------------------------------------------------------------------------
// view skeleton
// ---------------------------------------------------------------------------------------------

export function initMarket(root, hk) {
  M.root = root;
  hooks = hk || {};

  el.tabs = h('div.mk-tabs', { role: 'tablist' }, TABS.map(({ id, label }) =>
    h('button.mk-tab', { role: 'tab', dataset: { tab: id }, onclick: () => setTab(id) }, t(label))));
  el.search = h('input.text.mk-search', { type: 'search', spellcheck: 'false', autocomplete: 'off' });
  const searchDebounced = debounce(() => {
    const v = el.search.value.trim();
    if (v === M.search) return;
    M.search = v;
    reload();
  }, 350);
  el.search.addEventListener('input', searchDebounced);
  el.search.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') searchDebounced.flush();
  });
  el.sort = h('select.select.mk-sort', {
    title: t('Sort order'),
    onchange: () => { M.ordering = el.sort.value; reload(); },
  });
  el.sourceHeader = h('div.mk-account');
  el.fileInput = h('input', { type: 'file', accept: '.streamDeckPlugin,.streamDeckProfile,.streamDeckIconPack', multiple: true, hidden: true });
  el.fileInput.addEventListener('change', async () => {
    const files = [...el.fileInput.files];
    el.fileInput.value = '';
    for (const f of files) await importFile(f);
    refreshAfterInstall();
  });

  el.grid = h('div.mk-grid');
  el.more = h('button.btn.mk-more', { onclick: () => loadMore() }, t('Load more'));
  el.status = h('div.mk-status');
  el.sentinel = h('div.mk-sentinel');
  el.scroll = h('div.mk-scroll', null, el.grid, el.status, el.more, el.sentinel);
  el.detail = h('aside.mk-detail', { hidden: true, 'aria-label': t('Details') });
  el.body = h('div.mk-body', null, el.scroll, el.detail);

  root.replaceChildren(
    h('div.mk-header', null,
      h('button.btn.small.mk-close', { title: t('Close (Esc)'), onclick: () => closeMarket() }, '← ' + t('Back')),
      h('div.mk-title', null, bagIcon(18), t('Plugins')),
      el.tabs,
      el.search,
      el.sort,
      h('div.spacer'),
      el.sourceHeader,
      h('button.btn.small', { onclick: () => el.fileInput.click(), title: t('Install a .streamDeckPlugin, .streamDeckProfile or .streamDeckIconPack file') }, t('Install from file…')),
      h('button.btn.small', { onclick: () => elgatoHelp() }, t('Elgato Marketplace…')),
      el.fileInput),
    el.body);

  new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting)) maybeLoadMore();
  }, { root: el.scroll, rootMargin: '300px' }).observe(el.sentinel);

  addEventListener('keydown', (e) => {
    if (!M.open || e.key !== 'Escape' || document.querySelector('dialog[open]')) return;
    if (e.target.closest?.('input, textarea, select') && e.target !== el.search) return;
    e.preventDefault();
    if (source()?.onEscape?.()) return;
    if (M.detailId) closeDetail();
    else closeMarket();
  });

  for (const s of SOURCES) s.init?.(sourceApi);
  addEventListener('hashchange', syncHash);
  syncHash();
  renderTabs();
}

/** What sources may use from the view. */
const sourceApi = {
  refresh: () => (M.open ? refreshAfterInstall() : null),
  install: (item) => install(item),
  rerenderDetail: () => renderDetailButton(),
};

export function bagIcon(size = 16) {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('width', size);
  svg.setAttribute('height', size);
  svg.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS(ns, 'path');
  p.setAttribute('fill', 'currentColor');
  p.setAttribute('d', 'M20.5 11H19V7c0-1.1-.9-2-2-2h-4V3.5a2.5 2.5 0 0 0-5 0V5H4c-1.1 0-1.99.9-1.99 2v3.8H3.5c1.5 0 2.7 1.2 2.7 2.7S5 16.2 3.5 16.2H2V20c0 1.1.9 2 2 2h3.8v-1.5c0-1.5 1.2-2.7 2.7-2.7s2.7 1.2 2.7 2.7V22H17c1.1 0 2-.9 2-2v-4h1.5a2.5 2.5 0 0 0 0-5z');
  svg.append(p);
  return svg;
}

/** How to get plugins of the Elgato Marketplace: download on the website, install the file. */
async function elgatoHelp() {
  const link = h('a', { href: ELGATO_MARKETPLACE, target: '_blank', rel: 'noopener noreferrer' }, 'marketplace.elgato.com ↗');
  const body = h('div.mk-help', null,
    h('ol', null,
      h('li', null, t('Open the Elgato Marketplace and sign in:'), ' ', link),
      h('li', null, t('Open the product page and click "Get". Close the window that wants to open the official app.')),
      h('li', null, t('In your account under "Products", download the file with the download icon.')),
      h('li', null, t('Here, click "Install from file…" and choose the file – or drop it onto this window.'))),
    h('p.mk-note', null, t('Plugins protected with DRM only run in the official Elgato app; they cannot be installed here. Plugins that only contain a macOS or Windows program do not run on Linux either – the Linux check shows what works.')));
  const r = await modal(t('Plugins from the Elgato Marketplace'), body, [
    { value: 'close', label: t('Close') },
    { value: 'file', label: t('Install from file…'), primary: true },
  ]);
  if (r === 'file') el.fileInput.click();
}

function syncHash() {
  if (location.hash === '#market') openMarket();
  else if (M.open) closeMarket();
}

export function isMarketOpen() {
  return M.open;
}

export function openMarket() {
  if (!M.root) return;
  if (location.hash !== '#market') history.replaceState(null, '', '#market');
  if (M.open) return;
  M.open = true;
  M.root.hidden = false;
  document.body.classList.add('market-open');
  for (const s of SOURCES) s.onOpen?.();
  loadChecks();
  if (M.tab === 'installed') loadInstalled();
  else if (!M.items.length || M.error) reload();
  else refreshStates();
  el.search.focus({ preventScroll: true });
}

export function closeMarket() {
  if (location.hash === '#market') history.replaceState(null, '', location.pathname + location.search);
  if (!M.open) return;
  M.open = false;
  M.root.hidden = true;
  document.body.classList.remove('market-open');
  for (const s of SOURCES) s.onClose?.();
}

/** {"event":"market","type":"installed"|"open"|…} (other types are for the sources) */
export async function handleMarketEvent(ev) {
  switch (ev.type) {
    case 'open':
      openMarket();
      if (ev.hint === 'elgato') elgatoHelp();
      break;
    case 'installed':
      if (M.open) refreshAfterInstall();
      break;
    default:
      for (const s of SOURCES) await s.onEvent?.(ev);
      break;
  }
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

function setTab(tab) {
  if (tab === M.tab) return;
  M.tab = tab;
  M.ordering = null;
  closeDetail();
  renderTabs();
  if (tab === 'installed') loadInstalled();
  else reload();
}

function renderTabs() {
  for (const b of el.tabs.children) {
    const on = b.dataset.tab === M.tab;
    b.classList.toggle('active', on);
    b.setAttribute('aria-selected', String(on));
  }
  const inst = M.tab === 'installed';
  const src = source();
  el.search.hidden = inst;
  el.sort.hidden = inst || !src?.sorts?.length;
  el.search.placeholder = t(src?.searchPlaceholder || 'Search…');
  if (!inst && src?.sorts?.length) {
    M.ordering ??= src.sorts[0][0];
    el.sort.replaceChildren(...src.sorts.map(([v, l]) => h('option', { value: v, selected: v === M.ordering }, t(l))));
  }
  el.sourceHeader.replaceChildren(...kids(inst ? null : src?.header?.()));
}

function query(offset, limit) {
  return { tab: M.tab, search: M.search, ordering: M.ordering || '', offset, limit };
}

async function reload() {
  M.items = [];
  M.count = 0;
  M.error = null;
  el.grid.replaceChildren();
  el.scroll.scrollTop = 0;
  await loadMore();
}

function maybeLoadMore() {
  if (M.open && M.tab !== 'installed' && !M.loading && !M.error && M.items.length < M.count) loadMore();
}

async function loadMore() {
  if (M.tab === 'installed') return;
  const seq = ++M.seq;
  M.loading = true;
  M.error = null;
  renderStatus();
  try {
    const r = await source().list(query(M.items.length, LIMIT));
    if (seq !== M.seq) return;
    const items = Array.isArray(r?.items) ? r.items : [];
    M.count = Number(r?.count) || 0;
    const known = new Set(M.items.map((i) => i.id));
    const fresh = items.filter((i) => !known.has(i.id));
    M.items.push(...fresh);
    el.grid.append(...fresh.map(card));
    if (!items.length) M.count = M.items.length; // defensive: stop paging
  } catch (e) {
    if (seq !== M.seq) return;
    M.error = e.message;
    M.errorLocal = !e.status; // our own service is not reachable, not the catalogue
  } finally {
    if (seq === M.seq) {
      M.loading = false;
      renderStatus();
      // Fill the viewport if the first page does not cause scrolling.
      requestAnimationFrame(() => {
        if (el.scroll.scrollHeight <= el.scroll.clientHeight + 300) maybeLoadMore();
      });
    }
  }
}

function renderStatus() {
  const more = !M.error && M.items.length < M.count;
  el.more.hidden = M.loading || !more || M.tab === 'installed';
  el.status.replaceChildren();
  if (M.tab === 'installed') return;
  if (M.loading) el.status.append(spinner(), h('span', null, t('Loading…')));
  else if (M.error) {
    el.status.append(M.errorLocal ? errorBox(M.error)
      : errorBox(t(source().errorText || 'The catalogue is not reachable. Check the internet connection and try again.'), M.error),
      h('button.btn.small', { onclick: () => (M.items.length ? loadMore() : reload()) }, t('Try again')));
  } else if (!M.items.length) {
    el.status.append(h('span.muted', null, M.search ? t('No results for "{q}".', { q: M.search }) : t('No entries.')));
  } else if (!more) {
    el.status.append(h('span.muted', null, tn(M.count, '{n} entry', '{n} entries')));
  }
}

function card(item) {
  const node = h('div.mk-card', {
    role: 'button', tabIndex: 0, dataset: { id: item.id },
    title: item.summary || item.name || '',
    onclick: () => openDetail(item),
    onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openDetail(item); } },
  });
  fillCard(node, item);
  return node;
}

function fillCard(node, item) {
  const busy = M.installing.has(item.id);
  node.classList.toggle('selected', M.detailId === item.id);
  node.replaceChildren(...kids(
    h('div.mk-card-top', null,
      remoteImg([item.icon, item.thumbnail], item.name, 'mk-card-icon'),
      h('div.mk-card-head', null,
        h('div.mk-card-name', null, item.name || '–'),
        h('div.mk-card-org', null, item.org || ''))),
    item.summary ? h('div.mk-card-summary', null, plainText(item.summary)) : null,
    h('div.mk-card-foot', null,
      source().badges?.(item),
      h('span.spacer'),
      busy ? h('span.mk-badge', null, spinner('small'), t('Installing…')) : stateBadge(item.state),
      busy ? null : linuxBadge(item))));
}

function updateCard(item) {
  const node = [...el.grid.children].find((n) => n.dataset.id === item.id);
  if (node) fillCard(node, item);
}

function mergeItem(updated) {
  const it = M.items.find((i) => i.id === updated.id);
  if (it) {
    Object.assign(it, { state: updated.state });
    updateCard(it);
  }
  return it;
}

/** Re-fetch the install state of all loaded items. */
async function refreshStates() {
  if (M.tab === 'installed' || !M.items.length) return;
  const seq = M.seq;
  const total = M.items.length;
  try {
    for (let off = 0; off < total; off += 60) {
      const r = await source().list(query(off, Math.min(60, total - off)));
      if (seq !== M.seq) return;
      for (const it of r?.items || []) mergeItem(it);
    }
  } catch { /* keep old state */ }
}

// ---------------------------------------------------------------------------------------------
// detail panel
// ---------------------------------------------------------------------------------------------

let detail = { item: null, full: null, error: null };

function closeDetail() {
  M.detailId = null;
  M.detailSeq++;
  detail = { item: null, full: null, error: null };
  el.detail.hidden = true;
  el.detail.replaceChildren();
  for (const n of el.grid.querySelectorAll('.mk-card.selected')) n.classList.remove('selected');
}

async function openDetail(item) {
  M.detailId = item.id;
  const seq = ++M.detailSeq;
  detail = { item, full: null, error: null };
  for (const n of el.grid.children) n.classList.toggle('selected', n.dataset.id === item.id);
  el.detail.hidden = false;
  renderDetail();
  el.detail.scrollTop = 0;
  try {
    const full = await source().detail(item);
    if (seq !== M.detailSeq) return;
    detail.full = full;
    if (full?.state) mergeItem(full);
  } catch (e) {
    if (seq !== M.detailSeq) return;
    detail.error = e.message;
  }
  renderDetail();
}

export function fmtDate(s) {
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? null : d.toLocaleDateString(locale(), { day: '2-digit', month: '2-digit', year: 'numeric' });
}

/** The item as shown in the detail panel (list entry + details + current state). */
function detailItem() {
  const listed = M.items.find((i) => i.id === detail.item.id);
  return detail.full ? { ...detail.item, ...detail.full, state: (listed || detail.full).state } : detail.item;
}

function renderDetail() {
  if (!detail.item) return;
  const item = detailItem();
  const full = detail.full;
  const src = source();
  const meta = [];
  for (const [label, value] of full ? src.meta?.(item, full) || [] : []) {
    if (value) meta.push(h('dt', null, label), h('dd', null, value));
  }
  if (full && item.state?.installed) meta.push(h('dt', null, t('Installed')), h('dd', null, item.state.installed_version || t('yes')));
  const links = (src.links?.(item, full) || []).filter((l) => safeUrl(l?.url));

  el.detailButton = h('div.mk-install');
  el.detail.replaceChildren(...kids(
    h('div.mk-detail-head', null,
      h('button.icon-btn.mk-detail-close', { title: t('Close'), onclick: closeDetail }, '×'),
      remoteImg([item.thumbnail, item.icon], item.name, 'mk-detail-banner'),
      h('div.mk-detail-title', null,
        remoteImg([item.icon, item.thumbnail], item.name, 'mk-detail-icon'),
        h('div', null,
          h('div.mk-detail-name', null, item.name || '–'),
          h('div.mk-detail-org', null, item.org || ''),
          h('div.mk-detail-badges', null, src.badges?.(item), stateBadge(item.state), linuxBadge(item))))),
    el.detailButton,
    linuxSection(item),
    item.state?.installed && item.state.runs_on_linux === false && item.state.reason
      ? h('div.mk-warnbox', null, t('Does not run on Linux: {reason}', { reason: item.state.reason })) : null,
    !full && !detail.error ? h('div.mk-status', null, spinner(), h('span', null, t('Loading details…'))) : null,
    detail.error ? h('div.mk-status', null, errorBox(t('The details are not available right now.'), detail.error)) : null,
    meta.length ? h('dl.mk-meta', null, meta) : null,
    full?.description || item.summary ? h('section.mk-section', null,
      h('h3', null, t('Description')),
      h('div.mk-text', null, plainText(full?.description || item.summary))) : null,
    full?.release_notes ? h('section.mk-section', null,
      h('h3', null, t('Release notes')),
      h('div.mk-text', null, plainText(full.release_notes))) : null,
    links.length ? h('section.mk-section', null,
      h('h3', null, t('Links')),
      h('ul.mk-links', null, links.map((l) => h('li', null, h('a', { href: safeUrl(l.url), target: '_blank', rel: 'noopener noreferrer' }, l.label || l.url))))) : null));
  renderDetailButton();
}

function renderDetailButton() {
  const box = el.detailButton;
  if (!box || !detail.item) return;
  const item = detailItem();
  const st = item.state || {};
  const busy = M.installing.has(item.id);
  const upToDate = st.installed && (st.kind === 'profile' || (item.version && st.installed_version === item.version));
  let button = { label: st.installed ? t('Update') : t('Install'), action: () => install(item), disabled: busy || (!detail.full && !detail.error) };
  if (busy) button.label = t('Installing…');
  else if (upToDate) button = { label: t('Installed'), disabled: true };
  else button = source().installButton?.(item, st, button) || button;
  box.replaceChildren(...kids(
    h('div.mk-install-row', null,
      h('button.btn.primary.mk-install-btn', { disabled: button.disabled, onclick: button.action }, busy ? spinner('small light') : null, button.label),
      st.installed && !busy ? h('button.btn.danger-outline', { onclick: () => uninstall(item) }, t('Uninstall')) : null),
    source().notes?.(item, st),
    item.isPlugin !== false ? h('div.mk-note', null, t(LINUX_NOTE)) : null,
    busy ? h('div.mk-note', null, t('Download and installation can take a while…')) : null));
}

/** Remove an installed catalogue item (plugin or icon pack) from the detail panel. */
async function uninstall(item) {
  const st = item.state || {};
  const name = item.name || t('Plugin');
  const isPack = st.kind === 'iconpack';
  const yes = await confirmDialog(
    isPack ? t('Remove icon pack') : t('Remove plugin'),
    isPack ? t('Remove "{name}"? Icons already assigned to keys are kept.', { name })
      : t('Uninstall "{name}"? Keys with actions of this plugin will stop working.', { name }),
    t('Remove'));
  if (!yes) return;
  try {
    await api(isPack ? `/api/iconpacks/${enc(item.uuid || item.id)}` : `/api/plugins/${enc(item.uuid)}`, { method: 'DELETE' });
    toast(t('"{name}" removed', { name }), 'info', 3000);
    hooks.installed?.({ kind: isPack ? 'iconpack' : 'plugin', id: item.uuid || item.id, removed: true });
    delete M.checks[item.uuid];
  } catch (e) {
    toast(t('Removing failed: {msg}', { msg: e.message }), 'error');
    return;
  }
  await refreshAfterInstall(item.id);
}

async function install(item) {
  if (!item?.id || M.installing.has(item.id)) return;
  const name = item.name || t('Product');
  M.installing.add(item.id);
  // The Linux check that runs after installing reports back with a toast.
  if (item.uuid) M.requested.add(item.uuid);
  updateCard(M.items.find((i) => i.id === item.id) || item);
  if (detail.item?.id === item.id) renderDetailButton();
  try {
    const r = await source().install(item);
    toast(t('"{name}" installed', { name: r?.name || name }), 'info', 4000);
    if (r && r.runs_on_linux === false) {
      toast(r.reason ? t('"{name}" does not run on Linux: {reason}', { name: r.name || name, reason: r.reason }) : t('"{name}" does not run on Linux.', { name: r.name || name }), 'warn', 9000);
    }
    hooks.installed?.(r);
  } catch (e) {
    toast(t('Installing "{name}" failed: {msg}', { name, msg: e.message }), 'error', 9000);
  } finally {
    M.installing.delete(item.id);
    await refreshAfterInstall(item.id);
  }
}

async function refreshAfterInstall(id = detail.item?.id) {
  if (detail.item && id === detail.item.id) {
    try {
      const full = await source().detail(detail.item);
      if (detail.item?.id === id) {
        detail.full = full;
        if (full?.state) mergeItem(full);
        renderDetail();
      }
    } catch { renderDetailButton(); }
  } else if (id) {
    const it = M.items.find((i) => i.id === id);
    if (it) updateCard(it);
  }
  await refreshStates();
  if (M.tab === 'installed') loadInstalled();
}

// ---------------------------------------------------------------------------------------------
// installed plugins tab
// ---------------------------------------------------------------------------------------------

async function loadInstalled() {
  const seq = ++M.seq;
  M.loading = true;
  el.grid.replaceChildren();
  el.more.hidden = true;
  el.status.replaceChildren(spinner(), h('span', null, t('Loading…')));
  let list;
  try {
    list = await api('/api/plugins');
  } catch (e) {
    if (seq !== M.seq) return;
    M.loading = false;
    el.status.replaceChildren(h('span.mk-error', null, t('Could not load the plugins: {msg}', { msg: e.message })),
      h('button.btn.small', { onclick: loadInstalled }, t('Try again')));
    return;
  }
  let packs = [];
  try { packs = await api('/api/iconpacks'); } catch { /* older daemon */ }
  if (seq !== M.seq) return;
  M.loading = false;
  list = Array.isArray(list) ? list : [];
  packs = Array.isArray(packs) ? packs : [];
  const parts = [];
  if (list.length) parts.push(tn(list.length, '{n} plugin', '{n} plugins'));
  if (packs.length) parts.push(tn(packs.length, '{n} icon pack', '{n} icon packs'));
  el.status.replaceChildren(parts.length
    ? h('span.muted', null, t('{list} installed', { list: parts.join(', ') }))
    : h('span.muted', null, t('No additional plugins or icon packs installed.')));
  el.grid.replaceChildren(h('div.mk-installed', null, list.map(installedRow), packs.map(iconPackRow)));
}

function iconPackRow(p) {
  const name = (p.name || '').trim() || p.id;
  const remove = h('button.btn.small.danger-outline', {
    onclick: async () => {
      if (!await confirmDialog(t('Remove icon pack'), t('Remove "{name}"? Icons already assigned to keys are kept.', { name }), t('Remove'))) return;
      remove.disabled = true;
      try {
        await api(`/api/iconpacks/${enc(p.id)}`, { method: 'DELETE' });
        toast(t('"{name}" removed', { name }), 'info', 3000);
      } catch (e) {
        toast(t('Removing failed: {msg}', { msg: e.message }), 'error');
        remove.disabled = false;
        return;
      }
      loadInstalled();
      for (const it of M.items) if (it.uuid === p.id) it.state = { installed: false };
    },
  }, t('Remove'));
  return h('div.mk-plugin', null,
    p.icon ? h('img', { class: 'mk-plugin-icon', src: p.icon, alt: '', draggable: false })
      : h('div.mk-plugin-icon', null, (name[0] || '?').toUpperCase()),
    h('div.mk-plugin-main', null,
      h('div.mk-plugin-name', null, name, p.version ? h('span.muted', null, ` ${p.version}`) : null),
      h('div.mk-plugin-sub', null, [p.author, tn(p.count, '{n} icon', '{n} icons'), t('in the image editor under "Icon…"')].filter(Boolean).join(' · '))),
    h('span.mk-badge.ok', null, t('Icon pack')),
    remove);
}

function installedRow(p) {
  const name = (p.name || '').trim() || p.uuid;
  const ok = p.runs_on_linux !== false;
  const remove = h('button.btn.small.danger-outline', {
    onclick: async () => {
      const yes = await confirmDialog(t('Remove plugin'), t('Uninstall "{name}"? Keys with actions of this plugin will stop working.', { name }), t('Remove'));
      if (!yes) return;
      remove.disabled = true;
      try {
        await api(`/api/plugins/${enc(p.uuid)}`, { method: 'DELETE' });
        toast(t('"{name}" removed', { name }), 'info', 3000);
        hooks.installed?.({ kind: 'plugin', id: p.uuid, removed: true });
      } catch (e) {
        toast(t('Removing failed: {msg}', { msg: e.message }), 'error');
        remove.disabled = false;
        return;
      }
      loadInstalled();
      // Install badges in the catalogue are now stale.
      for (const it of M.items) if (it.uuid === p.uuid) it.state = { installed: false };
    },
  }, t('Remove'));
  return h('div.mk-plugin', null,
    typeof p.icon === 'string' && p.icon.startsWith('/plugin-files/')
      ? h('img', { class: 'mk-plugin-icon', src: p.icon, alt: '', draggable: false })
      : h('div.mk-plugin-icon', null, (name[0] || '?').toUpperCase()),
    h('div.mk-plugin-main', null,
      h('div.mk-plugin-name', null, name, p.version ? h('span.muted', null, ` ${p.version}`) : null),
      h('div.mk-plugin-sub', null, [p.author, p.uuid, Number.isFinite(p.actions) ? tn(p.actions, '{n} action', '{n} actions') : null].filter(Boolean).join(' · ')),
      !ok && p.reason ? h('div.mk-plugin-reason', null, p.reason) : null),
    installedCheck(p, ok),
    remove);
}

/** Linux check verdict of an installed plugin, with a button to (re)test it. */
function installedCheck(p, ok) {
  if (!ok) return h('span.mk-badge.warn', { title: p.reason || '' }, t('Does not run on Linux'));
  const busy = M.checking.has(p.uuid);
  const r = M.checks[p.uuid];
  const v = r && VERDICTS[r.verdict]?.();
  const badge = busy ? h('span.mk-badge', null, spinner('small'), t('Testing on Linux…'))
    : v ? h(`span.mk-badge.${v.cls}`, { title: r.checks.filter((c) => c.ok === false).map((c) => `${CHECK_TITLES[c.id]?.() || c.id}: ${c.detail}`).join('\n') }, v.label)
      : h('span.mk-badge.ok', null, t('Runs on Linux'));
  const test = h('button.btn.small', {
    disabled: busy,
    onclick: async () => {
      M.checking.add(p.uuid);
      M.requested.add(p.uuid);
      loadInstalled();
      try {
        await api(`/api/plugins/${enc(p.uuid)}/linux-check`, { method: 'POST' });
      } catch (e) {
        M.checking.delete(p.uuid);
        toast(e.message, 'error');
        loadInstalled();
      }
    },
  }, r ? t('Test again') : t('Test on Linux'));
  return h('span.mk-plugin-check', null, badge, test);
}
