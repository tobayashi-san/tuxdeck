// Profiles from the official app on Windows/macOS: right after importing one, TuxDeck
// offers to convert it into a regular Linux profile; profiles imported earlier get a
// small banner with the same offer. Nobody else ever sees any of this.

import { S, api, enc, h } from './store.js';
import { t, tn } from './i18n.js';
import { modal, toast } from './dialogs.js';

let shownFor = null;

/** Check again on the next render (plugins or the profile changed). */
export function markCompatDirty() {
  shownFor = null;
}

const fetchPlan = (profile) => api(`/api/profiles/${enc(profile)}/conversion`);
const dismissKey = (profile) => `tuxdeck.convert-dismissed.${profile}`;

export async function updateCompatBanner() {
  const box = document.getElementById('compat-banner');
  const profile = S.view.profile;
  if (!box || shownFor === profile) return;
  shownFor = profile;
  box.hidden = true;
  if (!profile || localStorage.getItem(dismissKey(profile))) return;
  let plan;
  try {
    plan = await fetchPlan(profile);
  } catch {
    return;
  }
  if (S.view.profile !== profile || !plan.needed) return;
  box.replaceChildren(
    h('span.compat-text', null, t('This profile comes from Windows or macOS.')),
    h('button.btn.small.primary', { onclick: () => offerConversion(profile) }, t('Convert for Linux…')),
    h('button.compat-close', {
      title: t('Hide'),
      'aria-label': t('Hide'),
      onclick: () => { localStorage.setItem(dismissKey(profile), '1'); box.hidden = true; },
    }, '×'));
  box.hidden = false;
}

/** After importing a profile: offer the conversion if it needs one. */
export async function afterProfileImport(profile) {
  try {
    const plan = await fetchPlan(profile);
    if (plan.needed) await offerConversion(profile, plan);
  } catch { /* offline or gone: the banner offers it later */ }
}

function section(count, label, items) {
  if (!count) return null;
  const list = items?.length ? h('ul.convert-list', null, items.map((x) => h('li', null, x))) : null;
  return h('div.convert-item', null, h('div.convert-head', null, label), list);
}

/** One chip per distinct program (two Outlook keys show once). */
const uniqueApps = (list) => [...new Map(list.map((a) => [`${a.title}\n${a.target}`, a])).values()];
const appItem = (a) => h('span', null, a.title || a.target, a.title && a.title !== a.target ? h('span.convert-to', null, ` → ${a.target}`) : null);

async function offerConversion(profile, plan = null) {
  plan ??= await fetchPlan(profile);
  if (!plan.needed) return;
  const keys = (list) => list.reduce((n, x) => n + x.keys, 0);
  const body = h('div.convert', null,
    h('p.convert-intro', null, t('It was made with the Stream Deck app on Windows or macOS. TuxDeck can turn it into a regular Linux profile; titles and icons stay as they are.')),
    section(plan.programs.length, tn(plan.programs.length, '{n} key opens its Linux program', '{n} keys open their Linux programs'), uniqueApps(plan.programs).map(appItem)),
    section(plan.web.length, tn(plan.web.length, '{n} key opens the web version', '{n} keys open the web version'), uniqueApps(plan.web).map(appItem)),
    section(plan.folders.length, tn(plan.folders.length, '{n} key opens a folder', '{n} keys open a folder'), uniqueApps(plan.folders).map(appItem)),
    section(keys(plan.replaced), tn(keys(plan.replaced), '{n} key switches to a Linux action', '{n} keys switch to Linux actions'),
      plan.replaced.map((r) => `${r.name} · ${tn(r.keys, '{n} key', '{n} keys')}`)),
    section(plan.manual.length, tn(plan.manual.length,
      '{n} key has no Linux counterpart: it keeps its title and icon and waits for a command of your own',
      '{n} keys have no Linux counterpart: they keep their title and icon and wait for a command of your own'), [...new Set(plan.manual)]),
    plan.installs.length ? h('p.convert-note', null, t('Installs from the catalogue: {plugins}', { plugins: plan.installs.map((p) => p.name).join(', ') })) : null,
    plan.waiting ? h('p.convert-note', null, tn(plan.waiting,
      'The plugin catalogue can\'t be reached: {n} key stays as it is for now, TuxDeck offers it again later.',
      'The plugin catalogue can\'t be reached: {n} keys stay as they are for now, TuxDeck offers them again later.')) : null,
    h('p.convert-note', null, t('A backup is made first (Settings → Backups).')));
  const choice = await modal(t('Convert for Linux?'), body, [
    { label: t('Not now'), value: 'later' },
    { label: t('Convert'), value: 'convert', primary: true },
  ]);
  if (choice !== 'convert') return;
  toast(t('Converting…'), 'info', 2500);
  try {
    const r = await api(`/api/profiles/${enc(profile)}/conversion`, { method: 'POST' });
    toast(r.manual
      ? tn(r.manual, 'Profile converted. {n} key still needs a command.', 'Profile converted. {n} keys still need a command.')
      : t('Profile converted.'), 'info', 6000);
    if (r.failed?.length) toast(t('Could not install: {plugins}', { plugins: r.failed.join(', ') }), 'warn', 9000);
    if (r.left) toast(tn(r.left, '{n} key stays as it is for now; TuxDeck offers it again later.', '{n} keys stay as they are for now; TuxDeck offers them again later.'), 'info', 9000);
  } catch (e) {
    toast(e.message, 'error', 8000);
  }
  markCompatDirty();
  updateCompatBanner();
}
