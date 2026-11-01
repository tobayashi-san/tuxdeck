// Top bar: device and profile selection, brightness, profile menu, import, settings.

import { afterProfileImport } from './compat.js';
import { MODELS, S, api, compatibleModel, currentDevice, debounce, deckLabel, enc, h, liveDevice, modelName, profileById } from './store.js';
import { askText, confirmDialog, contextMenu, modal, restoreFocus, toast } from './dialogs.js';
import { flushEditor } from './editor.js';
import { manageIconPacks } from './iconpacks.js';
import { t } from './i18n.js';
import { loadCatalogue, loadState, syncView, visibleProfiles } from './data.js';
import { $, el } from './dom.js';
import { renderBackups } from './backups.js';

export function renderTopbar() {
  const devs = S.state.devices;
  const d = currentDevice();
  el.deviceSelect.replaceChildren(...(devs.length
    ? devs.map((x) => h('option', { value: x.id, selected: x.id === S.deviceId },
      `${deckLabel(x)}${x.connected ? '' : ' – ' + t('not connected')}`))
    : [h('option', { value: '' }, t('No device'))]));
  el.deviceSelect.disabled = !devs.length;
  // Without the service the last known device state may be stale.
  const unknown = d && S.serviceDown;
  el.deviceStatus.className = 'status-dot ' + (unknown ? 'unknown' : d?.connected ? 'on' : 'off');
  el.deviceStatus.title = unknown ? t('Device state unknown – no connection to the service')
    : d ? (d.connected ? `${t('Connected')}${d.firmware ? ' · Firmware ' + d.firmware : ''}${d.sleeping ? ' · ' + t('Sleeping') : ''}${d.smart ? ' · ' + t('Smart Profile active') : ''}` : t('not connected')) : t('No device – offline editing');
  el.deviceStatus.setAttribute('aria-label', el.deviceStatus.title);

  const profs = visibleProfiles();
  el.profileSelect.replaceChildren(...(profs.length
    ? profs.map((p) => h('option', { value: p.id, selected: p.id === S.view.profile },
      (d ? p.name : `${p.name} (${modelName(p.model)})`) + (p.app ? ` ⟳ ${p.app}` : '')))
    : [h('option', { value: '' }, t('No profile'))]));
  el.profileSelect.disabled = !profs.length;

  const live = d && d.connected && !S.serviceDown;
  // Nothing to light on the Pedal.
  const lit = live && d.key_px !== 0;
  el.brightness.disabled = !lit;
  if (document.activeElement !== el.brightness && d && typeof d.brightness === 'number') {
    el.brightness.value = d.brightness;
  }
  el.brightnessVal.textContent = lit ? `${el.brightness.value} %` : '–';
}

el.deviceSelect.addEventListener('change', () => {
  S.deviceId = el.deviceSelect.value || null;
  S.userPickedDevice = true;
  S.navStack = [];
  syncView();
});

el.profileSelect.addEventListener('change', async () => {
  const id = el.profileSelect.value;
  if (!id) return;
  flushEditor();
  const live = liveDevice();
  if (live) {
    try {
      await api(`/api/devices/${enc(live.id)}`, { method: 'POST', body: { profile: id } });
    } catch (e) {
      toast(e.message, 'error');
    }
    await loadState();
  } else {
    S.localProfile = id;
    S.localPage = null;
    S.navStack = [];
    await syncView();
  }
});

const sendBrightness = debounce(async (id, value) => {
  try {
    await api(`/api/devices/${enc(id)}`, { method: 'POST', body: { brightness: value } });
  } catch (e) {
    toast(e.message, 'error');
  }
}, 120);
el.brightness.addEventListener('input', () => {
  const d = currentDevice();
  el.brightnessVal.textContent = `${el.brightness.value} %`;
  if (d) {
    d.brightness = Number(el.brightness.value);
    sendBrightness(d.id, Number(el.brightness.value));
  }
});
el.brightness.addEventListener('change', () => sendBrightness.flush(currentDevice()?.id, Number(el.brightness.value)));

