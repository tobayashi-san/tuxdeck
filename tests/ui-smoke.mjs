#!/usr/bin/env node
// Browser tests of the web UI: a headless Chromium-based browser (Chrome, Chromium or
// Brave) drives the real UI of an isolated daemon over the DevTools protocol.
//
//   node tests/ui-smoke.mjs [--bin target/release/tuxdeck] [--browser <path>] [--screenshots <dir>] [--keep] [-v]
//
// Failing tests save a screenshot (kept in the temp dir or --screenshots).
//
// Fails on uncaught exceptions and console errors of our own UI (plugin pages are
// ignored). No dependencies besides Node.js >= 22.

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name, def) => (args.includes(name) ? args[args.indexOf(name) + 1] : def);
const BIN = path.resolve(opt('--bin', path.join(ROOT, 'target/release/tuxdeck')));
const KEEP = args.includes('--keep');
const VERBOSE = args.includes('-v');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Process groups of daemons and browsers: killed when the run is interrupted (Ctrl+C,
// timeout), otherwise they would keep running in the background.
const CHILDREN = new Set();
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(sig, () => {
    for (const c of CHILDREN) { try { process.kill(-c.pid, 'SIGKILL'); } catch { /* gone */ } }
    process.exit(130);
  });
}
const track = (proc) => { CHILDREN.add(proc); proc.on('exit', () => CHILDREN.delete(proc)); return proc; };

let passes = 0;
let failures = 0;
let shotDir = null;
let current = null; // browser of the running suite (screenshots of failures)
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assertion failed');
}
async function test(name, fn) {
  const t0 = Date.now();
  try {
    await fn();
    passes++;
    console.log(`  ok    ${name} (${Date.now() - t0} ms)`);
  } catch (e) {
    failures++;
    console.log(`  FAIL  ${name}\n        ${String(e.stack || e).split('\n').slice(0, 5).join('\n        ')}`);
    if (current && shotDir) {
      const shot = await current.send('Page.captureScreenshot', { format: 'png' }).catch(() => null);
      if (shot) {
        const f = path.join(shotDir, `${name.replace(/[^\w]+/g, '-').slice(0, 60)}.png`);
        fs.writeFileSync(f, Buffer.from(shot.data, 'base64'));
        console.log(`        screenshot: ${f}`);
      }
    }
  }
}

async function freePort() {
  return new Promise((res) => {
    const s = net.createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
  });
}

