// Settings → Backups: download a backup, restore from a file or a stored backup.

import { api, enc, h } from './store.js';
import { confirmDialog, toast } from './dialogs.js';
import { t } from './i18n.js';
import { $ } from './dom.js';

const kindLabel = (k) => (k === 'daily' ? t('Daily backup') : k === 'before' ? t('Before restoring') : t('Backup'));

function sizeLabel(bytes) {
  return bytes >= 1e6 ? `${(bytes / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1e3))} kB`;
}

/** Fill the list of stored backups (called when the settings dialog opens). */
export async function renderBackups() {
  const list = $('backup-list');
  let items = [];
  try {
    items = await api('/api/backups');
  } catch (e) {
    list.replaceChildren(h('li.muted', null, e.message));
    return;
  }
  const date = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  list.replaceChildren(...(items.length ? items.map((b) => h('li', null,
    h('span.grow', { title: b.name }, `${date.format(new Date(b.created))} · ${kindLabel(b.kind)} · ${sizeLabel(b.size)}`),
    h('a.btn.small', { href: `/api/backups/${enc(b.name)}`, download: b.name }, t('Download')),
    h('button.btn.small', { type: 'button', onclick: () => restore(`/api/backups/${enc(b.name)}/restore`) }, t('Restore')))) : [h('li.muted', null, t('No backups yet.'))]));
}

async function restore(path, body) {
  const ok = await confirmDialog(t('Restore backup'),
    t('All profiles are replaced by the ones in the backup; plugins, plugin data and settings from the backup are taken over. The current state is backed up first.'),
    t('Restore'));
  if (!ok) return;
  toast(t('Restoring…'), 'info', 3000);
  try {
    const r = await api(path, body ? { method: 'POST', body, raw: true, headers: { 'Content-Type': 'application/octet-stream' } } : { method: 'POST' });
    toast(r?.message || t('Backup restored.'), 'info', 8000);
    // Language, profiles and catalogue may all have changed.
    setTimeout(() => location.reload(), 1200);
  } catch (e) {
    toast(e.message, 'error');
  }
}

$('backup-create').addEventListener('click', () => {
  const a = h('a', { href: `/api/backup?iconpacks=${$('backup-iconpacks').checked ? 1 : 0}`, download: '' });
  document.body.append(a);
  a.click();
  a.remove();
  toast(t('The backup is being created and downloaded.'), 'info');
  setTimeout(renderBackups, 2000);
});
$('backup-restore').addEventListener('click', () => $('backup-file').click());
$('backup-file').addEventListener('change', () => {
  const f = $('backup-file').files[0];
  $('backup-file').value = '';
  if (f) restore('/api/restore', f);
});
