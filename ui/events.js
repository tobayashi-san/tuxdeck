// Live events from the daemon (WebSocket) and reconnecting.

import { markCompatDirty, updateCompatBanner } from './compat.js';
import { S, slotKey } from './store.js';
import { handleLinuxCheckEvent, handleMarketEvent } from './market.js';
import { renderEditor, retryUnsaved } from './editor.js';
import { t } from './i18n.js';
import { toast } from './dialogs.js';
import { loadCatalogue, loadPage, loadState, schedulePageReload } from './data.js';
import { $, el } from './dom.js';
import { renderTopbar } from './topbar.js';

let wsRetry = 0;
let wsEverConnected = false;
const pressTimers = new Map();

function setConn(ok) {
  S.wsConnected = ok;
  el.conn.className = 'conn ' + (ok ? 'ok' : 'bad');
  el.conn.textContent = ok ? t('Live') : t('Disconnected – reconnecting…');
  el.conn.title = ok ? t('Connected to the TuxDeck service') : t('No connection to the service');
  if (S.serviceDown !== !ok) {
    S.serviceDown = !ok;
    renderTopbar();
    renderEditor();
  }
}

export function connectEvents() {
  let ws;
  try {
    ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/events`);
  } catch (e) {
    console.error(e);
    setTimeout(connectEvents, 3000);
    return;
  }
  ws.addEventListener('open', () => {
    const reconnect = wsEverConnected;
    wsEverConnected = true;
    wsRetry = 0;
    setConn(true);
    // Changes made while the service was away first, then reload what it has now.
    if (reconnect) retryUnsaved().finally(refreshAll);
  });
  ws.addEventListener('message', (m) => {
    let ev;
    try {
      ev = JSON.parse(m.data);
    } catch {
      return;
    }
    handleEvent(ev);
  });
  ws.addEventListener('close', () => {
    setConn(false);
    const delay = Math.min(10000, 500 * 2 ** wsRetry++);
    setTimeout(connectEvents, delay);
  });
}

export function sameId(a, b) {
  return a && b && String(a).toLowerCase() === String(b).toLowerCase();
}

function handleEvent(ev) {
  switch (ev?.event) {
    case 'state':
      loadState();
      break;
    case 'page':
      for (const k of [...S.pageCache.keys()]) {
        const [p, pg] = k.split('/');
        if (sameId(p, ev.profile) && sameId(pg, ev.page)) S.pageCache.delete(k);
      }
      if (sameId(ev.profile, S.view.profile)) markCompatDirty();
      if (sameId(ev.profile, S.view.profile) && sameId(ev.page, S.view.page)) schedulePageReload();
      break;
    case 'input':
      onInput(ev);
      break;
    case 'actions':
      markCompatDirty();
      loadCatalogue();
      updateCompatBanner();
      break;
    case 'market':
      handleMarketEvent(ev);
      break;
    case 'linux-check':
      handleLinuxCheckEvent(ev);
      break;
    case 'log':
      if (ev.level === 'warn' || ev.level === 'error') toast(ev.message || t('(empty message)'), ev.level, ev.level === 'error' ? 8000 : 5000);
      else console.info('[daemon]', ev.message);
      break;
    default:
      break;
  }
}

function onInput(ev) {
  if (!S.view.live || ev.device !== S.deviceId) return;
  const key = slotKey(ev.controller, ev.coord);
  const node = [...el.device.querySelectorAll('.slot')].find((s) => s.dataset.controller === ev.controller && s.dataset.coord === ev.coord);
  clearTimeout(pressTimers.get(key));
  if (ev.down) {
    S.pressed.add(key);
    node?.classList.add('pressed');
    // Safety: never keep a key glowing forever if the "up" event gets lost.
    pressTimers.set(key, setTimeout(() => {
      S.pressed.delete(key);
      node?.classList.remove('pressed');
    }, 10000));
  } else {
    S.pressed.delete(key);
    node?.classList.remove('pressed');
  }
}

async function refreshAll() {
  S.pageCache.clear();
  await Promise.all([loadCatalogue(), loadState()]);
  await loadPage();
}

addEventListener('unhandledrejection', (e) => {
  console.error(e.reason);
  toast(e.reason?.message || String(e.reason), 'error');
});