// ---- profile menu ----------------------------------------------------------------------------
el.profileMenuBtn.addEventListener('click', () => {
  const r = el.profileMenuBtn.getBoundingClientRect();
  const prof = profileById(S.view.profile);
  // Same rule as the service: a device must keep at least one profile of its model.
  const lastOfDevice = !!prof && S.state.profiles.filter((p) => p.model === prof.model).length <= 1
    && S.state.devices.some((d) => d.profile === prof.id);
  contextMenu(r.left, r.bottom + 4, [
    { label: t('New profile…'), action: createProfile },
    { label: t('Add a sample profile…'), action: addSampleProfile },
    { label: t('Rename…'), action: renameProfile, disabled: !prof },
    { label: prof?.app ? t('Smart Profile: {app}…', { app: prof.app }) : t('Smart Profile (per application)…'), action: smartProfile, disabled: !prof },
    {
      label: t('Delete…'), action: deleteProfile, disabled: !prof || lastOfDevice, danger: true,
      title: lastOfDevice ? t('This is the only profile for the device. Create another profile first.') : null,
    },
    '-',
    { label: t('Export'), action: exportProfile, disabled: !prof },
    { label: t('Import (profile / plugin / icon pack)…'), action: () => el.importInput.click() },
    { label: t('Manage icon packs…'), action: manageIconPacks },
  ]);
});

/** Sample profiles (Stream, Work, Play) made for Linux, with a preview of their keys. */
async function addSampleProfile() {
  const d = currentDevice();
  const model = d?.model || profileById(S.view.profile)?.model || '20GBJ9901';
  const list = await api('/api/templates');
  let chosen = list[0]?.id;
  const cards = list.map((tpl) => h('button.sample-card', {
    type: 'button',
    class: tpl.id === chosen ? 'selected' : null,
    onclick: (e) => {
      chosen = tpl.id;
      for (const c of e.currentTarget.parentElement.children) c.classList.toggle('selected', c === e.currentTarget);
    },
  },
  h('div.sample-keys', null, tpl.icons.slice(0, 8).map((src) => (src ? h('img', { src, alt: '' }) : h('span')))),
  h('div.sample-name', null, tpl.name),
  h('div.sample-desc', null, tpl.description)));
  const body = h('div.samples', null,
    h('p.modal-text', null, t('Ready-made profiles for Linux. Programs are taken from this computer; plugins come from the catalogue.')),
    h('div.sample-cards', null, cards));
  if (await modal(t('Add a sample profile'), body, [{ label: t('Cancel'), value: 'cancel' }, { label: t('Add'), value: 'add', primary: true }]) !== 'add') return;
  toast(t('Setting up the profile…'), 'info', 2500);
  try {
    const r = await api(`/api/templates/${enc(chosen)}`, { method: 'POST', body: { model } });
    if (r.failed?.length) toast(t('Could not install: {plugins}', { plugins: r.failed.join(', ') }), 'warn', 9000);
    await activateProfile(r.id);
  } catch (e) {
    toast(e.message, 'error', 8000);
  }
}

export async function createProfile() {
  const d = currentDevice();
  const curProf = profileById(S.view.profile);
  const nameInput = h('input.text', { type: 'text', value: t('New profile'), spellcheck: 'false' });
  const defModel = d?.model || curProf?.model || '20GBJ9901';
  const modelSel = h('select.select', null, Object.entries(MODELS).map(([m, info]) =>
    h('option', { value: m, selected: m === defModel }, `${info.name} (${m})`)));
  const body = h('div', null,
    h('label.modal-field', null, h('span', null, t('Name')), nameInput),
    d ? h('p.modal-text', null, t('For {device}', { device: deckLabel(d) }))
      : h('label.modal-field', null, h('span', null, t('Device type')), modelSel));
  const r = await modal(t('New profile'), body, [
    { label: t('Cancel'), value: 'cancel' }, { label: t('Create'), value: 'ok', primary: true },
  ]);
  if (r !== 'ok') return;
  const name = nameInput.value.trim() || t('New profile');
  const model = d ? d.model : modelSel.value;
  try {
    const p = await api('/api/profiles', { method: 'POST', body: { name, model } });
    await activateProfile(p?.id);
    toast(t('Profile "{name}" created', { name }), 'info', 2500);
  } catch (e) {
    toast(e.message, 'error');
  }
}

