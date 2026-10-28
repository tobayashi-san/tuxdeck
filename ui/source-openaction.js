// Catalogue of open-source plugins (OpenAction plugin registry, also used by OpenDeck).
// Packages come from the newest release of the author's GitHub repository.
import { api, enc, h } from './store.js';
import { t } from './i18n.js';

const norm = (it) => ({ ...it, uuid: it.id, checkKey: it.id, org: it.author, isPlugin: true });

export const openaction = {
  id: 'openaction',
  tabs: [{ id: 'catalogue', label: 'Plugins' }],
  sorts: [['', 'Catalogue'], ['name', 'Name']],
  searchPlaceholder: 'Search plugins…',
  errorText: 'The plugin catalogue is not reachable. Check the internet connection and try again.',

  async list({ search, ordering, offset, limit }) {
    const p = new URLSearchParams({ offset: String(offset), limit: String(limit) });
    if (search) p.set('search', search);
    if (ordering) p.set('ordering', ordering);
    const r = await api(`/api/catalogue?${p}`);
    return { items: (r?.items || []).map(norm), count: Number(r?.count) || 0 };
  },

  async detail(item) {
    return norm(await api(`/api/catalogue/${enc(item.id)}`));
  },

  install: (item) => api('/api/catalogue/install', { method: 'POST', body: { id: item.id } }),
  check: (item) => api('/api/catalogue/linux-check', { method: 'POST', body: { id: item.id } }),
  canTest: () => true,

  badges(item) {
    return item.ai_assisted ? h('span.mk-badge', { title: t('The author marked this plugin as made with the help of AI.') }, t('AI-assisted')) : null;
  },

  // No versions in the catalogue: an installed plugin can always be updated to the newest release.
  installButton(item, st, button) {
    return st.installed ? { ...button, label: t('Install newest version') } : button;
  },

  notes() {
    return h('div.mk-note', null, t('Open source: downloaded from the newest release on GitHub.'));
  },

  links(item) {
    return item.repository ? [{ url: item.repository, label: t('Source code and releases') }] : [];
  },
};
