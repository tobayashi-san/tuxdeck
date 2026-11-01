// Test plugin on the official @elgato/streamdeck library (bundled per major version).
import streamDeck, { SingletonAction } from '@elgato/streamdeck';

const log = (...a) => console.log(`lib${LIB_MAJOR}`, ...a);
const withTimeout = (p, ms) => Promise.race([p, new Promise((_, j) => setTimeout(() => j(new Error('timeout')), ms))]);

class Count extends SingletonAction {
  manifestId = `com.e2e.lib${LIB_MAJOR}.count`;

  onWillAppear(ev) {
    log('willAppear', ev.action.id, ev.payload.controller, JSON.stringify(ev.payload.settings));
  }

  async onKeyDown(ev) {
    const n = (ev.payload.settings.count ?? 0) + 1;
    await ev.action.setSettings({ count: n });
    const back = await withTimeout(ev.action.getSettings(), 3000);
    log('keyDown', n, JSON.stringify(back));
    await ev.action.setTitle(`n=${n}`);
    if (typeof ev.action.getResources === 'function') {
      const t = Date.now();
      try {
        const r = await withTimeout(ev.action.getResources(), 3000);
        log('resources', Date.now() - t, JSON.stringify(r));
      } catch (e) {
        log('resources failed', String(e));
      }
    }
  }

  onDidReceiveSettings(ev) {
    log('didReceiveSettings', JSON.stringify(ev.payload.settings));
  }

  onPropertyInspectorDidAppear(ev) {
    log('piAppear', ev.action.id);
  }

  async onSendToPlugin(ev) {
    log('sendToPlugin', JSON.stringify(ev.payload));
    // Answers the inspector's data source request (sdpi-components `datasource`).
    if (ev.payload?.event === 'getItems') {
      const items = { event: 'getItems', items: [{ label: 'Alpha', value: 'a' }, { label: 'Beta', value: 'b' }] };
      // Library 2 and 3: streamDeck.ui.sendToPropertyInspector; library 1: ui.current.
      if (typeof streamDeck.ui.sendToPropertyInspector === 'function') await streamDeck.ui.sendToPropertyInspector(items);
      else await streamDeck.ui.current?.sendToPropertyInspector(items);
    }
  }
}

if (LIB_MAJOR === 2) streamDeck.settings.useExperimentalMessageIdentifiers = true;
streamDeck.actions.registerAction(new Count());
streamDeck.settings.onDidReceiveGlobalSettings((ev) => log('globalChanged', JSON.stringify(ev.settings)));
await streamDeck.connect();
log('connected', JSON.stringify(streamDeck.info?.application ?? null));
log('global', JSON.stringify(await withTimeout(streamDeck.settings.getGlobalSettings(), 3000)));
if (typeof streamDeck.system?.getSecrets === 'function') {
  try {
    log('secrets', JSON.stringify(await withTimeout(streamDeck.system.getSecrets(), 3000)));
  } catch (e) {
    log('secrets failed', String(e));
  }
}