async function activateProfile(id) {
  if (!id) return loadState();
  const live = liveDevice();
  if (live) {
    await loadState();
    const p = profileById(id);
    if (p && compatibleModel(live.model, p.model)) {
      try {
        await api(`/api/devices/${enc(live.id)}`, { method: 'POST', body: { profile: id } });
      } catch (e) {
        toast(e.message, 'error');
      }
    }
  } else {
    S.localProfile = id;
    S.localPage = null;
  }
  await loadState();
}

async function renameProfile() {
  const prof = profileById(S.view.profile);
  if (!prof) return;
  const name = await askText(t('Rename profile'), t('Name'), prof.name, t('Rename'));
  if (!name || name === prof.name) return;
  try {
    await api(`/api/profiles/${enc(prof.id)}`, { method: 'PATCH', body: { name } });
  } catch (e) {
    toast(e.message, 'error');
  }
  await loadState();
}

/** Link the profile to an application: it is shown while that application has the focus. */
async function smartProfile() {
  const prof = profileById(S.view.profile);
  if (!prof) return;
  let wins = { active: '', windows: [], available: true };
  try { wins = await api('/api/windows'); } catch { /* optional */ }
  const listId = 'smart-apps-' + Date.now();
  const input = h('input.text', { type: 'text', value: prof.app || '', spellcheck: 'false', list: listId, placeholder: t('e.g. firefox') });
  const classes = [...new Set(wins.windows.map((w) => w.class))];
  const body = h('div', null,
    h('p.modal-text', null, t('The profile is shown automatically while a window of this application is active, and afterwards the previous profile returns. Enter the window class (Hyprland: "hyprctl clients", X11: "xprop WM_CLASS"); separate several applications with ";".')),
    h('label.modal-field', null, h('span', null, t('Application')), input),
    h('datalist', { id: listId }, classes.map((c) => h('option', { value: c }))),
    classes.length ? h('div.chip-row', null, classes.slice(0, 12).map((c) =>
      h('button.chip', { type: 'button', onclick: () => { input.value = c; input.focus(); } }, c))) : null,
    wins.active ? h('p.field-help', null, t('Currently active: {app}', { app: wins.active })) : null,
    // GNOME and KDE on Wayland don't tell other programs which window is active.
    wins.available === false ? h('p.modal-text.warn-text', null, t('On this desktop TuxDeck can\'t see which window is active, so Smart Profiles don\'t switch here. They work on Hyprland and in X11 sessions.')) : null);
  // With several decks that can show the profile: only one of them, or every one.
  const decks = S.state.devices.filter((x) => compatibleModel(x.model, prof.model));
  const deckSelect = h('select.select', null,
    h('option', { value: '', selected: !prof.device }, t('Every deck of this model')),
    decks.map((x) => h('option', { value: x.id, selected: x.id === prof.device }, t('Only {device}', { device: deckLabel(x) }))));
  if (decks.length > 1 || prof.device) body.append(h('label.modal-field', null, h('span', null, t('Deck')), deckSelect));
  const r = await modal(t('Smart Profile – {name}', { name: prof.name }), body, [
    { label: t('Cancel'), value: 'cancel' },
    ...(prof.app ? [{ label: t('Remove link'), value: 'clear' }] : []),
    { label: t('Save'), value: 'ok', primary: true },
  ]);
  if (r !== 'ok' && r !== 'clear') return;
  const app = r === 'clear' ? '' : input.value.trim();
  try {
    await api(`/api/profiles/${enc(prof.id)}`, { method: 'PATCH', body: { app, ...(decks.length > 1 || prof.device ? { device: deckSelect.value } : {}) } });
    toast(!app ? t('Link removed')
      : wins.available === false ? t('Saved. Smart Profiles don\'t switch on this desktop.')
        : t('"{profile}" is activated automatically for "{app}"', { profile: prof.name, app }), 'info', 3000);
  } catch (e) {
    toast(e.message, 'error');
  }
  await loadState();
}

