// Icon packs (.streamDeckIconPack): picker for the image editor and pack management.
import { api, enc, h, debounce } from './store.js';
import { modal, toast, confirmDialog } from './dialogs.js';
import { t, tn } from './i18n.js';

const PAGE = 300; // icons per request (the service allows up to 500)

async function listPacks() {
  try {
    return await api('/api/iconpacks');
  } catch (e) {
    toast(e.message, 'error');
    return [];
  }
}

/** Fetch an icon and return it as data URL (SVG kept as is, raster images ≤ 288 px). */
async function iconDataUrl(item, toDataUrl) {
  const r = await fetch(item.url);
  if (!r.ok) throw new Error(t('Could not load the icon ({status})', { status: r.status }));
  const blob = await r.blob();
  const name = item.path.split('/').pop() || 'icon.png';
  return toDataUrl(new File([blob], name, { type: blob.type }));
}

/**
 * Let the user pick an icon from an installed pack.
 * `toDataUrl(file)` converts the chosen file (editor's imageFileToDataUrl).
 * Resolves with a data URL or null.
 */
export async function pickIcon(toDataUrl) {
  const packs = await listPacks();
  if (!packs.length) {
    const r = await modal(t('Icon packs'), h('p.modal-text', null,
      t('No icon packs are installed. Import a .streamDeckIconPack file, for example one downloaded from the Elgato Marketplace website.')), [
      { label: t('Close'), value: 'cancel' },
      { label: t('Import…'), value: 'import', primary: true },
    ]);
    if (r === 'import') await importIconPack();
    return null;
  }
  let items = [];
  let total = 0;
  let seq = 0;
  const packSel = h('select.select', { 'aria-label': t('Icon pack') },
    packs.map((p) => h('option', { value: p.id }, `${p.name} (${p.count})`)));
  const search = h('input.text', { type: 'search', placeholder: t('Search (name or tag)'), spellcheck: 'false' });
  const grid = h('div.icon-grid', { role: 'listbox', 'aria-label': t('Icons') });
  const info = h('span.field-help');
  const more = h('button.btn.small', { type: 'button', hidden: true, onclick: () => load(true) });
  const cell = (it, i) => h('button.icon-cell', { type: 'submit', value: `icon:${i}`, title: [it.name, ...(it.tags || [])].join(' · ') },
    h('img', { src: it.url, alt: it.name, loading: 'lazy', draggable: false }),
    h('span', null, it.name));
  // Pages of PAGE icons; "Show more" appends the next page.
  const load = async (append = false) => {
    const my = ++seq;
    if (append) more.disabled = true;
    else {
      items = [];
      grid.replaceChildren(h('div.muted', null, t('Loading…')));
      more.hidden = true;
    }
    try {
      const r = await api(`/api/iconpacks/${enc(packSel.value)}/icons?offset=${items.length}&limit=${PAGE}&search=${encodeURIComponent(search.value)}`);
      if (my !== seq) return;
      const start = items.length;
      const got = r.items || [];
      items.push(...got);
      total = r.total || 0;
      if (!append) grid.replaceChildren();
      grid.append(...got.map((it, i) => cell(it, start + i)));
      if (!items.length) grid.replaceChildren(h('div.muted', null, t('No results.')));
      info.textContent = total > items.length ? t('{shown} of {total} icons', { shown: items.length, total }) : tn(total, '{n} icon', '{n} icons');
      more.hidden = items.length >= total || !got.length;
      more.textContent = t('Show {n} more', { n: Math.min(PAGE, total - items.length) });
      // Keyboard users continue with the first new icon.
      if (append) grid.children[start]?.focus();
    } catch (e) {
      if (my !== seq) return;
      if (append) toast(e.message, 'error');
      else grid.replaceChildren(h('div.muted', null, e.message));
    } finally {
      more.disabled = false;
    }
  };
  packSel.addEventListener('change', () => load());
  const searchLater = debounce(() => load(), 250);
  search.addEventListener('input', searchLater);
  // Enter in the search field must not close the dialog.
  search.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); searchLater.flush?.(); } });
  const body = h('div.icon-picker', null, h('div.icon-picker-bar', null, packSel, search), grid,
    h('div.icon-picker-foot', null, info, more));
  load();
  const r = await modal(t('Icon from an icon pack'), body, [{ label: t('Cancel'), value: 'cancel' }]);
  const m = /^icon:(\d+)$/.exec(r || '');
  if (!m) return null;
  return iconDataUrl(items[Number(m[1])], toDataUrl);
}

/** Choose and install a .streamDeckIconPack file. */
export function importIconPack() {
  return new Promise((resolve) => {
    const input = h('input', { type: 'file', accept: '.streamDeckIconPack', hidden: true });
    input.addEventListener('change', async () => {
      const f = input.files[0];
      input.remove();
      if (!f) return resolve(false);
      try {
        const r = await api('/api/import', {
          method: 'POST', raw: true, body: await f.arrayBuffer(),
          headers: { 'X-Filename': f.name.replace(/[^\x20-\x7e]/g, '_'), 'Content-Type': 'application/octet-stream' },
        });
        toast(t('Icon pack "{name}" installed', { name: r?.id }), 'info', 3000);
        resolve(true);
      } catch (e) {
        toast(t('Import failed: {msg}', { msg: e.message }), 'error');
        resolve(false);
      }
    });
    document.body.append(input);
    input.click();
  });
}

/** List installed packs with a remove button each. */
export async function manageIconPacks() {
  for (;;) {
    const packs = await listPacks();
    const body = h('div', null,
      packs.length
        ? h('ul.iconpack-list', null, packs.map((p) => h('li', null,
          p.icon ? h('img', { src: p.icon, alt: '', draggable: false }) : h('span.iconpack-noicon'),
          h('div', null, h('b', null, p.name), h('div.muted', null, `${p.author || t('unknown')} · ${t('Version {v}', { v: p.version || '?' })} · ${tn(p.count, '{n} icon', '{n} icons')}`)),
          h('button.btn.small.danger-outline', { type: 'submit', value: `del:${p.id}` }, t('Remove')))))
        : h('p.modal-text', null, t('No icon packs installed.')));
    const r = await modal(t('Icon packs'), body, [
      { label: t('Close'), value: 'cancel' },
      { label: t('Import…'), value: 'import', primary: true },
    ]);
    if (r === 'import') {
      await importIconPack();
      continue;
    }
    const m = /^del:(.+)$/.exec(r || '');
    if (!m) return;
    const p = packs.find((x) => x.id === m[1]);
    if (await confirmDialog(t('Remove icon pack'), t('Remove "{name}"? Icons already assigned to keys are kept.', { name: p?.name || m[1] }), t('Remove'))) {
      try {
        await api(`/api/iconpacks/${enc(m[1])}`, { method: 'DELETE' });
      } catch (e) {
        toast(e.message, 'error');
      }
    }
  }
}
