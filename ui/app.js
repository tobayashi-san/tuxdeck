// TuxDeck configuration UI – main module.
//
// State model (see store.js `S`):
//  * S.state      – last GET /api/state (devices, profiles, settings)
//  * S.view       – what the canvas shows: {profile, page, live, folderDepth}
//      live == true  → the selected device is connected; the view follows the hardware
//                      (device.profile / device.page). Navigation = POST /api/devices/{id}.
//      live == false → offline editing; S.localProfile / S.localPage drive the view and
//                      navigation happens purely in the UI.
//  * S.navStack   – parent pages for "Zurück" when entering folders from the UI. If the
//                   hardware navigates on its own the stack is dropped and the parent is
//                   searched (openchild action whose Settings.ProfileUUID == folder id).
//  * S.page       – GET /api/pages/{profile}/{page} of the shown page.
//
// The UI is split into modules (see the imports below); this one wires them up,
// handles global shortcuts and boots.
import { OPENCHILD, S, api, instanceAt } from './store.js';
import { closeContextMenu } from './dialogs.js';
import { initEditor, renderEditor } from './editor.js';
import { initI18n, t, translateDom } from './i18n.js';
import { initMarket, isMarketOpen, openMarket } from './market.js';
import { afterWrite, copySlot, deleteSlot, pasteSlot, pressSlot, select, slotFromEvent } from './canvas.js';
import { setSidebarOpen } from './catalogue.js';
import { loadCatalogue, loadState } from './data.js';
import { $, el } from './dom.js';
import { connectEvents } from './events.js';
import { folderTarget, goToPage } from './navigation.js';
import './topbar.js';
import './dragdrop.js';

// ============================================================================================
// Keyboard shortcuts
// ============================================================================================

addEventListener('keydown', (e) => {
  const tgt = e.target;
  if (e.defaultPrevented || isMarketOpen()) return;
  if (e.key === 'Escape' && document.body.classList.contains('sidebar-open') && el.sidebar.contains(tgt)) {
    setSidebarOpen(false);
    el.sidebarBtn.focus();
    return;
  }
  if (tgt && (tgt.closest?.('input, textarea, select, [contenteditable], dialog') || tgt.isContentEditable)) return;
  if (document.querySelector('dialog[open]')) return;
  // A focused slot is what the keyboard user is working on.
  const focused = slotFromEvent(e);
  const sel = focused ? { controller: focused.controller, coord: focused.coord } : S.selected;
  const mod = e.ctrlKey || e.metaKey;
  if ((e.key === 'Delete' || e.key === 'Backspace') && sel) {
    e.preventDefault();
    deleteSlot(sel);
  } else if (mod && e.key.toLowerCase() === 'c' && sel) {
    e.preventDefault();
    copySlot(sel);
  } else if (mod && e.key.toLowerCase() === 'v' && sel) {
    e.preventDefault();
    pasteSlot(sel);
  } else if (e.key === 'Escape') {
    closeContextMenu();
    select(null);
    setSidebarOpen(false);
  } else if (e.key === 'Enter' && sel && !tgt.closest?.('button, a, [role=button]')) {
    const inst = instanceAt(sel.controller, sel.coord);
    if (inst?.UUID === OPENCHILD) goToPage(folderTarget(inst), { push: true });
  }
});


// ============================================================================================
// Boot
// ============================================================================================

$('market-btn').addEventListener('click', () => openMarket());
el.conn.className = 'conn';
(async () => {
  // The daemon knows the effective language (setting or system language); everything
  // that renders text is set up only after the dictionary is loaded.
  try {
    const st = await api('/api/state');
    await initI18n(st?.settings?.ui_language || 'en');
  } catch {
    await initI18n('en');
  }
  translateDom();
  initEditor(el.editor, {
    press: (sel) => pressSlot(sel),
    remove: (sel) => deleteSlot(sel),
    changed: () => afterWrite(),
  });
  initMarket($('market'), {
    // Plugin/profile installed or removed: refresh action catalogue and profiles
    // (the daemon also emits "actions"/"state"; this covers a missed event).
    installed: () => {
      loadCatalogue();
      loadState();
    },
  });
  el.conn.textContent = t('Connecting…');
  renderEditor();
  try {
    await Promise.all([loadCatalogue(), loadState()]);
  } finally {
    connectEvents();
  }
})();