async function deleteProfile() {
  const prof = profileById(S.view.profile);
  if (!prof) return;
  const ok = await confirmDialog(t('Delete profile'), t('Delete the profile "{name}" with all its pages and folders?', { name: prof.name }));
  if (!ok) return;
  try {
    await api(`/api/profiles/${enc(prof.id)}`, { method: 'DELETE' });
    if (S.localProfile === prof.id) S.localProfile = null;
  } catch (e) {
    toast(e.message, 'error');
  }
  await loadState();
}

function exportProfile() {
  const prof = profileById(S.view.profile);
  if (!prof) return;
  const a = h('a', { href: `/api/profiles/${enc(prof.id)}/export`, download: `${prof.name}.streamDeckProfile` });
  document.body.append(a);
  a.click();
  a.remove();
}

export const IMPORT_RE = /\.(streamDeckProfile|streamDeckPlugin|streamDeckIconPack)$/i;

el.importInput.addEventListener('change', () => {
  const f = el.importInput.files[0];
  el.importInput.value = '';
  if (f) importFile(f);
});

/** Import a profile, plugin or icon pack file (file dialog or dropped onto the window). */
export async function importFile(f) {
  if (!IMPORT_RE.test(f.name)) {
    toast(t('Only .streamDeckProfile, .streamDeckPlugin and .streamDeckIconPack files can be imported.'), 'error');
    return;
  }
  // HTTP header values must be Latin-1; replace anything else.
  const headerName = f.name.replace(/[^\x20-\x7e]/g, '_');
  try {
    toast(t('Importing {file}…', { file: f.name }), 'info', 2000);
    const buf = await f.arrayBuffer();
    const r = await api('/api/import', {
      method: 'POST', raw: true, body: buf,
      headers: { 'X-Filename': headerName, 'Content-Type': 'application/octet-stream', 'X-Linux-Check': '1' },
    });
    if (r?.kind === 'plugin') {
      if (r.runs_on_linux === false) {
        toast(r.reason ? t('"{name}" does not run on Linux: {reason}', { name: r.name, reason: r.reason }) : t('"{name}" does not run on Linux.', { name: r.name }), 'warn', 9000);
      } else toast(t('Plugin installed – it is now tested on Linux in the background'), 'info', 4000);
      await loadCatalogue();
    } else if (r?.kind === 'iconpack') {
      toast(t('Icon pack installed – find it in the image editor under "Icon…"'), 'info', 4000);
    } else {
      await loadState();
      const p = profileById(r?.id);
      const d = currentDevice();
      if (p && d && !compatibleModel(d.model, p.model)) {
        // Profiles are shown for the decks they fit: this one waits for its model.
        toast(t('"{name}" was made for the {model}: it shows up when such a deck is connected.', { name: p.name, model: modelName(p.model) }), 'warn', 10000);
        return;
      }
      toast(t('Profile imported'), 'info', 3000);
      await activateProfile(r?.id);
      if (r?.id) await afterProfileImport(r.id);
    }
  } catch (e) {
    toast(t('Import failed: {msg}', { msg: e.message }), 'error');
  }
}