function findBrowser() {
  const candidates = [opt('--browser'), process.env.BROWSER, 'google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'brave'];
  for (const c of candidates.filter(Boolean)) {
    try {
      return execFileSync('sh', ['-c', `command -v "${c}"`]).toString().trim() || null;
    } catch { /* next */ }
  }
  return null;
}

// ------------------------------------------------------------------------------- daemon

class Daemon {
  constructor(model, dir) {
    this.model = model;
    this.dir = dir;
    this.lines = [];
  }

  async start() {
    const data = path.join(this.dir, 'data');
    const config = path.join(this.dir, 'config');
    if (!this.port) {
      fs.mkdirSync(data, { recursive: true });
      fs.mkdirSync(config, { recursive: true });
      fs.mkdirSync(path.join(this.dir, 'home'), { recursive: true });
      this.port = await freePort();
      this.sdk = await freePort();
      fs.writeFileSync(path.join(config, 'settings.json'), JSON.stringify({ ui_port: this.port, sdk_port: this.sdk, checks_repo: '' }));
    }
    const env = {
      ...process.env,
      TUXDECK_VIRTUAL_DEVICE: this.model, TUXDECK_DRY_RUN: '1', TUXDECK_NO_URL_HANDLER: '1', TUXDECK_NO_WINDOW_WATCH: '1',
      TUXDECK_DATA_DIR: data, TUXDECK_CONFIG_DIR: config, HOME: path.join(this.dir, 'home'),
      RUST_LOG: 'info', NO_COLOR: '1', LANGUAGE: '', LC_ALL: '', LC_MESSAGES: '', LANG: 'C.UTF-8',
    };
    delete env.HYPRLAND_INSTANCE_SIGNATURE;
    this.proc = track(spawn(BIN, ['run', '--no-browser'], { env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] }));
    const onData = (b) => {
      for (const l of b.toString().split('\n').filter((x) => x.trim())) {
        this.lines.push(l);
        if (VERBOSE) console.log(`    [daemon] ${l}`);
      }
    };
    this.proc.stdout.on('data', onData);
    this.proc.stderr.on('data', onData);
    for (let i = 0; i < 100; i++) {
      try {
        this.token ??= fs.readFileSync(path.join(config, 'api-token'), 'utf8').trim();
        const st = await this.api('/api/state');
        if (st.devices.length && st.devices[0].profile) return;
      } catch { /* not up yet */ }
      await sleep(100);
    }
    throw new Error(`daemon did not start:\n${this.lines.slice(-20).join('\n')}`);
  }

  async stop() {
    if (this.proc && this.proc.exitCode === null) {
      try { process.kill(-this.proc.pid, 'SIGTERM'); } catch { /* gone */ }
      for (let i = 0; i < 50 && this.proc.exitCode === null; i++) await sleep(100);
      try { process.kill(-this.proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
  }

  url(p = '/') { return `http://127.0.0.1:${this.port}${p}`; }

  async api(p, method = 'GET', body) {
    const init = { method, headers: { 'x-tuxdeck-token': this.token } };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
      init.headers['content-type'] = 'application/json';
    }
    const r = await fetch(this.url(p), init);
    if (!r.ok) throw new Error(`${method} ${p} → ${r.status}`);
    return r.headers.get('content-type')?.includes('json') ? r.json() : r.arrayBuffer();
  }

  async page() {
    const d = (await this.api('/api/state')).devices[0];
    return this.api(`/api/pages/${d.profile}/${d.page}`);
  }

  async action(controller, coord) {
    const p = await this.page();
    return p.controllers.find((c) => c.Type === controller)?.Actions?.[coord];
  }
}

// ------------------------------------------------------------------------------ browser

class Browser {
  constructor(bin, dir) {
    this.bin = bin;
    this.dir = dir;
    this.errors = [];
    this.id = 0;
    this.pending = new Map();
  }

  async start() {
    fs.mkdirSync(this.dir, { recursive: true });
    this.proc = track(spawn(this.bin, [
      '--headless=new', '--remote-debugging-port=0', `--user-data-dir=${this.dir}`, '--window-size=1280,900',
      '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--no-sandbox', 'about:blank',
    ], { detached: true, stdio: 'ignore' }));
    // The browser writes its port into the profile when the DevTools server is up.
    const portFile = path.join(this.dir, 'DevToolsActivePort');
    for (let i = 0; i < 100 && !fs.existsSync(portFile); i++) await sleep(100);
    const port = fs.readFileSync(portFile, 'utf8').split('\n')[0];
    let targets = [];
    for (let i = 0; i < 50 && !targets.some((t) => t.type === 'page'); i++) {
      targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json().catch(() => []);
      await sleep(100);
    }
    this.ws = new WebSocket(targets.find((t) => t.type === 'page').webSocketDebuggerUrl);
    await new Promise((r, j) => { this.ws.onopen = r; this.ws.onerror = j; });
    this.ws.onmessage = (e) => this.onMessage(JSON.parse(e.data));
    await this.send('Runtime.enable');
    await this.send('Log.enable');
    await this.send('Page.enable');
    await this.send('Network.enable');
  }

  onMessage(m) {
    // API requests of the UI, for failure messages.
    if (m.method === 'Network.requestWillBeSent' && m.params.request.url.includes('/api/')) {
      (this.requests ??= []).push(`${m.params.request.method} ${new URL(m.params.request.url).pathname}${new URL(m.params.request.url).search}`);
    }
    if (this.quiet && m.method) return; // errors are expected right now
    if (m.id && this.pending.has(m.id)) {
      const { res, rej } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) rej(new Error(m.error.message));
      else res(m.result);
      return;
    }
    // Only our own UI counts (plugin property inspectors run their own code).
    const ours = (url) => !url || (url.startsWith(this.origin) && !/\/(plugins|plugin-files|plugin-host)\//.test(url));
    if (m.method === 'Runtime.exceptionThrown') {
      const d = m.params.exceptionDetails;
      if (ours(d.url || d.stackTrace?.callFrames?.[0]?.url)) this.errors.push(d.exception?.description || d.text);
    } else if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') {
      const url = m.params.stackTrace?.callFrames?.[0]?.url;
      if (ours(url)) this.errors.push('console.error: ' + m.params.args.map((a) => a.value ?? a.description).join(' '));
    } else if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error' && ours(m.params.entry.url)) {
      // Failed requests while the daemon is deliberately stopped are expected.
      if (!this.offline) this.errors.push('log: ' + m.params.entry.text + ' ' + (m.params.entry.url || ''));
    }
  }

  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => this.pending.set(id, { res, rej }));
  }

  async eval(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(`eval failed: ${r.exceptionDetails.exception?.description || r.exceptionDetails.text}\n${expr}`);
    return r.result.value;
  }

  async waitFor(expr, what = expr, ms = 5000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (await this.eval(expr).catch(() => false)) return;
      await sleep(50);
    }
    throw new Error(`timeout waiting for ${what}`);
  }

  async open(url) {
    this.origin = new URL(url).origin;
    await this.send('Page.navigate', { url });
    await this.waitFor(`document.readyState === 'complete' && document.querySelectorAll('#device .slot').length > 0`, 'device grid');
  }

  async key(key, { code = key, keyCode = 0, modifiers = 0 } = {}) {
    for (const type of ['keyDown', 'keyUp']) {
      await this.send('Input.dispatchKeyEvent', { type, key, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode, modifiers });
    }
  }

  async stop() {
    try { this.ws?.close(); } catch { /* closed */ }
    if (this.proc && this.proc.exitCode === null) {
      try { process.kill(-this.proc.pid, 'SIGTERM'); } catch { /* gone */ }
      for (let i = 0; i < 30 && this.proc.exitCode === null; i++) await sleep(100);
      try { process.kill(-this.proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
  }
}

const slotSel = (controller, coord) => `#device .slot[data-controller="${controller}"][data-coord="${coord}"]`;
const KEYS = { Enter: 13, Escape: 27, Delete: 46, F10: 121 };

// -------------------------------------------------------------------------------- suites

async function suiteNeo(d, b) {
  await b.open(d.url(`/?token=${d.token}`));

  await test('loads: device grid, live connection, no errors', async () => {
    assert(await b.eval(`document.querySelectorAll('#device .slot[data-controller="Keypad"]').length`) === 8, '8 keys');
    await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'live indicator');
  });

  await test('editor: a typed title is saved', async () => {
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').click()`);
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field');
    await b.eval(`(() => { const t = document.querySelector('#editor .ed-titlecol textarea'); t.focus(); t.value = 'UI test'; t.dispatchEvent(new Event('input', { bubbles: true })); t.blur(); })()`);
    for (let i = 0; i < 40 && (await d.action('Keypad', '0,0'))?.States?.[0]?.Title !== 'UI test'; i++) await sleep(50);
    assert((await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'UI test', 'title saved');
  });

  await test('catalogue: Enter places the focused action on the selected key', async () => {
    const p = await d.page();
    await d.api(`/api/pages/${p.profile}/${p.id}/slot?controller=Keypad&coord=1,1`, 'DELETE');
    await b.eval(`document.querySelector('${slotSel('Keypad', '1,1')}').click()`);
    await b.eval(`document.querySelector('.action-item[data-uuid="com.elgato.streamdeck.system.website"]').focus()`);
    await b.key('Enter', { keyCode: KEYS.Enter });
    for (let i = 0; i < 40 && !(await d.action('Keypad', '1,1')); i++) await sleep(50);
    assert((await d.action('Keypad', '1,1'))?.UUID === 'com.elgato.streamdeck.system.website', 'placed');
  });

  await test('drag & drop: an action dropped onto a key', async () => {
    const p = await d.page();
    await d.api(`/api/pages/${p.profile}/${p.id}/slot?controller=Keypad&coord=2,1`, 'DELETE');
    await b.waitFor(`!document.querySelector('${slotSel('Keypad', '2,1')}').classList.contains('has-action')`, 'empty slot', 2000).catch(() => {});
    await b.eval(`(() => {
      const dt = new DataTransfer();
      const src = document.querySelector('.action-item[data-uuid="com.elgato.streamdeck.system.text"]');
      const dst = document.querySelector('${slotSel('Keypad', '2,1')}');
      src.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
      dst.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt }));
      dst.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt }));
      src.dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt }));
    })()`);
    for (let i = 0; i < 40 && !(await d.action('Keypad', '2,1')); i++) await sleep(50);
    assert((await d.action('Keypad', '2,1'))?.UUID === 'com.elgato.streamdeck.system.text', 'dropped');
  });

  await test('keyboard: Shift+F10 opens the slot menu, Escape closes it and keeps the selection', async () => {
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').click()`);
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').focus()`);
    await b.key('F10', { keyCode: KEYS.F10, modifiers: 8 });
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'context menu');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.querySelector('.ctxmenu')`, 'menu closed');
    assert(await b.eval(`!!document.querySelector('#device .slot.selected')`), 'still selected');
  });

  await test('delete with the keyboard, undo from the toast', async () => {
    const before = await d.action('Keypad', '1,1');
    assert(before, 'action to delete');
    await b.eval(`document.querySelector('${slotSel('Keypad', '1,1')}').click()`);
    await b.eval(`document.querySelector('${slotSel('Keypad', '1,1')}').focus()`);
    await b.key('Delete', { keyCode: KEYS.Delete });
    for (let i = 0; i < 40 && (await d.action('Keypad', '1,1')); i++) await sleep(50);
    assert(!(await d.action('Keypad', '1,1')), 'deleted');
    await b.waitFor(`!!document.querySelector('.toast-action')`, 'undo button');
    await b.eval(`document.querySelector('.toast-action').click()`);
    for (let i = 0; i < 40 && !(await d.action('Keypad', '1,1')); i++) await sleep(50);
    assert((await d.action('Keypad', '1,1'))?.UUID === before.UUID, `restored (last writes: ${(b.requests || []).filter((r) => !r.startsWith('GET')).slice(-4).join(' | ')})`);
  });

  await test('pages: add a page and switch to it', async () => {
    const st = await d.api('/api/state');
    const n = st.profiles.find((p) => p.id === st.devices[0].profile).pages.length;
    await b.eval(`document.querySelector('.add-page').click()`);
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab').length === ${n + 1}`, 'new page tab');
    const st2 = await d.api('/api/state');
    assert(st2.profiles.find((p) => p.id === st2.devices[0].profile).pages.length === n + 1, 'page created');
    await b.eval(`document.querySelectorAll('#pagebar .page-tab')[0].click()`);
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab')[0].classList.contains('active')`, 'first page active');
  });

  await test('settings: switch the language to German and back', async () => {
    const setLang = async (lang) => {
      await b.eval(`document.getElementById('settings-btn').click()`);
      await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings dialog');
      // The daily backup made at startup is listed.
      await b.waitFor(`document.querySelectorAll('#backup-list li button').length > 0`, 'backup list');
      await b.eval(`(() => { const s = document.getElementById('set-language'); s.value = '${lang}'; s.dispatchEvent(new Event('change', { bubbles: true })); })()`);
      // The UI reloads with the new dictionary.
      await sleep(500);
      await b.waitFor(`document.readyState === 'complete' && document.querySelectorAll('#device .slot').length > 0`, 'reload');
    };
    await setLang('de');
    await b.waitFor(`document.getElementById('action-search').placeholder.startsWith('Aktionen')`, 'German UI');
    await setLang('en');
    await b.waitFor(`document.getElementById('action-search').placeholder.startsWith('Search')`, 'English UI');
  });

  await test('offline: an edit made while the service is down is saved after reconnecting', async () => {
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').click()`);
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field');
    b.offline = true;
    await d.stop();
    await b.waitFor(`!document.getElementById('conn-indicator').classList.contains('ok')`, 'disconnected');
    await b.eval(`(() => { const t = document.querySelector('#editor .ed-titlecol textarea'); t.focus(); t.value = 'Offline edit'; t.dispatchEvent(new Event('input', { bubbles: true })); t.blur(); })()`);
    await sleep(800);
    await d.start();
    await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'reconnected', 15000);
    for (let i = 0; i < 60 && (await d.action('Keypad', '0,0'))?.States?.[0]?.Title !== 'Offline edit'; i++) await sleep(100);
    b.offline = false;
    assert((await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'Offline edit', 'queued edit saved');
  });
}

