// Imported profiles: the banner above the deck and the "Make it work on Linux" dialog.
// The daemon reports what a profile needs (/api/profiles/{id}/compat): missing plugins
// grouped by what can be done about them, and the applications its Open keys start here.

import { S, api, enc, h } from './store.js';
import { t, tn } from './i18n.js';
import { modal, toast } from './dialogs.js';

let shownFor = null;

/** Fetch the report again on the next render (plugins or the profile changed). */
export function markCompatDirty() {
  shownFor = null;
}

const fetchReport = (profile) => api(`/api/profiles/${enc(profile)}/compat`);

/** Keys that don't work yet: replaceable, installable or unavailable plugin actions and
 * applications without a Linux counterpart. */
function openIssues(r) {
  const plugins = r.groups.reduce((n, g) => n + g.replace + g.install + g.unavailable + g.unknown, 0);
  return plugins + r.apps.filter((a) => a.kind === 'none').length;
}

export async function updateCompatBanner() {
  const box = document.getElementById('compat-banner');
  const profile = S.view.profile;
  if (!box || shownFor === profile) return;
  shownFor = profile;
  box.hidden = true;
  if (!profile) return;
  let r;
  try {
    r = await fetchReport(profile);
  } catch {
    return;
  }
  if (S.view.profile !== profile) return;
  const n = openIssues(r);
  const dismissKey = `tuxdeck.compat-dismissed.${profile}`;
  if (!n || localStorage.getItem(dismissKey) === String(n)) return;
  box.replaceChildren(
    h('span.compat-text', null, tn(n, '{n} key in this profile needs a Linux counterpart.', '{n} keys in this profile need a Linux counterpart.')),
    h('button.btn.small.primary', { onclick: () => openCompatDialog(profile) }, t('Make it work on Linux…')),
    h('button.compat-close', {
      title: t('Hide'),
      'aria-label': t('Hide'),
      onclick: () => { localStorage.setItem(dismissKey, String(n)); box.hidden = true; },
    }, '×'));
  box.hidden = false;
}

function groupRow(g, profile, refresh) {
  const plugin = g.plugin?.name;
  const keys = tn(g.keys, '{n} key', '{n} keys');
  let status;
  let button = null;
  const fix = async (label) => {
    button.disabled = true;
    button.textContent = t('Working…');
    try {
      const r = await api(`/api/profiles/${enc(profile)}/compat`, { method: 'POST', body: { group: g.group } });
      toast(r.changed ? tn(r.changed, '{n} key replaced.', '{n} keys replaced.') : t('{name} installed.', { name: plugin || g.name }), 'info');
      markCompatDirty();
      await refresh();
    } catch (e) {
      toast(e.message, 'error', 8000);
      button.disabled = false;
      button.textContent = label;
    }
  };
  if (g.replace) {
    const label = plugin ? t('Replace with {plugin}', { plugin }) : t('Use keyboard shortcuts');
    status = tn(g.replace, '{n} key can be replaced.', '{n} keys can be replaced.');
    button = h('button.btn.small.primary', { type: 'button', onclick: () => fix(label) }, label);
  } else if (g.install) {
    const label = t('Install {plugin}', { plugin });
    status = t('The same plugin is in the catalogue.');
    button = h('button.btn.small.primary', { type: 'button', onclick: () => fix(label) }, label);
  } else if (g.builtin) {
    status = t('Works: TuxDeck does this itself.');
  } else if (g.unknown) {
    status = t('Not in the catalogue. If you have the plugin file, install it under Plugins → Install from file.');
  } else {
    status = t('Not available on Linux.');
  }
  const rest = (g.replace || g.builtin) && g.unavailable ? tn(g.unavailable, '{n} key has no counterpart.', '{n} keys have no counterpart.') : null;
  const ok = !g.replace && !g.install && !g.unavailable && !g.unknown;
  return h('div.compat-row', { class: ok ? 'ok' : g.replace || g.install ? 'fix' : 'missing' },
    h('div.compat-main', null,
      h('div.compat-name', null, g.name, h('span.compat-keys', null, keys)),
      h('div.compat-status', null, [status, rest].filter(Boolean).join(' ')),
      g.hint && (g.unavailable || !g.replace) ? h('div.compat-hint', null, g.hint) : null),
    button);
}

function appRow(a) {
  const target = a.kind === 'none' ? t('not installed') : a.kind === 'web' ? t('{target} (web)', { target: a.target }) : a.target;
  return h('div.compat-app', { class: a.kind === 'none' ? 'missing' : 'ok' },
    h('span.compat-app-title', null, (a.title || a.path).replace(/\s+/g, ' ')),
    h('span.compat-arrow', null, '→'),
    h('span.compat-app-target', null, target));
}

export async function openCompatDialog(profile = S.view.profile) {
  const body = h('div.compat-dialog');
  const render = (r) => {
    const parts = [h('p.compat-intro', null, t('Profiles from Windows and macOS use plugins and programs that work differently here. This is what TuxDeck can do about them.'))];
    if (r.groups.length) {
      parts.push(h('h3', null, t('Plugins')));
      for (const g of r.groups) parts.push(groupRow(g, profile, refresh));
    }
    if (r.apps.length) {
      parts.push(h('h3', null, t('Applications')));
      parts.push(h('p.compat-intro', null, t('Keys that open Windows or macOS programs start the Linux version, or the web version where there is none.')));
      parts.push(h('div.compat-apps', null, r.apps.map(appRow)));
    }
    if (!r.groups.length && !r.apps.length) parts.push(h('p', null, t('Everything in this profile works on Linux.')));
    body.replaceChildren(...parts);
  };
  const refresh = async () => render(await fetchReport(profile));
  await refresh();
  await modal(t('Make it work on Linux'), body, [{ label: t('Close'), value: 'close', primary: true }]);
  markCompatDirty();
  updateCompatBanner();
}