// ---- settings dialog -------------------------------------------------------------------------
el.settingsBtn.addEventListener('click', () => {
  renderBackups();
  const st = S.state.settings || {};
  el.setCmd.checked = !!st.cmd_as_ctrl;
  el.setPlatform.value = st.platform || 'linux';
  // Each language in its own name (e.g. "Deutsch"); the list comes from the daemon.
  const langs = Array.isArray(st.ui_languages) ? st.ui_languages : ['en'];
  el.setLanguage.replaceChildren(el.setLanguage.options[0], ...langs.map((code) => {
    let name = code;
    try { name = new Intl.DisplayNames([code.replace('_', '-')], { type: 'language' }).of(code.replace('_', '-')) || code; } catch { /* keep code */ }
    return h('option', { value: code }, name.charAt(0).toUpperCase() + name.slice(1));
  }));
  el.setLanguage.value = st.language && (st.language === 'auto' || langs.includes(st.language)) ? st.language : 'auto';
  // Optional daemon settings: only shown when the daemon reports them.
  // Touch point color only matters for devices with touch points (Neo).
  el.setTouchRow.hidden = typeof st.touch_color !== 'string' || !S.state.devices.some((d) => d.touch_points > 0);
  if (!el.setTouchRow.hidden) el.setTouch.value = /^#[0-9a-f]{6}$/i.test(st.touch_color) ? st.touch_color.toLowerCase() : '#ffffff';
  el.setInjectorRow.hidden = typeof st.injector !== 'string';
  if (!el.setInjectorRow.hidden) {
    if (![...el.setInjector.options].some((o) => o.value === st.injector)) {
      el.setInjector.append(h('option', { value: st.injector }, st.injector));
    }
    el.setInjector.value = st.injector;
    const act = document.getElementById('set-injector-active');
    if (act) act.textContent = st.injector_active ? t('Active: {name}', { name: st.injector_active }) : '';
  }
  el.setDevices.replaceChildren(...(S.state.devices.length
    ? S.state.devices.map((d) => h('li', null,
      h('span.status-dot', { class: d.connected ? 'on' : 'off' }),
      h('b', null, deckLabel(d)),
      h('span.muted', null, ` ${d.model} · ${d.id}${d.firmware ? ' · FW ' + d.firmware : ''}`)))
    : [h('li.muted', null, t('No known devices.'))]));
  el.settingsDialog.showModal();
});
el.settingsDialog.addEventListener('close', () => restoreFocus(el.settingsBtn));
async function saveSetting(partial) {
  try {
    await api('/api/settings', { method: 'POST', body: partial });
    Object.assign(S.state.settings, partial);
  } catch (e) {
    toast(e.message, 'error');
  }
}
el.setCmd.addEventListener('change', () => saveSetting({ cmd_as_ctrl: el.setCmd.checked }));
el.setPlatform.addEventListener('change', () => saveSetting({ platform: el.setPlatform.value }));
el.setLanguage.addEventListener('change', async () => {
  await saveSetting({ language: el.setLanguage.value });
  location.reload();
});
el.setTouch.addEventListener('change', () => saveSetting({ touch_color: el.setTouch.value }));
el.setInjector.addEventListener('change', async () => {
  const want = el.setInjector.value;
  await saveSetting({ injector: want });
  // The daemon may silently refuse unknown values → show what is really active.
  try {
    const st = await api('/api/state');
    if (st?.settings?.injector && st.settings.injector !== want) {
      toast(t('"{name}" is not supported by the service.', { name: want }), 'warn');
      el.setInjector.value = st.settings.injector;
      S.state.settings.injector = st.settings.injector;
    }
    // The chosen backend may not be available (tool not installed): the service falls back.
    const active = st?.settings?.injector_active;
    const act = document.getElementById('set-injector-active');
    if (act) act.textContent = active ? t('Active: {name}', { name: active }) : '';
    if (active && want !== 'auto' && active !== want) {
      toast(t('"{name}" is not available, using {active}.', { name: want, active }), 'warn');
    }
    if (st?.settings) Object.assign(S.state.settings, st.settings);
  } catch { /* ignore */ }
});