async function suitePlus(d, b) {
  await test('without the token the UI explains how to open it', async () => {
    b.quiet = true;
    b.origin = d.url('').replace(/\/$/, '');
    await b.send('Page.navigate', { url: d.url('/') });
    await b.waitFor(`!!document.querySelector('.no-access')`, 'no-access notice');
    // With the token (as `tuxdeck ui` opens it) the UI works.
    await b.open(d.url(`/?token=${d.token}`));
    await sleep(300);
    b.quiet = false;
  });
  await test('plus: keys and dials render', async () => {
    assert(await b.eval(`document.querySelectorAll('#device .slot[data-controller="Keypad"]').length`) === 8, '8 keys');
    assert(await b.eval(`document.querySelectorAll('#device .slot[data-controller="Encoder"]').length`) === 4, '4 dials');
    await b.eval(`document.querySelector('${slotSel('Encoder', '0,0')}').click()`);
    await b.waitFor(`!!document.querySelector('#editor .ed-header')`, 'dial editor');
  });
}

// ---------------------------------------------------------------------------------- main

if (!fs.existsSync(BIN)) {
  console.error(`binary not found: ${BIN}`);
  process.exit(2);
}
const browserBin = findBrowser();
if (!browserBin) {
  console.error('no Chromium-based browser found (set BROWSER or --browser)');
  process.exit(2);
}
console.log(`TuxDeck UI tests – ${BIN} with ${browserBin}`);
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'sd-ui-'));
shotDir = opt('--screenshots', path.join(tmp, 'screenshots'));
fs.mkdirSync(shotDir, { recursive: true });
for (const [model, suite] of [['neo', suiteNeo], ['plus', suitePlus]]) {
  console.log(`\n# ${model}`);
  const d = new Daemon(model, path.join(tmp, model));
  const b = new Browser(browserBin, path.join(tmp, `${model}-browser`));
  try {
    await d.start();
    await b.start();
    current = b;
    await suite(d, b);
    await test(`${model}: no JS errors in the UI`, async () => {
      if (b.errors.length) throw new Error(b.errors.join('\n'));
    });
  } catch (e) {
    failures++;
    console.log(`  FAIL  ${model}: ${e.stack || e}`);
  } finally {
    await b.stop();
    await d.stop();
  }
}
if (!KEEP && !failures) fs.rmSync(tmp, { recursive: true, force: true });
console.log(`\n${passes} passed, ${failures} failed`);
process.exit(failures ? 1 : 0);
