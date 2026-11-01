#!/usr/bin/env node
// Browser tests of the web UI: a headless Chromium-based browser (Chrome, Chromium or
// Brave) drives the real UI of an isolated daemon over the DevTools protocol.
//
//   node tests/ui-smoke.mjs [--bin target/release/tuxdeck] [--browser <path>] [--screenshots <dir>]
//                           [--only neo,profiles,market,plus,mini,xl] [--keep] [-v]
//
// Failing tests save a screenshot (kept in the temp dir or --screenshots).
//
// Every test fails on uncaught exceptions and console errors of our own UI that happen
// while it runs (plugin pages are ignored). A local stand-in for the plugin catalogue
// keeps everything offline. No dependencies besides Node.js >= 22.

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name, def) => (args.includes(name) ? args[args.indexOf(name) + 1] : def);
const BIN = path.resolve(opt('--bin', path.join(ROOT, 'target/release/tuxdeck')));
const ONLY = opt('--only', '').split(',').filter(Boolean);
const KEEP = args.includes('--keep');
const VERBOSE = args.includes('-v');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const J = JSON.stringify;
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
let tmp = null;
let current = null; // browser of the running suite (screenshots of failures, JS errors)
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assertion failed');
}
function eq(a, b, msg) {
  if (J(a) !== J(b)) throw new Error(`${msg || 'expected equal'}: ${J(a)} !== ${J(b)}`);
}
async function test(name, fn) {
  const t0 = Date.now();
  const errors0 = current ? current.errors.length : 0;
  try {
    await fn();
    // Every test must leave the UI without (new) JS errors.
    if (current && current.errors.length > errors0) throw new Error(`JS errors in the UI:\n${current.errors.slice(errors0).join('\n')}`);
    passes++;
    console.log(`  ok    ${name} (${Date.now() - t0} ms)`);
  } catch (e) {
    failures++;
    console.log(`  FAIL  ${name}\n        ${String(e.stack || e).split('\n').slice(0, 6).join('\n        ')}`);
    if (current && shotDir) {
      const shot = await current.send('Page.captureScreenshot', { format: 'png' }).catch(() => null);
      if (shot) {
        const f = path.join(shotDir, `${name.replace(/[^\w]+/g, '-').slice(0, 60)}.png`);
        fs.writeFileSync(f, Buffer.from(shot.data, 'base64'));
        console.log(`        screenshot: ${f}`);
      }
    }
    // Leave no dialog or menu open for the next test.
    await current?.eval(`(() => { document.querySelectorAll('dialog[open]').forEach((d) => d.close()); document.querySelector('.ctxmenu')?.remove(); })()`).catch(() => {});
  }
}

/** Poll an async condition (daemon side) until it returns something truthy. */
async function until(fn, what, ms = 5000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      last = await fn();
      if (last) return last;
    } catch { /* retry */ }
    await sleep(50);
  }
  throw new Error(`timeout waiting for ${what}`);
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

/** Minimal ZIP writer (stored entries). */
function zip(files) {
  const parts = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of Object.entries(files)) {
    const data = Buffer.isBuffer(content) ? content : Buffer.from(content);
    const nameBuf = Buffer.from(name);
    const crc = zlib.crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    parts.push(local, nameBuf, data);
    const c = Buffer.alloc(46);
    c.writeUInt32LE(0x02014b50, 0);
    c.writeUInt16LE(20, 4);
    c.writeUInt16LE(20, 6);
    c.writeUInt32LE(crc, 16);
    c.writeUInt32LE(data.length, 20);
    c.writeUInt32LE(data.length, 24);
    c.writeUInt16LE(nameBuf.length, 28);
    c.writeUInt32LE(offset, 42);
    central.push(c, nameBuf);
    offset += 30 + nameBuf.length + data.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(Object.keys(files).length, 8);
  end.writeUInt16LE(Object.keys(files).length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, cd, end]);
}

const SVG = (color) => `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144"><rect width="144" height="144" fill="${color}"/></svg>`;

/** Write a file for the browser's file chooser (temp dir of this run). */
function tmpFile(name, content) {
  const dir = path.join(tmp, 'files');
  fs.mkdirSync(dir, { recursive: true });
  const f = path.join(dir, name);
  fs.writeFileSync(f, content);
  return f;
}

// Test plugin with a property inspector (registers like the SDK libraries do).
const PLUGIN = zip({
  'com.ui.counter.sdPlugin/manifest.json': J({
    Name: 'UI Counter', UUID: 'com.ui.counter', Version: '1.0', Author: 'ui-test', SDKVersion: 2, CodePath: 'plugin.js', Category: 'UI Test',
    Icon: 'img', OS: [{ Platform: 'mac', MinimumVersion: '10.15' }], Nodejs: { Version: '20' },
    Actions: [{ UUID: 'com.ui.counter.count', Name: 'Counter', Icon: 'img', States: [{ Image: 'img' }], PropertyInspectorPath: 'pi/pi.html', Controllers: ['Keypad'] }],
  }),
  'com.ui.counter.sdPlugin/plugin.js': `const a = process.argv; const arg = (n) => a[a.indexOf(n) + 1];
const ws = new WebSocket('ws://127.0.0.1:' + arg('-port'));
ws.onopen = () => ws.send(JSON.stringify({ event: arg('-registerEvent'), uuid: arg('-pluginUUID') }));`,
  'com.ui.counter.sdPlugin/img.svg': SVG('#2266aa'),
  // The inspector also tries what third-party inspector code must not do: use the UI's API.
  'com.ui.counter.sdPlugin/pi/pi.html': '<!doctype html><title>pi</title><body><p id="pi-ok">PI</p><script>function connectElgatoStreamDeckSocket(port, uuid, ev) {'
    + ' const ws = new WebSocket("ws://127.0.0.1:" + port); ws.onopen = async () => { ws.send(JSON.stringify({ event: ev, uuid }));'
    + ' const ui = new URL(document.referrer).origin; let probe;'
    + ' try { const r = await fetch(ui + "/api/state", { credentials: "include" }); probe = "status " + r.status; } catch { probe = "blocked"; }'
    + ' const own = (await fetch("/api/state")).status;'
    + ' ws.send(JSON.stringify({ event: "setSettings", context: uuid, payload: { probe, own } })); }; document.title = "connected"; }</script>',
});

// A plugin built on Elgato's @elgato/streamdeck 3 with a property inspector made of
// sdpi-components, whose list is filled by the plugin (tests/fixtures/elgato-sdk).
const SDK_FIXTURES = path.join(ROOT, 'tests/fixtures/elgato-sdk');
const SDPI_PLUGIN = () => zip({
  'com.e2e.lib3.sdPlugin/manifest.json': J({
    Name: 'Library Plugin', UUID: 'com.e2e.lib3', Version: '1.0', Author: 'ui-test', SDKVersion: 3, CodePath: 'plugin.mjs', Category: 'UI Test',
    Icon: 'img', OS: [{ Platform: 'mac', MinimumVersion: '10.15' }], Nodejs: { Version: '20' }, Software: { MinimumVersion: '7.1' },
    Actions: [{ UUID: 'com.e2e.lib3.count', Name: 'Library counter', Icon: 'img', States: [{ Image: 'img' }], PropertyInspectorPath: 'pi.html', Controllers: ['Keypad'] }],
  }),
  'com.e2e.lib3.sdPlugin/plugin.mjs': fs.readFileSync(path.join(SDK_FIXTURES, 'plugin-v3.mjs')),
  'com.e2e.lib3.sdPlugin/sdpi-components.js': fs.readFileSync(path.join(SDK_FIXTURES, 'sdpi-components.js')),
  'com.e2e.lib3.sdPlugin/img.svg': SVG('#884400'),
  // Picks "Beta" as soon as the plugin has delivered the list (a user would click it).
  'com.e2e.lib3.sdPlugin/pi.html': `<!doctype html><html><head><script src="sdpi-components.js"></script></head><body>
<sdpi-item label="Pick"><sdpi-select setting="pick" datasource="getItems" hot-reload></sdpi-select></sdpi-item>
<script>
const timer = setInterval(() => {
  const sel = document.querySelector('sdpi-select')?.shadowRoot?.querySelector('select');
  const opt = sel && [...sel.options].find((o) => o.textContent.trim() === 'Beta');
  if (!opt) return;
  clearInterval(timer);
  sel.value = opt.value;
  sel.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
}, 100);
</script></body></html>`,
});

const iconPack = (id, name, icons) => zip({
  [`${id}.sdIconPack/manifest.json`]: J({ Name: name, Author: 'ui-test', Version: '1.0.0', Icon: 'icon.svg', Description: 'Test pack' }),
  [`${id}.sdIconPack/icon.svg`]: SVG('#00aa00'),
  [`${id}.sdIconPack/icons.json`]: J(icons.map(([file, n, tags]) => ({ path: file, name: n, tags }))),
  ...Object.fromEntries(icons.map(([file], i) => [`${id}.sdIconPack/icons/${file}`, SVG(['#ff8800', '#00ff00', '#0000ff'][i % 3])])),
});
const ICONPACK = iconPack('com.ui.icons', 'UI Icons', [['train.svg', 'Train', ['travel']], ['salad.svg', 'Salad', ['food']], ['bike.svg', 'Bike', ['travel', 'sport']]]);
const ICONPACK2 = iconPack('com.ui.more', 'More Icons', [['star.svg', 'Star', ['shape']]]);

// ---------------------------------------------------------------------- fake catalogue
// Stands in for the OpenAction plugin catalogue so that no test talks to the real one:
// 30 entries ("Tool 30" … "Tool 02" in catalogue order, plus the installed test plugin).

const CHECKS = ['linux_runtime', 'starts', 'actions', 'input', 'responds', 'inspector', 'log'];
const report = (uuid, verdict, failing = []) => ({
  uuid, name: uuid, verdict, version: '1.0', tested_at: '2026-10-01T10:00:00Z',
  checks: CHECKS.map((id) => ({ id, ok: !failing.includes(id), detail: failing.includes(id) ? `${id} failed in the test` : '' })),
});
const CATALOGUE = {
  server: null,
  entries: {},
  async start() {
    this.port = await freePort();
    for (let i = 1; i < 30; i++) {
      const id = `com.ui.cat${String(i).padStart(2, '0')}`;
      this.entries[id] = {
        name: `Tool ${String(31 - i).padStart(2, '0')}`, author: i % 2 ? 'Ada' : 'Grace',
        repository: `https://github.com/ui-test/cat${i}`, description: i === 7 ? 'Draws a zebra on the key' : `Catalogue entry number ${i}`,
        flags: i === 3 ? { ai_assisted: true } : {},
      };
    }
    this.entries['com.ui.counter'] = { name: 'UI Counter', author: 'ui-test', repository: 'https://github.com/ui-test/counter', description: 'The installed test plugin' };
    this.server = http.createServer((req, res) => {
      const url = req.url.split('?')[0];
      if (url === '/catalogue.json') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(J(this.entries));
      } else if (url.startsWith('/icons/')) {
        res.writeHead(200, { 'content-type': 'image/svg+xml' });
        res.end(SVG('#445566'));
      } else {
        res.writeHead(404);
        res.end();
      }
    });
    await new Promise((r) => this.server.listen(this.port, '127.0.0.1', r));
  },
  env() {
    const base = `http://127.0.0.1:${this.port}`;
    return { TUXDECK_CATALOGUE_URL: `${base}/catalogue.json`, TUXDECK_GITHUB_API: base, TUXDECK_CATALOGUE_ICONS: `${base}/icons` };
  },
};

// ------------------------------------------------------------------------------- daemon

class Daemon {
  /** `prep(dataDir)` runs once before the first start (seed files). */
  constructor(model, dir, { prep } = {}) {
    this.model = model;
    this.dir = dir;
    this.prep = prep;
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
      fs.writeFileSync(path.join(config, 'settings.json'), J({ ui_port: this.port, sdk_port: this.sdk, checks_repo: '' }));
      this.prep?.(data);
    }
    const env = {
      ...process.env,
      TUXDECK_VIRTUAL_DEVICE: this.model, TUXDECK_DRY_RUN: '1', TUXDECK_NO_URL_HANDLER: '1', TUXDECK_NO_WINDOW_WATCH: '1',
      TUXDECK_DATA_DIR: data, TUXDECK_CONFIG_DIR: config, HOME: path.join(this.dir, 'home'),
      RUST_LOG: 'info', NO_COLOR: '1', LANGUAGE: '', LC_ALL: '', LC_MESSAGES: '', LANG: 'C.UTF-8',
      ...CATALOGUE.env(),
    };
    for (const k of ['HYPRLAND_INSTANCE_SIGNATURE', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME']) delete env[k];
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

  /** The UI with a one-time login code (like `tuxdeck ui`). */
  async loginUrl() { return this.url(`/?code=${(await this.api('/api/login-code', 'POST')).code}`); }

  async api(p, method = 'GET', body, headers = {}) {
    const init = { method, headers: { 'x-tuxdeck-token': this.token, ...headers } };
    if (Buffer.isBuffer(body)) init.body = body;
    else if (body !== undefined) {
      init.body = J(body);
      init.headers['content-type'] = 'application/json';
    }
    const r = await fetch(this.url(p), init);
    if (!r.ok) throw new Error(`${method} ${p} → ${r.status} ${await r.text()}`);
    return r.headers.get('content-type')?.includes('json') ? r.json() : Buffer.from(await r.arrayBuffer());
  }

  state() { return this.api('/api/state'); }
  async dev() { return (await this.state()).devices[0]; }

  async page(profile, page) {
    if (!profile) {
      const d = await this.dev();
      [profile, page] = [d.profile, d.page];
    }
    return this.api(`/api/pages/${profile}/${page}`);
  }

  async action(controller, coord) {
    const p = await this.page();
    return p.controllers.find((c) => c.Type === controller)?.Actions?.[coord];
  }

  async clear(controller, coord) {
    const d = await this.dev();
    await this.api(`/api/pages/${d.profile}/${d.page}/slot?controller=${controller}&coord=${coord}`, 'DELETE');
  }

  /** Path of a file (by name) somewhere in the data directory. */
  file(name, dir = path.join(this.dir, 'data')) {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) {
        const f = this.file(name, p);
        if (f) return f;
      } else if (e.name === name) return p;
    }
    return null;
  }

  mark() { return this.lines.length; }
  logsSince(mark, re) { return this.lines.slice(mark).filter((l) => re.test(l)); }
}

// ------------------------------------------------------------------------------ browser

class Browser {
  constructor(bin, dir) {
    this.bin = bin;
    this.dir = dir;
    this.errors = [];
    this.id = 0;
    this.pending = new Map();
    this.requests = [];
    this.responses = []; // {path, status, id, done}
    this.fileQueue = [];
    this.interceptors = [];
    this.downloads = path.join(dir, '..', `${path.basename(dir)}-downloads`);
  }

  async start() {
    fs.mkdirSync(this.dir, { recursive: true });
    fs.mkdirSync(this.downloads, { recursive: true });
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
    await this.send('DOM.enable');
    // File dialogs are answered from `fileQueue`; downloads land in `downloads`.
    await this.send('Page.setInterceptFileChooserDialog', { enabled: true });
    await this.send('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: this.downloads, eventsEnabled: true }).catch(() =>
      this.send('Page.setDownloadBehavior', { behavior: 'allow', downloadPath: this.downloads }));
  }

  onMessage(m) {
    const p = m.params;
    // API requests of the UI, for failure messages and waiting on them.
    if (m.method === 'Network.requestWillBeSent' && p.request.url.includes('/api/')) {
      const u = new URL(p.request.url);
      this.requests.push(`${p.request.method} ${u.pathname}${u.search}`);
    } else if (m.method === 'Network.responseReceived' && p.response.url.includes('/api/')) {
      const u = new URL(p.response.url);
      this.responses.push({ path: `${u.pathname}${u.search}`, status: p.response.status, id: p.requestId, done: false });
    } else if (m.method === 'Network.loadingFinished' || m.method === 'Network.loadingFailed') {
      for (const r of this.responses) if (r.id === p.requestId) r.done = true;
    } else if (m.method === 'Page.fileChooserOpened') {
      const files = this.fileQueue.shift();
      if (files) this.send('DOM.setFileInputFiles', { files, backendNodeId: p.backendNodeId }).catch((e) => this.errors.push(`file chooser: ${e.message}`));
      return;
    } else if (m.method === 'Fetch.requestPaused') {
      this.onIntercept(p);
      return;
    } else if (m.method === 'Page.javascriptDialogOpening') {
      // alert/confirm/"leave page?" would block the page (and every further command).
      this.errors.push(`unexpected ${p.type} dialog: ${p.message}`);
      this.send('Page.handleJavaScriptDialog', { accept: true }).catch(() => {});
      return;
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
      const d = p.exceptionDetails;
      if (ours(d.url || d.stackTrace?.callFrames?.[0]?.url)) this.errors.push(d.exception?.description || d.text);
    } else if (m.method === 'Runtime.consoleAPICalled' && p.type === 'error') {
      const url = p.stackTrace?.callFrames?.[0]?.url;
      if (ours(url)) this.errors.push('console.error: ' + p.args.map((a) => a.value ?? a.description).join(' '));
    } else if (m.method === 'Log.entryAdded' && p.entry.level === 'error' && ours(p.entry.url)) {
      // Failed requests while the daemon is deliberately stopped (or answered with an
      // error on purpose) are expected.
      if (!this.offline) this.errors.push('log: ' + p.entry.text + ' ' + (p.entry.url || ''));
    }
  }

  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(J({ id, method, params }));
    return new Promise((res, rej) => {
      // Never hang the whole run on one command.
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rej(new Error(`no answer to ${method} within 30 s`));
      }, 30000);
      this.pending.set(id, { res: (v) => { clearTimeout(timer); res(v); }, rej: (e) => { clearTimeout(timer); rej(e); } });
    });
  }

  async eval(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(`eval failed: ${r.exceptionDetails.exception?.description || r.exceptionDetails.text}\n${expr}`);
    return r.result.value;
  }

  /** Evaluate with the UI's shared state `S` (store.js) in scope. */
  S(expr) {
    return this.eval(`(async () => { const { S } = await import('/store.js'); return ${expr}; })()`);
  }

  async waitFor(expr, what = expr, ms = 5000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (await this.eval(expr).catch(() => false)) return;
      await sleep(50);
    }
    throw new Error(`timeout waiting for ${what}`);
  }

  /** Has the UI loaded its data and connected to the live events? */
  static READY = `document.readyState === 'complete' && (document.querySelectorAll('#device .slot').length > 0 || !!document.querySelector('#device .no-profile'))
    && document.getElementById('conn-indicator').classList.contains('ok')`;

  async open(url) {
    this.origin = new URL(url).origin;
    await this.send('Page.navigate', { url });
    await this.waitFor(Browser.READY, 'UI ready', 10000);
  }

  /** Reload (or run `fn`, which reloads) and wait until the new page is ready. */
  async reload(fn = () => this.send('Page.reload', {})) {
    await this.eval('window.__stale = true');
    await fn();
    await this.waitFor(`!window.__stale && ${Browser.READY}`, 'UI reloaded', 15000);
  }

  async key(key, { code = key, keyCode = 0, modifiers = 0 } = {}) {
    for (const type of ['keyDown', 'keyUp']) await this.keyEvent(type, key, { code, keyCode, modifiers });
  }

  keyEvent(type, key, { code = key, keyCode = 0, modifiers = 0 } = {}) {
    return this.send('Input.dispatchKeyEvent', { type, key, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode, modifiers });
  }

  async count(sel) { return this.eval(`document.querySelectorAll(${J(sel)}).length`); }
  async exists(sel) { return this.eval(`!!document.querySelector(${J(sel)})`); }
  async text(sel) { return this.eval(`document.querySelector(${J(sel)})?.textContent ?? null`); }
  async visible(sel) {
    return this.eval(`(() => { const e = document.querySelector(${J(sel)}); return !!e && !e.closest('[hidden]') && e.getClientRects().length > 0; })()`);
  }

  async click(sel, what = sel) {
    assert(await this.eval(`(() => { const e = document.querySelector(${J(sel)}); if (!e) return false; e.click(); return true; })()`), `no element ${what}`);
  }

  /** Click the first element matching `sel` whose text contains `text`. */
  async clickText(sel, text) {
    assert(await this.eval(`(() => {
      const e = [...document.querySelectorAll(${J(sel)})].find((x) => x.textContent.includes(${J(text)}));
      if (!e) return false; e.click(); return true; })()`), `no ${sel} with text "${text}"`);
  }

  /** Set the value of an input/select and fire input + change. */
  async set(sel, value) {
    assert(await this.eval(`(() => {
      const e = document.querySelector(${J(sel)}); if (!e) return false;
      if (e.type === 'checkbox') e.checked = ${J(value)}; else e.value = ${J(value)};
      e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true }));
      return true; })()`), `no element ${sel}`);
  }

  async waitDialog(title, ms = 5000) {
    await this.waitFor(`[...document.querySelectorAll('dialog[open] h2')].some((h) => h.textContent === ${J(title)})`, `dialog "${title}"`, ms);
  }

  /** Press a button (by its text) of the topmost open dialog. */
  async dialogButton(text) {
    await this.clickText('dialog[open]:last-of-type .modal-buttons button', text);
  }

  async waitNoDialog() { await this.waitFor(`!document.querySelector('dialog[open]')`, 'dialog closed'); }

  /** Click an entry of the open context menu. */
  async menu(text) {
    await this.waitFor(`!!document.querySelector('.ctxmenu')`, 'context menu');
    await this.clickText('.ctxmenu .ctx-item', text);
  }

  async profileMenu(text) {
    // Like a pointer click: the button gets the focus (it is given back after the menu/dialog).
    await this.eval(`document.getElementById('profile-menu-btn').focus()`);
    await this.click('#profile-menu-btn');
    await this.menu(text);
  }

  /** Answer the next file dialog with these files. */
  chooseFiles(...files) { this.fileQueue.push(files); }

  async toast(text, ms = 5000) {
    await this.waitFor(`[...document.querySelectorAll('#toasts .toast-msg')].some((t) => t.textContent.includes(${J(text)}))`, `toast "${text}"`, ms);
  }

  /** Recent writes, answers and toasts (for failure messages). */
  async diag() {
    const toasts = await this.eval(`[...document.querySelectorAll('#toasts .toast-msg')].map((t) => t.textContent)`).catch(() => []);
    return `(writes: ${this.requests.filter((r) => !r.startsWith('GET')).slice(-5).join(' | ')}; answers: ${this.responses.slice(-6).map((r) => `${r.status} ${r.path}`).join(' | ')}; toasts: ${J(toasts)})`;
  }

  /** Wait for a finished API response whose path matches `re` (after index `from`). */
  async response(re, from = 0, ms = 5000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const r = this.responses.slice(from).find((x) => re.test(x.path) && x.done);
      if (r) return r;
      await sleep(30);
    }
    throw new Error(`timeout waiting for a response to ${re}`);
  }

  /** Wait for a finished download; returns its path. */
  async download(before, ms = 10000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const f = fs.readdirSync(this.downloads).find((n) => !before.includes(n) && !n.endsWith('.crdownload'));
      if (f) return path.join(this.downloads, f);
      await sleep(50);
    }
    throw new Error('timeout waiting for a download');
  }

  /**
   * Intercept API requests in the browser: `respond(url)` answers them without the daemon
   * ({status, body}); `transform(json, url)` changes the daemon's answer.
   */
  async intercept(urlPattern, handler) {
    this.interceptors.push({ re: new RegExp(urlPattern.replace(/[.?]/g, '\\$&').replace(/\*/g, '.*')), urlPattern, ...handler });
    await this.send('Fetch.enable', { patterns: this.interceptors.map((x) => ({ urlPattern: x.urlPattern, requestStage: x.respond ? 'Request' : 'Response' })) });
  }

  async clearIntercepts() {
    this.interceptors = [];
    await this.send('Fetch.disable');
  }

  async onIntercept(p) {
    const ic = this.interceptors.find((x) => x.re.test(p.request.url));
    try {
      if (!ic) {
        await this.send('Fetch.continueRequest', { requestId: p.requestId });
      } else if (ic.respond) {
        const { status = 200, body } = ic.respond(p.request.url);
        await this.send('Fetch.fulfillRequest', {
          requestId: p.requestId, responseCode: status,
          responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
          body: Buffer.from(J(body)).toString('base64'),
        });
      } else {
        const r = await this.send('Fetch.getResponseBody', { requestId: p.requestId });
        const raw = r.base64Encoded ? Buffer.from(r.body, 'base64').toString() : r.body;
        const body = ic.transform(JSON.parse(raw), p.request.url);
        await this.send('Fetch.fulfillRequest', {
          requestId: p.requestId, responseCode: p.responseStatusCode || 200,
          responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
          body: Buffer.from(J(body)).toString('base64'),
        });
      }
    } catch (e) {
      this.errors.push(`intercept ${p.request.url}: ${e.message}`);
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
const KEYS = { Enter: 13, Escape: 27, Delete: 46, F10: 121, Tab: 9, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Home: 36, End: 35 };
const CTRL = 2;

/** Click a slot and wait until the editor shows it. */
async function selectSlot(b, controller, coord) {
  await b.click(slotSel(controller, coord));
  await b.waitFor(`(async () => { const { S } = await import('/store.js'); return S.selected?.controller === ${J(controller)} && S.selected?.coord === ${J(coord)}; })()`, `slot ${controller} ${coord} selected`);
}

/** Wait until the UI shows the slot as filled/empty (after a change through the API). */
async function slotShows(b, controller, coord, filled, ms = 5000) {
  await b.waitFor(`document.querySelector('${slotSel(controller, coord)}')?.classList.contains(${J(filled ? 'filled' : 'empty')})`, `slot ${coord} ${filled ? 'filled' : 'empty'}`, ms);
}

/** Put an action on the selected slot like a user: double-click it in the action list. */
async function placeOnSelected(b, uuid) {
  assert(await b.eval(`(() => { const e = document.querySelector('.action-item[data-uuid=${J(uuid)}]'); if (!e) return false;
    e.dispatchEvent(new MouseEvent('dblclick', { bubbles: true })); return true; })()`), `action ${uuid} in the list`);
}

/** Place an action on a slot (as above) and wait until the service has it and the editor shows it. */
async function placeAndEdit(b, d, controller, coord, uuid) {
  await selectSlot(b, controller, coord);
  await placeOnSelected(b, uuid);
  await until(async () => (await d.action(controller, coord))?.UUID === uuid, `${uuid} placed`);
  await b.waitFor(`(async () => { const { S, instanceAt } = await import('/store.js');
    return instanceAt(${J(controller)}, ${J(coord)})?.UUID === ${J(uuid)} && S.selected?.coord === ${J(coord)}
      && document.querySelector('#editor .ed-name')?.textContent === S.actionMap.get(${J(uuid)})?.name; })()`, `editor of ${uuid}`);
}

/** Synthetic HTML5 drag & drop between two elements. */
async function drag(b, srcSel, dstSel, { ctrl = false } = {}) {
  assert(await b.eval(`(() => {
    const dt = new DataTransfer();
    const src = document.querySelector(${J(srcSel)});
    const dst = document.querySelector(${J(dstSel)});
    if (!src || !dst) return false;
    src.dispatchEvent(new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: dt }));
    dst.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt, ctrlKey: ${ctrl} }));
    dst.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt, ctrlKey: ${ctrl} }));
    document.dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt }));
    return true;
  })()`), `drag ${srcSel} → ${dstSel}`);
}

/** Type into the title field of the editor and leave it (saves at once). */
async function typeTitle(b, text) {
  await b.eval(`(() => { const t = document.querySelector('#editor .ed-titlecol textarea'); t.focus(); t.value = ${J(text)}; t.dispatchEvent(new Event('input', { bubbles: true })); t.blur(); })()`);
}

/** Visible text and labels (title, placeholder, aria-label) of the page, without the deck previews. */
const VISIBLE_TEXTS = `(() => {
  const out = new Set();
  const shown = (e) => e && !e.closest('[hidden], [aria-hidden="true"]') && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n; (n = walk.nextNode());) {
    const s = n.textContent.trim();
    if (s && shown(n.parentElement) && !n.parentElement.closest('script, style, .act-name, .cat-name, .mk-card, .mk-detail, #device')) out.add(s);
  }
  for (const e of document.querySelectorAll('[title], [placeholder], [aria-label]')) {
    if (!shown(e) || e.closest('#device, .action-item, .mk-card')) continue;
    for (const a of ['title', 'placeholder', 'aria-label']) { const v = e.getAttribute(a)?.trim(); if (v) out.add(v); }
  }
  return [...out];
})()`;

/** Visible buttons without a text or label. */
const UNLABELLED_BUTTONS = `[...document.querySelectorAll('button, [role=button]')].filter((e) => !e.closest('[hidden]') && e.getClientRects().length > 0)
  .filter((e) => !(e.textContent.trim() || e.getAttribute('aria-label')?.trim() || e.getAttribute('title')?.trim()))
  .map((e) => e.outerHTML.slice(0, 120))`;

// -------------------------------------------------------------------------------- suites

async function suiteNeo(d, b) {
  // The page tests count from one page (the starter profile has two).
  {
    const st = await d.state();
    const prof = st.profiles.find((p) => p.id === st.devices[0].profile);
    for (const pg of prof.pages.slice(1)) await d.api(`/api/pages/${prof.id}/${pg}`, 'DELETE');
  }
  await b.open(await d.loginUrl());

  await test('loads: device grid, live connection, no errors', async () => {
    assert(await b.count(`#device .slot[data-controller="Keypad"]`) === 8, '8 keys');
    assert(await b.count(`#device .neo-row .slot`) === 3, 'touch points and info bar');
    await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'live indicator');
    eq(await b.text('#conn-indicator'), 'Live');
  });

  await test('editor: a typed title is saved', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field');
    await typeTitle(b, 'UI test');
    await until(async () => (await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'UI test', 'title saved', 2000);
  });

  await test('catalogue: Enter places the focused action on the selected key', async () => {
    await d.clear('Keypad', '1,1');
    await slotShows(b, 'Keypad', '1,1', false);
    await b.click(slotSel('Keypad', '1,1'));
    await b.eval(`document.querySelector('.action-item[data-uuid="com.elgato.streamdeck.system.website"]').focus()`);
    await b.key('Enter', { keyCode: KEYS.Enter });
    await until(async () => (await d.action('Keypad', '1,1'))?.UUID === 'com.elgato.streamdeck.system.website', 'placed', 2000);
  });

  await test('catalogue: the search filters the action list, Escape clears it', async () => {
    await b.eval(`(() => { const s = document.getElementById('action-search'); s.focus(); s.value = 'hotkey sw'; s.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await b.waitFor(`document.querySelectorAll('#catalogue .action-item').length === 1`, 'one match');
    eq(await b.eval(`document.querySelector('#catalogue .action-item').dataset.uuid`), 'com.elgato.streamdeck.system.hotkeyswitch');
    await b.set('#action-search', 'no such action at all');
    await b.waitFor(`document.querySelector('#catalogue .muted')?.textContent === 'No matching actions.'`, 'no matches');
    await b.eval(`document.getElementById('action-search').focus()`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.querySelectorAll('#catalogue .action-item').length > 20`, 'full list');
    // Collapsing a category hides its actions (and is remembered).
    await b.click('#catalogue .cat-head[data-category="Soundboard"]');
    await b.waitFor(`document.querySelector('#catalogue .cat-head[data-category="Soundboard"]').getAttribute('aria-expanded') === 'false'`, 'collapsed');
    assert(await b.eval(`JSON.parse(localStorage.getItem('sd.collapsed')).includes('Soundboard')`), 'remembered');
    await b.click('#catalogue .cat-head[data-category="Soundboard"]');
    await b.waitFor(`document.querySelector('#catalogue .cat-head[data-category="Soundboard"]').getAttribute('aria-expanded') === 'true'`, 'expanded');
  });

  await test('drag & drop: an action dropped onto a key', async () => {
    await d.clear('Keypad', '2,1');
    await slotShows(b, 'Keypad', '2,1', false);
    await drag(b, '.action-item[data-uuid="com.elgato.streamdeck.system.text"]', slotSel('Keypad', '2,1'));
    await until(async () => (await d.action('Keypad', '2,1'))?.UUID === 'com.elgato.streamdeck.system.text', 'dropped', 2000);
  });

  await test('keyboard: Shift+F10 opens the slot menu, Escape closes it and keeps the selection', async () => {
    await b.click(slotSel('Keypad', '0,0'));
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').focus()`);
    await b.key('F10', { keyCode: KEYS.F10, modifiers: 8 });
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'context menu');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.querySelector('.ctxmenu')`, 'menu closed');
    assert(await b.exists('#device .slot.selected'), 'still selected');
  });

  await test('delete with the keyboard, undo from the toast', async () => {
    const before = await d.action('Keypad', '1,1');
    assert(before, 'action to delete');
    await b.click(slotSel('Keypad', '1,1'));
    await b.eval(`document.querySelector('${slotSel('Keypad', '1,1')}').focus()`);
    await b.key('Delete', { keyCode: KEYS.Delete });
    await until(async () => !(await d.action('Keypad', '1,1')), 'deleted', 2000);
    await b.waitFor(`!!document.querySelector('.toast-action')`, 'undo button');
    await b.click('.toast-action');
    await until(async () => (await d.action('Keypad', '1,1'))?.UUID === before.UUID, `restored (last writes: ${b.requests.filter((r) => !r.startsWith('GET')).slice(-4).join(' | ')})`, 2000);
  });

  await test('pages: add a page and switch to it', async () => {
    const st = await d.state();
    const n = st.profiles.find((p) => p.id === st.devices[0].profile).pages.length;
    await b.click('.add-page');
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab').length === ${n + 1}`, 'new page tab');
    const st2 = await d.state();
    const pages = st2.profiles.find((p) => p.id === st2.devices[0].profile).pages;
    assert(pages.length === n + 1, 'page created');
    await until(async () => (await d.dev()).page === pages[n], 'device shows the new page');
    await b.eval(`document.querySelectorAll('#pagebar .page-tab')[0].click()`);
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab')[0].classList.contains('active')`, 'first page active');
    await until(async () => (await d.dev()).page === pages[0], 'device back on page 1');
  });

  await test('pages: next/previous buttons, delete a page after confirming', async () => {
    const prof = async () => { const st = await d.state(); return st.profiles.find((p) => p.id === st.devices[0].profile); };
    const pages = (await prof()).pages;
    eq(pages.length, 2, 'two pages from the previous test');
    await b.click('#pagebar .icon-btn[title="Next page"]');
    await until(async () => (await d.dev()).page === pages[1], 'next page');
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab')[1].classList.contains('active')`, 'tab 2 active');
    assert(await b.eval(`document.querySelector('#pagebar .icon-btn[title="Next page"]').disabled`), 'no next page');
    await b.click('#pagebar .icon-btn[title="Previous page"]');
    await until(async () => (await d.dev()).page === pages[0], 'previous page');
    // Context menu of a tab: "Delete page…" asks first; Cancel keeps the page.
    await b.eval(`document.querySelectorAll('#pagebar .page-tab')[1].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 600, clientY: 700 }))`);
    await b.menu('Delete page…');
    await b.waitDialog('Delete page');
    await b.dialogButton('Cancel');
    await b.waitNoDialog();
    eq((await prof()).pages.length, 2, 'kept after Cancel');
    await b.click('#pagebar .page-tab-wrap:nth-child(2) .page-del');
    await b.waitDialog('Delete page');
    await b.dialogButton('Delete');
    await until(async () => (await prof()).pages.length === 1, 'page deleted');
    await b.waitFor(`document.querySelectorAll('#pagebar .page-tab').length === 1 && !document.querySelector('#pagebar .page-del')`, 'one tab, no delete button');
  });

  await test('pages: move a page by dragging its tab, from its menu or with Alt+arrow; undo', async () => {
    const prof = async () => { const st = await d.state(); return st.profiles.find((p) => p.id === st.devices[0].profile); };
    const { id } = await prof();
    await d.api(`/api/profiles/${id}/pages`, 'POST');
    await d.api(`/api/profiles/${id}/pages`, 'POST');
    const [A, B, C] = (await prof()).pages;
    const order = async () => (await prof()).pages;
    const tabsShow = (ids) => b.waitFor(`JSON.stringify([...document.querySelectorAll('#pagebar .page-tab-wrap')].map((w) => w.dataset.page)) === ${J(JSON.stringify(ids))}`, `tabs ${ids}`);
    await tabsShow([A, B, C]);
    const shown = (await d.dev()).page;
    // Menu: "Move right" on page 1, then undo from the toast.
    await b.eval(`document.querySelectorAll('#pagebar .page-tab')[0].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 600, clientY: 700 }))`);
    eq(await b.eval(`[...document.querySelectorAll('.ctxmenu .ctx-item')].map((i) => [i.querySelector('span').textContent, i.disabled])`),
      [['Open', false], ['Move left', true], ['Move right', false], ['Delete page…', false]]);
    await b.menu('Move right');
    await until(async () => J(await order()) === J([B, A, C]), 'moved right');
    await tabsShow([B, A, C]);
    eq((await d.dev()).page, shown, 'the deck stays on its page');
    await b.waitFor(`!!document.querySelector('.toast-action')`, 'undo offered');
    await b.click('.toast-action');
    await until(async () => J(await order()) === J([A, B, C]), 'undone');
    await tabsShow([A, B, C]);
    // Keyboard: Alt+← on page 3 makes it page 2; the tab keeps the focus.
    await b.eval(`document.querySelectorAll('#pagebar .page-tab')[2].focus()`);
    await b.key('ArrowLeft', { keyCode: KEYS.ArrowLeft, modifiers: 1 });
    await until(async () => J(await order()) === J([A, C, B]), 'Alt+ArrowLeft');
    await tabsShow([A, C, B]);
    await b.waitFor(`document.activeElement?.closest('.page-tab-wrap')?.dataset.page === ${J(C)}`, 'focus on the moved tab');
    // Dragging: page 3 (B) onto the left half of page 1 → first.
    await b.eval(`(() => {
      const wraps = [...document.querySelectorAll('#pagebar .page-tab-wrap')];
      const dt = new DataTransfer();
      wraps[2].querySelector('.page-tab').dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
      const r = wraps[0].getBoundingClientRect();
      const at = { bubbles: true, cancelable: true, dataTransfer: dt, clientX: r.left + 2, clientY: r.top + r.height / 2 };
      wraps[0].dispatchEvent(new DragEvent('dragover', at));
      window.__marked = wraps[0].classList.contains('drop-before');
      wraps[0].dispatchEvent(new DragEvent('drop', at));
      wraps[2].querySelector('.page-tab').dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt }));
    })()`);
    assert(await b.eval('window.__marked'), 'drop position marked');
    await until(async () => J(await order()) === J([B, A, C]), 'dragged to the front');
    await tabsShow([B, A, C]);
    eq(await b.count('#pagebar .drop-before, #pagebar .drop-after, #pagebar .dragging'), 0, 'marks removed');
    // Back to a single page for the following tests.
    for (const p of (await order()).filter((x) => x !== shown)) await d.api(`/api/pages/${id}/${p}`, 'DELETE');
    await tabsShow([shown]);
  });

  await test('topbar: the device list and status show the connected deck', async () => {
    eq(await b.eval(`[...document.querySelectorAll('#device-select option')].map((o) => o.textContent)`), ['Stream Deck Neo']);
    assert(await b.eval(`document.getElementById('device-status').classList.contains('on')`), 'status dot on');
    assert((await b.eval(`document.getElementById('device-status').title`)).includes('Firmware virtual'), 'firmware in the status');
    assert(!(await b.eval(`document.getElementById('brightness').disabled`)), 'brightness enabled');
  });

  await test('topbar: switching to a disconnected device shows its (missing) profiles offline', async () => {
    // A second, disconnected deck (as known from an earlier session) – the virtual setup has one.
    await b.intercept('*/api/state', {
      transform: (st) => ({
        ...st, devices: [...st.devices, {
          id: 'FAKE-XL', name: 'Stream Deck XL', model: '20GAT9901', kind: 'xl', connected: false, cols: 8, rows: 4, key_px: 96,
          touch_points: 0, screen: null, encoders: 0, firmware: '', profile: null, page: null, folder_depth: 0, brightness: 50,
        }],
      }),
    });
    try {
      await b.reload();
      await b.waitFor(`document.querySelectorAll('#device-select option').length === 2`, 'two devices');
      eq(await b.eval(`document.querySelectorAll('#device-select option')[1].textContent`), 'Stream Deck XL – not connected');
      await b.set('#device-select', 'FAKE-XL');
      await b.waitFor(`!!document.querySelector('#device .no-profile')`, 'no profile for the XL');
      assert((await b.text('#device .no-profile')).includes('No profile for this device.'), 'explains');
      assert(await b.exists('#device .no-profile button.primary'), 'offers to create one');
      assert(await b.eval(`document.getElementById('device-status').classList.contains('off')`), 'status off');
      assert(await b.eval(`document.getElementById('brightness').disabled`), 'no brightness offline');
      await b.set('#device-select', 'VIRTUAL-NEO');
      await b.waitFor(`document.querySelectorAll('#device .slot[data-controller="Keypad"]').length === 8`, 'Neo again');
    } finally {
      await b.clearIntercepts();
    }
    await b.reload();
  });

  await test('topbar: the brightness slider sets the device brightness', async () => {
    await b.set('#brightness', '35');
    await until(async () => (await d.dev()).brightness === 35, 'brightness 35');
    eq(await b.text('#brightness-val'), '35 %');
  });

  await test('canvas: arrow keys, Home and End move between slots; one tab stop', async () => {
    await b.click(slotSel('Keypad', '0,0'));
    const focused = `(() => { const s = document.activeElement.closest('.slot'); return s ? s.dataset.controller + ' ' + s.dataset.coord : null; })()`;
    const press = async (k, expect) => {
      await b.key(k, { keyCode: KEYS[k] });
      eq(await b.eval(focused), expect, `after ${k}`);
    };
    await press('ArrowRight', 'Keypad 1,0');
    await press('ArrowDown', 'Keypad 1,1');
    await press('End', 'Keypad 3,1');
    await press('Home', 'Keypad 0,1');
    await press('ArrowDown', 'Neo 0,0');
    await press('ArrowRight', 'Neo 1,0');
    await press('ArrowRight', 'Neo 2,0');
    await press('ArrowRight', 'Neo 2,0');
    await press('ArrowUp', 'Keypad 3,1');
    eq(await b.count('#device .slot[tabindex="0"]'), 1, 'one tab stop');
    // Enter selects the focused slot.
    await b.key('Enter', { keyCode: KEYS.Enter });
    await b.waitFor(`(async () => { const { S } = await import('/store.js'); return S.selected?.coord === '3,1'; })()`, 'selected');
    assert((await b.text('#editor .ed-sub')).includes('Key 2/4'), 'editor shows key 2/4');
  });

  await test('clipboard: Ctrl+C on a key, Ctrl+V on another copies the action', async () => {
    const src = await d.action('Keypad', '0,0');
    await b.click(slotSel('Keypad', '0,0'));
    await b.key('c', { code: 'KeyC', keyCode: 67, modifiers: CTRL });
    await b.toast('Action copied');
    await b.click(slotSel('Keypad', '3,0'));
    await b.key('v', { code: 'KeyV', keyCode: 86, modifiers: CTRL });
    await until(async () => (await d.action('Keypad', '3,0'))?.States?.[0]?.Title === 'UI test', 'pasted');
    const copy = await d.action('Keypad', '3,0');
    eq(copy.UUID, src.UUID);
    assert(copy.ActionID !== src.ActionID, 'a new instance');
  });

  await test('drag & drop: a key is moved, Ctrl copies it, the trash deletes it', async () => {
    await d.clear('Keypad', '2,0');
    await slotShows(b, 'Keypad', '2,0', false);
    await drag(b, slotSel('Keypad', '3,0'), slotSel('Keypad', '2,0'));
    await until(async () => (await d.action('Keypad', '2,0'))?.States?.[0]?.Title === 'UI test' && !(await d.action('Keypad', '3,0')), 'moved');
    await slotShows(b, 'Keypad', '3,0', false);
    await drag(b, slotSel('Keypad', '2,0'), slotSel('Keypad', '3,0'), { ctrl: true });
    await until(async () => (await d.action('Keypad', '3,0'))?.States?.[0]?.Title === 'UI test' && !!(await d.action('Keypad', '2,0')), 'copied');
    await slotShows(b, 'Keypad', '3,0', true);
    await drag(b, slotSel('Keypad', '3,0'), '#trash');
    await until(async () => !(await d.action('Keypad', '3,0')), 'deleted in the trash');
    await b.toast('Action deleted');
    assert(!(await b.exists('body.dragging')), 'drag state cleared');
  });

  await test('context menu: entries of a key; "Test press" runs the action', async () => {
    const dev = await d.dev();
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'PATCH', { controller: 'Keypad', coord: '1,1', settings_merge: { path: 'https://example.org' } });
    await b.eval(`(() => { const s = document.querySelector('${slotSel('Keypad', '1,1')}'); const r = s.getBoundingClientRect();
      s.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.left + 10, clientY: r.top + 10 })); })()`);
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'menu');
    const items = await b.eval(`[...document.querySelectorAll('.ctxmenu .ctx-item')].map((i) => [i.querySelector('span').textContent, i.disabled])`);
    eq(items, [['Copy', false], ['Paste', false], ['Delete', false], ['Test press', false]]);
    const m = d.mark();
    await b.menu('Test press');
    await until(() => d.logsSince(m, /DRYRUN.*example\.org/).length, 'the website action ran (dry run)');
    // An empty slot offers nothing to copy, delete or press.
    await slotShows(b, 'Keypad', '3,0', false);
    await b.eval(`(() => { const s = document.querySelector('${slotSel('Keypad', '3,0')}'); s.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 50, clientY: 50 })); })()`);
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'menu');
    eq(await b.eval(`[...document.querySelectorAll('.ctxmenu .ctx-item')].filter((i) => !i.disabled).map((i) => i.querySelector('span').textContent)`), ['Paste']);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.querySelector('.ctxmenu')`, 'menu closed');
    // The editor's "Test press" button does the same.
    await selectSlot(b, 'Keypad', '1,1');
    const m2 = d.mark();
    await b.click('#editor .ed-press');
    await until(() => d.logsSince(m2, /DRYRUN.*example\.org/).length, 'pressed from the editor');
  });

  await test('folders: open with double-click or Enter, breadcrumb, Back and the parent-folder key', async () => {
    await placeAndEdit(b, d, 'Keypad', '3,1', 'com.elgato.streamdeck.profile.openchild');
    const folder = (await d.action('Keypad', '3,1')).Settings.ProfileUUID.toLowerCase();
    const root = (await d.dev()).page;
    // The canvas shows that page (with its data loaded).
    const shows = (pg) => b.waitFor(`(async () => { const { S } = await import('/store.js'); return S.view.page === ${J(pg)} && S.page?.id === S.view.page; })()`, `page ${pg} shown`);
    await b.waitFor(`document.querySelector('${slotSel('Keypad', '3,1')}')?.classList.contains('folder')`, 'folder key');
    assert(await b.exists(`${slotSel('Keypad', '3,1')} .folder-badge`), 'folder badge');
    // Give it a name for the breadcrumb.
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'editor');
    await typeTitle(b, 'Apps');
    await until(async () => (await d.action('Keypad', '3,1'))?.States?.[0]?.Title === 'Apps', 'folder titled');
    await b.eval(`document.querySelector('${slotSel('Keypad', '3,1')}').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }))`);
    await until(async () => (await d.dev()).page === folder, 'device in the folder');
    await shows(folder);
    await b.waitFor(`!!document.querySelector('#crumbs .back-btn')`, 'Back button');
    await b.waitFor(`document.querySelector('#crumbs .crumb.current')?.textContent === '\\uebe9 Apps'`, 'folder name in the breadcrumb');
    eq(await b.eval(`[...document.querySelectorAll('#crumbs .crumb')].map((c) => c.textContent)`), ['Default Profile', 'Page 1', '\uebe9 Apps'], 'folder icon (Elgato icon font) and name');
    assert(await b.exists('#pagebar .page-tab.active.inactive-folder'), 'page tab marked as "in folder"');
    eq((await d.action('Keypad', '0,0'))?.UUID, 'com.elgato.streamdeck.profile.backtoparent', 'parent-folder key at 0,0');
    await b.click('#crumbs .back-btn');
    await until(async () => (await d.dev()).page === root, 'back on the page');
    await shows(root);
    await b.waitFor(`!document.querySelector('#crumbs .back-btn')`, 'no Back button');
    // Keyboard: Enter on the folder key opens it; the parent-folder key leads back.
    await slotShows(b, 'Keypad', '3,1', true);
    await b.click(slotSel('Keypad', '3,1'));
    await b.key('Enter', { keyCode: KEYS.Enter });
    await until(async () => (await d.dev()).page === folder, 'opened with Enter');
    await shows(folder);
    await b.eval(`document.querySelector('${slotSel('Keypad', '0,0')}').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }))`);
    await until(async () => (await d.dev()).page === root, 'parent-folder key');
    await shows(root);
    // Deleting a folder asks first and cannot be undone.
    await slotShows(b, 'Keypad', '3,1', true);
    await b.click(slotSel('Keypad', '3,1'));
    await b.key('Delete', { keyCode: KEYS.Delete });
    await b.waitDialog('Delete folder');
    await b.dialogButton('Delete');
    await until(async () => !(await d.action('Keypad', '3,1')), 'folder deleted');
    const st = await d.state();
    eq(st.profiles.find((p) => p.id === st.devices[0].profile).folders, [], 'folder page gone');
  });

  await test('editor: title style, colour and background are saved', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    const st0 = async () => (await d.action('Keypad', '0,0')).States[0];
    await b.set('#editor select[title="Alignment"]', 'top');
    await b.set('#editor select[title="Font size"]', '18');
    await b.set('#editor select[title="Font style"]', 'Bold');
    await b.set('#editor .toggle-btn input', true);
    await b.set('#editor input[title="Title color"]', '#ff0000');
    await b.click('#editor .bg-swatch[title="#2563eb"]');
    await until(async () => { const s = await st0(); return s.TitleAlignment === 'top' && s.FontSize === 18 && s.FontStyle === 'Bold' && s.FontUnderline === true && s.TitleColor === '#ff0000' && s.Background === '#2563eb'; },
      'style saved');
    await b.set('#editor .title-tools label.check input', false);
    await until(async () => (await st0()).ShowTitle === false, 'title hidden');
    await b.click('#editor .bg-none');
    await until(async () => !(await st0()).Background, 'background removed');
    await b.set('#editor .title-tools label.check input', true);
    await until(async () => (await st0()).ShowTitle === true, 'title shown');
    // The preview is re-rendered with the new look.
    assert((await b.eval(`document.querySelector('#editor .ed-preview img').src`)).includes('/api/render/'), 'preview');
  });

  await test('editor: the icon can be drawn black, white or in a colour of its own', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    const st0 = async () => (await d.action('Keypad', '0,0')).States[0];
    const dev = await d.dev();
    const render = () => d.api(`/api/render/${dev.profile}/${dev.page}/Keypad/0,0.png?size=72&v=${Date.now()}`);
    await b.waitFor(`!!document.querySelector('#editor .ed-iconcolor')`, 'icon colour row');
    const before = await render();
    await b.click('#editor .ed-iconcolor .bg-swatch[data-color="#000000"]');
    await until(async () => (await st0()).ImageColor === '#000000', 'black saved');
    await until(async () => !(await render()).equals(before), 'drawn differently');
    await b.waitFor(`document.querySelector('#editor .ed-iconcolor .bg-swatch[data-color="#000000"]')?.classList.contains('active')`, 'black marked');
    await b.set('#editor .ed-iconcolor input[type=color]', '#ff0000');
    await until(async () => (await st0()).ImageColor === '#ff0000', 'own colour saved');
    await b.click('#editor .ed-iconcolor .ic-none');
    await until(async () => !(await st0()).ImageColor, 'back to the icon as it comes');
    await until(async () => (await render()).equals(before), 'drawn as before');
  });

  await test('editor: a gradient background from cssgradient.io is saved and drawn', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    const st0 = async () => (await d.action('Keypad', '0,0')).States[0];
    const before = await d.api(`/api/render/${(await d.state()).devices[0].profile}/${(await d.state()).devices[0].page}/Keypad/0,0.png?size=72`);
    await b.click('#editor .bg-gradient');
    await b.waitDialog('Background gradient');
    // Pasted as cssgradient.io copies it.
    await b.set('.grad-css', 'background: linear-gradient(90deg, rgba(255,0,0,1) 0%, rgba(0,0,255,1) 100%);');
    await b.waitFor(`document.querySelector('.grad-preview').style.background.includes('linear-gradient')`, 'preview');
    await b.dialogButton('Apply');
    await until(async () => (await st0()).Background === 'linear-gradient(90deg, rgba(255,0,0,1) 0%, rgba(0,0,255,1) 100%)', 'gradient saved');
    await until(async () => !(await d.api(`/api/render/${(await d.state()).devices[0].profile}/${(await d.state()).devices[0].page}/Keypad/0,0.png?size=72&v=${Date.now()}`)).equals(before), 'drawn on the key');
    // A background from an imported profile can't make the UI load a URL.
    const dv = await d.dev();
    await d.api(`/api/pages/${dv.profile}/${dv.page}/slot`, 'PATCH', { controller: 'Keypad', coord: '0,0', state: 0, style: { Background: 'linear-gradient(#000, #000), url(http://127.0.0.1:9/beacon)' } });
    await until(async () => /beacon/.test((await st0()).Background || ''), 'stored as it came');
    await selectSlot(b, 'Keypad', '1,0');
    await selectSlot(b, 'Keypad', '0,0');
    eq(await b.eval(`[...document.querySelectorAll('#editor [style]')].map((e) => e.getAttribute('style')).filter((x) => /url\\(/.test(x))`), [], 'no url() in the editor');
    // A ready-made one, then none again.
    await b.click('#editor .bg-swatch[title="Gradient"]');
    await until(async () => /^linear-gradient\(135deg/.test((await st0()).Background || ''), 'preset');
    await b.click('#editor .bg-none');
    await until(async () => !(await st0()).Background, 'background removed');
  });

  await test('colour pickers stay open while a colour is picked (their field is not replaced)', async () => {
    // An open picker sends "input" events; it closes when its <input> leaves the page.
    const picking = async (sel, what, saved) => {
      await b.waitFor(`!!document.querySelector(${J(sel)})`, what);
      await b.eval(`(() => { const e = document.querySelector(${J(sel)}); window.__picker = e;
        for (const c of ['#123456', '#345678', '#56789a']) { e.value = c; e.dispatchEvent(new Event('input', { bubbles: true })); } })()`);
      if (saved) await until(saved, `${what}: saved`);
      await sleep(700); // page events and reloads after the save
      assert(await b.eval('window.__picker.isConnected'), `${what}: the field was replaced (the picker would close)`);
      await b.eval(`window.__picker.dispatchEvent(new Event('change', { bubbles: true }))`);
    };
    await selectSlot(b, 'Keypad', '0,0');
    const st0 = async () => (await d.action('Keypad', '0,0')).States[0];
    await picking('#editor .ed-titlecol input.color, #editor input.color[title="Title color"]', 'title colour', async () => (await st0()).TitleColor === '#56789a');
    await picking('#editor .bg-custom', 'key background', async () => (await st0()).Background === '#56789a');
    // The gradient dialog: colour and position of a colour stop.
    await b.click('#editor .bg-gradient');
    await b.waitDialog('Background gradient');
    await picking('.grad-stop input.color', 'gradient colour');
    await b.eval(`(() => { const r = document.querySelector('.grad-stop input[type=range]'); window.__picker = r; r.value = 40; r.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    assert(await b.eval('window.__picker.isConnected'), 'gradient position slider kept while dragging');
    eq(await b.text('.grad-stop .tb-value'), '40%');
    assert((await b.eval(`document.querySelector('.grad-css').value`)).includes('#56789a 40%'), 'CSS follows');
    await b.dialogButton('Cancel');
    await b.waitNoDialog();
    // Settings forms: the info bar text action's colours.
    const dev = await d.dev();
    const clock = await d.action('Neo', '1,0');
    await d.clear('Neo', '1,0');
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'POST', { controller: 'Neo', coord: '1,0', uuid: 'dev.tuxdeck.infobar.text' });
    await selectSlot(b, 'Neo', '1,0');
    await picking('#editor .ed-settings input.color:not(.bg-custom)', 'text colour', async () => (await d.action('Neo', '1,0')).Settings.color === '#56789a');
    await picking('#editor .ed-settings .bg-custom', 'info bar background', async () => (await d.action('Neo', '1,0')).Settings.background === '#56789a');
    // A change still waiting to be saved is saved when another key is selected right away.
    await b.set('#editor .ed-settings textarea', 'typed and gone');
    await selectSlot(b, 'Keypad', '0,0');
    await until(async () => (await d.action('Neo', '1,0')).Settings.text === 'typed and gone', 'pending text saved on leaving');
    await d.clear('Neo', '1,0');
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'POST', { controller: 'Neo', coord: '1,0', uuid: clock.UUID });
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'PATCH', { controller: 'Neo', coord: '1,0', settings: clock.Settings });
  });

  await test('canvas: a running timer redraws only its own key; nothing flickers', async () => {
    const dev = await d.dev();
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'POST', { controller: 'Keypad', coord: '2,0', uuid: 'com.elgato.streamdeck.system.timer' });
    await slotShows(b, 'Keypad', '2,0', true);
    await selectSlot(b, 'Keypad', '0,0');
    await sleep(500); // the page reload after placing the timer
    // Watch for <img> elements that are removed or emptied (what the eye sees as flicker).
    await b.eval(`(() => { window.__flicker = []; new MutationObserver((ms) => { for (const m of ms) {
      for (const n of m.removedNodes) if (n.nodeName === 'IMG' || n.querySelector?.('img')) window.__flicker.push('removed ' + (n.dataset?.key || n.className));
      if (m.type === 'attributes' && m.target.nodeName === 'IMG' && !m.target.getAttribute('src')) window.__flicker.push('emptied ' + m.target.dataset.key);
    } }).observe(document.querySelector('#device'), { childList: true, subtree: true, attributes: true, attributeFilter: ['src'] }); })()`);
    await d.api(`/api/pages/${dev.profile}/${dev.page}/press`, 'POST', { controller: 'Keypad', coord: '2,0' });
    const from = b.requests.length;
    await sleep(3500);
    const renders = b.requests.slice(from).filter((r) => r.includes('/api/render/'));
    const mine = (r, c) => r.includes(`/${c.replace(',', '%2C')}.png`) || r.includes(`/${c}.png`);
    const timer = renders.filter((r) => r.includes('/Keypad/') && mine(r, '2,0'));
    assert(timer.length >= 2, `the timer key is redrawn every second (${J(renders)})`);
    eq(renders.filter((r) => !(r.includes('/Keypad/') && mine(r, '2,0')) && !(r.includes('/Neo/') && mine(r, '1,0'))), [], 'no other key is reloaded');
    eq(await b.eval('window.__flicker'), [], 'no picture was removed or emptied');
    // Stop the timer again and clear the key.
    await d.api(`/api/pages/${dev.profile}/${dev.page}/press`, 'POST', { controller: 'Keypad', coord: '2,0' });
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot?controller=Keypad&coord=2,0`, 'DELETE');
  });

  await test('editor: an uploaded image is used, "Default" restores the action image', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    const before = (await d.action('Keypad', '0,0')).States[0].Image;
    b.chooseFiles(tmpFile('red.svg', SVG('#ff0000')));
    await b.clickText('#editor .ed-imgbtns button', 'Image…');
    await until(async () => { const img = (await d.action('Keypad', '0,0')).States[0].Image; return img && img !== before; }, 'image saved');
    const img = (await d.action('Keypad', '0,0')).States[0].Image;
    eq(fs.readFileSync(d.file(path.basename(img)), 'utf8'), SVG('#ff0000'), 'the uploaded SVG is stored as it is');
    // A PNG bigger than 288 px is scaled down in the browser before it is sent.
    const big = await b.eval(`(async () => { const c = document.createElement('canvas'); c.width = 600; c.height = 300; const x = c.getContext('2d'); x.fillStyle = '#0f0'; x.fillRect(0, 0, 600, 300);
      return c.toDataURL('image/png').split(',')[1]; })()`);
    b.chooseFiles(tmpFile('big.png', Buffer.from(big, 'base64')));
    await b.clickText('#editor .ed-imgbtns button', 'Image…');
    const png = await until(async () => { const x = (await d.action('Keypad', '0,0')).States[0].Image; return x !== img && x; }, 'PNG saved');
    const stored = fs.readFileSync(d.file(path.basename(png)));
    eq([stored.readUInt32BE(16), stored.readUInt32BE(20)], [288, 144], 'scaled to 288 px');
    await b.clickText('#editor .ed-imgbtns button', 'Default');
    await until(async () => !(await d.action('Keypad', '0,0')).States[0].Image, 'default image');
  });

  await test('editor: the states of a two-state action have their own tab and title', async () => {
    await placeAndEdit(b, d, 'Keypad', '3,0', 'com.elgato.streamdeck.system.hotkeyswitch');
    await b.waitFor(`document.querySelectorAll('#editor .state-tab').length === 2`, 'two state tabs');
    // Both hotkey rows are labelled by state.
    eq(await b.eval(`[...document.querySelectorAll('#editor .hk-num')].map((x) => x.textContent)`), ['State 1', 'State 2']);
    await b.clickText('#editor .state-tab', 'State 2');
    await b.waitFor(`document.querySelectorAll('#editor .state-tab')[1].getAttribute('aria-selected') === 'true'`, 'state 2 active');
    await typeTitle(b, 'Second');
    await until(async () => (await d.action('Keypad', '3,0'))?.States?.[1]?.Title === 'Second', 'title of state 2');
    assert((await d.action('Keypad', '3,0')).States[0].Title !== 'Second', 'state 1 unchanged');
  });

  await test('editor: the hotkey recorder records Ctrl+Z and removes it again', async () => {
    await placeAndEdit(b, d, 'Keypad', '3,0', 'com.elgato.streamdeck.system.hotkey');
    await b.waitFor(`!!document.querySelector('#editor .hotkey button.btn')`, 'record button');
    await b.clickText('#editor .hotkey button.btn', 'Record');
    await b.waitFor(`document.querySelector('#editor .hk-display').textContent === 'Press a key combination…'`, 'recording');
    await b.keyEvent('keyDown', 'Control', { code: 'ControlLeft', keyCode: 17, modifiers: CTRL });
    await b.keyEvent('keyDown', 'z', { code: 'KeyZ', keyCode: 90, modifiers: CTRL });
    await b.keyEvent('keyUp', 'z', { code: 'KeyZ', keyCode: 90, modifiers: CTRL });
    await b.keyEvent('keyUp', 'Control', { code: 'ControlLeft', keyCode: 17 });
    await b.waitFor(`/Ctrl/.test(document.querySelector('#editor .hk-display').textContent)`, 'shows Ctrl+Z');
    const hk = await until(async () => (await d.action('Keypad', '3,0'))?.Settings?.Hotkeys?.[0]?.LinuxCode === 44 && (await d.action('Keypad', '3,0')).Settings.Hotkeys[0], 'hotkey saved');
    eq([hk.KeyCtrl, hk.KeyShift, hk.KeyModifiers, hk.QTKeyCode], [true, false, 2, 90]);
    // A second row is offered for another combination.
    eq(await b.count('#editor .hotkey'), 2, 'second row');
    await b.click('#editor .hotkey .icon-btn.danger:not([disabled])');
    await until(async () => (await d.action('Keypad', '3,0'))?.Settings?.Hotkeys?.[0]?.QTKeyCode === 33554431, 'hotkey removed');
  });

  await test('editor: Super combinations the desktop keeps: switch on Super, record just the key', async () => {
    await selectSlot(b, 'Keypad', '3,0');
    await b.waitFor(`!!document.querySelector('#editor .hotkey .hk-mods')`, 'modifier switches');
    const hk0 = async () => (await d.action('Keypad', '3,0'))?.Settings?.Hotkeys?.[0];
    // What arrives when the desktop takes Super+E: Super alone.
    await b.clickText('#editor .hotkey button.btn', 'Record');
    await b.keyEvent('keyDown', 'Meta', { code: 'MetaLeft', keyCode: 91, modifiers: 4 });
    await b.keyEvent('keyUp', 'Meta', { code: 'MetaLeft', keyCode: 91 });
    await until(async () => (await hk0())?.LinuxCode === 125, 'only Super saved');
    await b.waitFor(`!!document.querySelector('#editor .hk-hint')`, 'hint shown');
    // Switch on Super, then record E.
    await b.click(`#editor .hotkey .hk-mod[data-mod="meta"]`);
    await b.waitFor(`document.querySelector('#editor .hotkey .hk-mod[data-mod="meta"]').getAttribute('aria-pressed') === 'true'`, 'Super on');
    await b.clickText('#editor .hotkey button.btn', 'Record');
    await b.keyEvent('keyDown', 'e', { code: 'KeyE', keyCode: 69 });
    await b.keyEvent('keyUp', 'e', { code: 'KeyE', keyCode: 69 });
    await until(async () => (await hk0())?.LinuxCode === 18, 'Super+E saved');
    const hk = await hk0();
    eq([hk.KeyCmd, hk.KeyCtrl, hk.KeyShift, hk.KeyOption, hk.KeyModifiers], [true, false, false, false, 8]);
    assert(/^(Super|⌘) \+ E$/.test(await b.text('#editor .hk-display')), 'shows Super + E (⌘ on the mac platform)');
    assert(!(await b.exists('#editor .hk-hint')), 'hint gone');
    // The switches change a recorded combination: Ctrl on and off again.
    await b.click(`#editor .hotkey .hk-mod[data-mod="ctrl"]`);
    await until(async () => (await hk0())?.KeyModifiers === 10, 'Ctrl added');
    assert(/^Ctrl \+ (Super|⌘) \+ E$/.test(await b.text('#editor .hk-display')), 'shows Ctrl + Super + E');
    await b.click(`#editor .hotkey .hk-mod[data-mod="ctrl"]`);
    await until(async () => (await hk0())?.KeyModifiers === 8 && !(await hk0()).KeyCtrl, 'Ctrl removed');
    await b.click('#editor .hotkey .icon-btn.danger:not([disabled])');
    await until(async () => (await hk0())?.QTKeyCode === 33554431, 'removed');
    eq(await b.eval(`[...document.querySelectorAll('#editor .hotkey:first-child .hk-mod')].map((m) => m.getAttribute('aria-pressed'))`), ['false', 'false', 'false', 'false'], 'switches off after removing');
  });

  await test('editor: on a QWERTZ layout Ctrl+Z is recorded and shown as Z (the key where US has Y)', async () => {
    // The layout the browser reports for the keys (Chromium: navigator.keyboard).
    await b.eval(`(() => { window.__layout = navigator.keyboard?.getLayoutMap; const kb = navigator.keyboard || {};
      Object.defineProperty(navigator, 'keyboard', { configurable: true, value: { ...kb, getLayoutMap: async () => new Map([['KeyY', 'z'], ['KeyZ', 'y'], ['KeyQ', 'q']]) } }); })()`);
    await placeAndEdit(b, d, 'Keypad', '1,1', 'com.elgato.streamdeck.system.hotkey');
    await b.waitFor(`!!document.querySelector('#editor .hotkey button.btn')`, 'record button');
    await b.clickText('#editor .hotkey button.btn', 'Record');
    await b.waitFor(`document.querySelector('#editor .hk-display').textContent === 'Press a key combination…'`, 'recording');
    await b.keyEvent('keyDown', 'Control', { code: 'ControlLeft', keyCode: 17, modifiers: CTRL });
    await b.keyEvent('keyDown', 'z', { code: 'KeyY', keyCode: 90, modifiers: CTRL });
    await b.keyEvent('keyUp', 'z', { code: 'KeyY', keyCode: 90, modifiers: CTRL });
    await b.keyEvent('keyUp', 'Control', { code: 'ControlLeft', keyCode: 17 });
    await b.waitFor(`document.querySelector('#editor .hk-display').textContent === 'Ctrl + Z'`, 'shows Ctrl + Z');
    const hk = await until(async () => (await d.action('Keypad', '1,1'))?.Settings?.Hotkeys?.[0]?.LinuxCode === 21 && (await d.action('Keypad', '1,1')).Settings.Hotkeys[0], 'hotkey saved');
    eq([hk.LinuxCode, hk.QTKeyCode], [21, 90], 'the physical key and the character Z');
    await b.click('#editor .hotkey .icon-btn.danger:not([disabled])');
    await until(async () => (await d.action('Keypad', '1,1'))?.Settings?.Hotkeys?.[0]?.QTKeyCode === 33554431, 'hotkey removed');
    await b.eval(`delete navigator.keyboard`);
  });

  await test('editor: multi action – add, reorder, edit and remove nested actions', async () => {
    await placeAndEdit(b, d, 'Keypad', '3,0', 'com.elgato.streamdeck.multiactions.routine');
    const key = await b.S(`S.actionMap.get('com.elgato.streamdeck.multiactions.routine').form.find((f) => f.type === 'actions').key`);
    const nested = async () => ((await d.action('Keypad', '3,0'))?.Settings?.[key] || []).map((a) => a.UUID.split('.').pop());
    await b.waitFor(`!!document.querySelector('#editor .multi > select')`, 'add select');
    await b.set('#editor .multi > select', 'com.elgato.streamdeck.system.website');
    await b.waitFor(`document.querySelectorAll('#editor .multi-item').length === 1`, 'one entry');
    // A new entry opens its settings: fill in the URL.
    await b.waitFor(`!!document.querySelector('#editor .nested-form input')`, 'nested settings');
    await b.set('#editor .nested-form input[type="text"]', 'https://example.org');
    await b.set('#editor .multi > select', 'com.elgato.streamdeck.multiactions.delay');
    await b.waitFor(`document.querySelectorAll('#editor .multi-item').length === 2`, 'two entries');
    await until(async () => J(await nested()) === J(['website', 'delay']), 'saved in order');
    eq((await d.action('Keypad', '3,0')).Settings[key][0].Settings.path, 'https://example.org', 'nested setting saved');
    await b.click('#editor .multi-item:nth-child(2) .icon-btn[title="Move up"]');
    await until(async () => J(await nested()) === J(['delay', 'website']), 'reordered');
    await b.click('#editor .multi-item:nth-child(1) .icon-btn[title="Remove"]');
    await until(async () => J(await nested()) === J(['website']), 'removed');
    assert((await b.text('#editor .multi-item .multi-summary')).includes('example.org'), 'summary of the collapsed entry');
  });

  await test('Neo: info bar and touch points have their own editor; drop targets are marked', async () => {
    await selectSlot(b, 'Neo', '1,0');
    await b.waitFor(`!!document.querySelector('#editor .ed-note')`, 'note');
    eq(await b.text('#editor .ed-note'), 'The info bar draws this action itself; choose its look in the settings.');
    assert(!(await b.exists('#editor .ed-titlecol')), 'no title for the built-in info bar');
    assert(await b.exists('#editor .ed-preview.static.bar'), 'static bar preview');
    // Layout → theme options depend on each other.
    const layouts = await b.eval(`[...document.querySelectorAll('#editor .ed-settings select')[0].options].filter((o) => !o.disabled).map((o) => o.value)`);
    assert(layouts.length > 1, 'several layouts');
    const before = (await d.action('Neo', '1,0')).Settings.layout;
    await b.set('#editor .ed-settings select', layouts.find((v, i) => i > 0 && v !== '-1') || layouts[1]);
    await until(async () => (await d.action('Neo', '1,0')).Settings.layout !== before, 'layout saved');
    // Touch point: an ordinary key action, but no image or title.
    await d.clear('Neo', '0,0');
    await slotShows(b, 'Neo', '0,0', false);
    await placeAndEdit(b, d, 'Neo', '0,0', 'com.elgato.streamdeck.system.website');
    await b.waitFor(`document.querySelector('#editor .ed-note')?.textContent === 'Touch points have no display: they light up and trigger the action.'`, 'touch note');
    assert(!(await b.exists('#editor .ed-imgbtns')), 'no image buttons');
    // Dragging an info bar action marks where it may go.
    await b.eval(`document.querySelector('.action-item[data-uuid="com.elgato.streamdeck.system.pagination"]').dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: new DataTransfer() }))`);
    assert(await b.eval(`document.querySelector('${slotSel('Neo', '1,0')}').classList.contains('drop-candidate')`), 'info bar accepts it');
    eq(await b.eval(`[...document.querySelectorAll('#device .slot.drop-denied')].length`), 10, 'keys and touch points refuse it');
    await b.eval(`document.dispatchEvent(new DragEvent('dragend', { bubbles: true }))`);
    eq(await b.count('#device .slot.drop-denied, #device .slot.drop-candidate'), 0, 'marks removed');
  });

  await test('Neo: the info bar shows a free text with placeholders, colours and a gradient', async () => {
    const dev = await d.dev();
    const clock = await d.action('Neo', '1,0');
    await d.clear('Neo', '1,0');
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'POST', { controller: 'Neo', coord: '1,0', uuid: 'dev.tuxdeck.infobar.text' });
    await selectSlot(b, 'Neo', '1,0');
    await b.waitFor(`!!document.querySelector('#editor .ed-settings textarea')`, 'text field');
    assert(!(await b.exists('#editor .ed-titlecol')), 'the text is set in the settings, not as a title');
    assert(await b.exists('#editor .ed-settings .bg-swatches'), 'background choices');
    await b.set('#editor .ed-settings textarea', 'Hello {pagename}');
    await until(async () => (await d.action('Neo', '1,0')).Settings.text === 'Hello {pagename}', 'text saved');
    await b.set('#editor .ed-settings input[type=color]', '#ff0000');
    await until(async () => (await d.action('Neo', '1,0')).Settings.color === '#ff0000', 'colour saved');
    // A ready-made gradient as the background (the 10th swatch: none, 8 colours, gradients).
    await b.eval(`document.querySelectorAll('#editor .ed-settings .bg-swatches .bg-swatch')[9].click()`);
    await until(async () => /^linear-gradient/.test((await d.action('Neo', '1,0')).Settings.background || ''), 'gradient saved');
    // The info bar picture: the gradient's first colour in the corner, red text in the middle.
    const px = await b.eval(`(async () => {
      const { renderUrl } = await import('/store.js');
      const img = new Image(); img.src = renderUrl('Neo', '1,0', 248) + '&t=' + Date.now(); await img.decode();
      const c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
      const g = c.getContext('2d'); g.drawImage(img, 0, 0);
      const d = g.getImageData(0, 0, c.width, c.height).data;
      let red = 0; for (let i = 0; i < d.length; i += 4) if (d[i] > 200 && d[i + 1] < 80 && d[i + 2] < 80) red++;
      return { corner: [...g.getImageData(1, 1, 1, 1).data].slice(0, 3), red, w: img.width };
    })()`);
    eq(px.w, 248, 'info bar size');
    assert(px.corner[2] > 180 && px.corner[0] < 120, `gradient in the corner: ${J(px.corner)}`);
    assert(px.red > 50, `red text drawn: ${px.red} px`);
    // The canvas shows the same.
    await b.waitFor(`document.querySelector('${slotSel('Neo', '1,0')} img')?.complete`, 'canvas picture');
    // Back to the clock for the following tests.
    await d.clear('Neo', '1,0');
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'POST', { controller: 'Neo', coord: '1,0', uuid: clock.UUID });
    await d.api(`/api/pages/${dev.profile}/${dev.page}/slot`, 'PATCH', { controller: 'Neo', coord: '1,0', settings: clock.Settings });
  });

  await test('editor: every built-in action can be placed and opens its editor without errors', async () => {
    await d.clear('Keypad', '2,1');
    await slotShows(b, 'Keypad', '2,1', false);
    // The deck field only shows with several decks.
    const actions = (await b.S(`S.catalogue.flatMap((g) => g.actions.map((a) => ({ uuid: a.uuid, name: a.name, ctl: a.controllers || ['Keypad'],
      form: (a.form || []).filter((f) => f.type !== 'device' || S.state.devices.length > 1).length, pi: !!a.pi })))`))
      .filter((a) => !a.pi);
    // The folder last (replacing it would ask first).
    actions.sort((x, y) => (x.uuid.endsWith('openchild') ? 1 : 0) - (y.uuid.endsWith('openchild') ? 1 : 0));
    assert(actions.length >= 25, `catalogue has ${actions.length} actions`);
    for (const a of actions) {
      const [ctl, coord] = a.ctl.includes('Keypad') ? ['Keypad', '2,1'] : ['Neo', '1,0'];
      const errors0 = b.errors.length;
      await placeAndEdit(b, d, ctl, coord, a.uuid);
      await b.waitFor(`document.querySelectorAll('#editor .ed-settings > .form > .form-row').length === ${a.form}`, `editor of ${a.uuid} with ${a.form} fields`);
      if (!a.form) assert((await b.text('#editor .ed-settings')).includes('This action has no settings.'), `${a.uuid}: no settings note`);
      if (b.errors.length > errors0) throw new Error(`${a.uuid}: ${b.errors.slice(errors0).join('\n')}`);
    }
    await d.clear('Keypad', '2,1');
    await slotShows(b, 'Keypad', '2,1', false);
  });

  await test('settings: ⌘ as Ctrl, platform, touch colour and key injector are saved', async () => {
    await b.eval(`document.getElementById('settings-btn').focus()`);
    await b.click('#settings-btn');
    await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings dialog');
    const set = async () => (await d.state()).settings;
    assert(!(await b.eval(`document.getElementById('set-touch-row').hidden`)), 'touch colour offered on a Neo');
    assert(!(await b.eval(`document.getElementById('set-injector-row').hidden`)), 'injector offered');
    eq(await b.eval(`[...document.querySelectorAll('#set-devices li b')].map((x) => x.textContent)`), ['Stream Deck Neo']);
    await b.set('#set-cmd', false);
    await until(async () => (await set()).cmd_as_ctrl === false, 'cmd_as_ctrl');
    await b.set('#set-platform', 'linux');
    await until(async () => (await set()).platform === 'linux', 'platform');
    await b.set('#set-touch', '#ff8800');
    await until(async () => (await set()).touch_color === '#ff8800', 'touch colour');
    await b.set('#set-injector', 'xdotool');
    await until(async () => (await set()).injector === 'xdotool', 'injector');
    await b.waitFor(`document.getElementById('set-injector-active').textContent.startsWith('Active: ')`, 'active backend shown');
    await b.set('#set-injector', 'auto');
    await until(async () => (await set()).injector === 'auto', 'injector auto');
    await b.set('#set-cmd', true);
    await until(async () => (await set()).cmd_as_ctrl === true, 'cmd_as_ctrl back');
    await b.clickText('#settings-dialog .modal-buttons button', 'Close');
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed');
    await b.waitFor(`document.activeElement.id === 'settings-btn'`, 'focus back on the settings button');
  });

  await test('backups: listed with their kind; a stored backup and a new one download', async () => {
    await b.click('#settings-btn');
    await b.waitFor(`document.querySelectorAll('#backup-list li a').length > 0`, 'backup list');
    assert((await b.text('#backup-list li .grow')).includes('Daily backup'), 'daily backup at startup');
    const before = fs.readdirSync(b.downloads);
    await b.click('#backup-list li a');
    const f = await b.download(before);
    assert(fs.readFileSync(f).readUInt32LE(0) === 0x04034b50, 'a zip');
    const before2 = fs.readdirSync(b.downloads);
    await b.click('#backup-create');
    await b.toast('The backup is being created and downloaded.');
    const f2 = await b.download(before2);
    assert(/tuxdeck-backup.*\.zip$/.test(path.basename(f2)) && fs.readFileSync(f2).readUInt32LE(0) === 0x04034b50, `downloaded ${path.basename(f2)}`);
    await b.clickText('#settings-dialog .modal-buttons button', 'Close');
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed');
  });

  await test('dialogs: Tab stays inside, Escape closes, the focus returns', async () => {
    await b.eval(`document.getElementById('settings-btn').focus()`);
    await b.click('#settings-btn');
    await b.waitFor(`document.getElementById('settings-dialog').open`, 'open');
    for (let i = 0; i < 25; i++) {
      await b.key('Tab', { keyCode: KEYS.Tab });
      assert(await b.eval(`!!document.activeElement?.closest('#settings-dialog') || document.activeElement === document.body`), `focus left the dialog after ${i + 1} Tabs: ${await b.eval('document.activeElement.outerHTML.slice(0, 80)')}`);
    }
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed with Escape');
    await b.waitFor(`document.activeElement.id === 'settings-btn'`, 'focus back on the settings button');
    // Generic modal (New profile): Escape cancels, nothing is created.
    const n = (await d.state()).profiles.length;
    await b.profileMenu('New profile…');
    await b.waitDialog('New profile');
    assert(await b.eval(`document.activeElement.matches('dialog[open] input')`), 'name field focused');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitNoDialog();
    eq((await d.state()).profiles.length, n, 'nothing created');
    await b.waitFor(`document.activeElement.id === 'profile-menu-btn'`, 'focus back on the menu button');
  });

  await test('accessibility: every visible button has a text or label', async () => {
    await selectSlot(b, 'Keypad', '1,1');
    eq(await b.eval(UNLABELLED_BUTTONS), [], 'main view');
    await b.click('#settings-btn');
    await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings');
    eq(await b.eval(UNLABELLED_BUTTONS), [], 'settings dialog');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed');
    eq(await b.eval(`[...document.querySelectorAll('img')].filter((i) => !i.hasAttribute('alt')).map((i) => i.src.slice(0, 80))`), [], 'images have alt');
    eq(await b.eval(`[...document.querySelectorAll('input:not([type=hidden]):not([type=file]), select, textarea')].filter((e) => !e.closest('[hidden]') && e.getClientRects().length)
      .filter((e) => !(e.labels?.length || e.getAttribute('aria-label') || e.getAttribute('title') || e.getAttribute('placeholder'))).map((e) => e.outerHTML.slice(0, 100))`), [], 'form fields have labels');
  });

  await test('toasts: a status dot instead of a coloured left edge', async () => {
    await b.eval(`(async () => { const { toast } = await import('/dialogs.js'); toast('Info toast', 'info', 8000); toast('Warn toast', 'warn', 8000); toast('Error toast', 'error', 8000); })()`);
    await b.toast('Error toast');
    const styles = await b.eval(`[...document.querySelectorAll('#toasts .toast')].slice(-3).map((t) => {
      const s = getComputedStyle(t); const dot = getComputedStyle(t, '::before');
      return { left: s.borderLeftWidth, top: s.borderTopWidth, leftColor: s.borderLeftColor, topColor: s.borderTopColor, dot: [dot.width, dot.height, dot.borderRadius], bg: dot.backgroundColor };
    })`);
    for (const s of styles) {
      eq([s.left, s.leftColor], [s.top, s.topColor], 'left edge like the others');
      eq(s.dot, ['8px', '8px', '50%'], 'round status dot');
    }
    eq(new Set(styles.map((s) => s.bg)).size, 3, 'one colour per level');
    await b.eval(`document.querySelectorAll('#toasts .toast-close').forEach((x) => x.click())`);
    eq(await b.count('#toasts .toast'), 0, 'closed');
  });

  await test('settings: switch the language to German and back; no English left in German', async () => {
    const setLang = async (lang) => {
      await b.click('#settings-btn');
      await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings dialog');
      // The daily backup made at startup is listed.
      await b.waitFor(`document.querySelectorAll('#backup-list li button').length > 0`, 'backup list');
      // The UI reloads with the new dictionary.
      await b.reload(() => b.set('#set-language', lang));
    };
    await setLang('de');
    try {
    await b.waitFor(`document.getElementById('action-search').placeholder.startsWith('Aktionen')`, 'German UI');
    eq(await b.eval(`document.documentElement.lang`), 'de');
    eq(await b.text('#sidebar-btn span[data-i18n]'), 'Aktionen');
    eq(await b.text('label[for="profile-select"]'), 'Profil');
    eq(await b.text('#conn-indicator'), 'Live');
    // Every text and label shown must be translated (the dictionary has an entry with a
    // different German text for each English source string that is still visible).
    const de = await b.eval(`fetch('/i18n/de.json').then((r) => r.json())`);
    // Names of profiles are the user's (the starter profile keeps the name it was created with).
    const names = new Set((await d.state()).profiles.map((p) => p.name));
    const english = (texts) => texts.filter((s) => de[s] && de[s] !== s && !names.has(s));
    const views = {};
    views.main = english(await b.eval(VISIBLE_TEXTS));
    await selectSlot(b, 'Keypad', '1,1');
    await b.waitFor(`!!document.querySelector('#editor .ed-press')`, 'editor');
    eq(await b.text('#editor .ed-press'), '▶ Test drücken');
    views.editor = english(await b.eval(VISIBLE_TEXTS));
    await b.click('#profile-menu-btn');
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'menu');
    views.menu = english(await b.eval(VISIBLE_TEXTS));
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.click('#settings-btn');
    await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings');
    eq(await b.text('#settings-dialog h2'), 'Einstellungen');
    views.settings = english(await b.eval(VISIBLE_TEXTS));
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.click('#market-btn');
    await b.waitFor(`document.querySelectorAll('#market .mk-card').length > 0`, 'plugins view');
    views.market = english(await b.eval(VISIBLE_TEXTS));
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.getElementById('market').hidden`, 'plugins view closed');
    for (const [view, left] of Object.entries(views)) eq(left, [], `untranslated in ${view}`);
    await setLang('en');
    } finally {
      // Back to English also when something above failed (the next tests read English texts).
      if (await b.eval(`document.documentElement.lang !== 'en'`)) {
        await b.eval(`(() => { document.querySelectorAll('dialog[open]').forEach((x) => x.close()); if (location.hash) history.replaceState(null, '', '/'); })()`);
        await b.reload(() => d.api('/api/settings', 'POST', { language: 'en' }).then(() => b.send('Page.reload', {})));
      }
    }
    await b.waitFor(`document.getElementById('action-search').placeholder.startsWith('Search')`, 'English UI');
  });

  await test('offline: an edit made while the service is down is saved after reconnecting', async () => {
    // Only a UI that is live can tell "the service went away" from "not connected yet".
    await b.waitFor(Browser.READY, 'UI ready', 10000);
    await b.waitFor(`document.querySelector('${slotSel('Keypad', '0,0')}')?.classList.contains('filled')`, 'slot 0,0 ready', 10000);
    await selectSlot(b, 'Keypad', '0,0');
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field', 10000);
    b.offline = true;
    try {
      await d.stop();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('bad')`, 'disconnected', 10000);
      assert(await b.eval(`document.getElementById('device-status').classList.contains('unknown')`), 'device state unknown');
      assert(await b.eval(`document.querySelector('#editor .ed-press').disabled`), 'no test press without the service');
      await typeTitle(b, 'Offline edit');
      await b.waitFor(`!document.querySelector('#editor .ed-unsaved').hidden`, '"Not saved yet" badge');
      await b.toast('The service is not reachable');
      await d.start();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'reconnected', 15000);
      await until(async () => (await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'Offline edit', 'queued edit saved', 6000).catch(async (e) => { throw new Error(e.message + ' ' + await b.diag()); });
      await b.waitFor(`document.querySelector('#editor .ed-unsaved').hidden`, 'badge gone');
      await b.toast('Saved 1 kept change');
    } finally {
      b.offline = false;
    }
  });

  await test('offline: an edit kept before the live connection was ever up is saved on the first connect', async () => {
    // The page is loaded, but its event socket has not connected yet when the service goes
    // away (e.g. it restarts right after opening the UI). The socket is held back by
    // pointing it at a closed port until `__wsAllowed` is set.
    const { identifier } = await b.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `(() => { const Real = WebSocket;
        window.WebSocket = function (url, protocols) { return new Real(window.__wsAllowed || !String(url).includes('/api/events') ? url : 'ws://127.0.0.1:9/', protocols); };
        window.WebSocket.prototype = Real.prototype; Object.assign(window.WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 }); })();`,
    });
    b.offline = true;
    try {
      await b.eval('window.__stale = true');
      await b.send('Page.reload', {});
      await b.waitFor(`!window.__stale && document.querySelector('${slotSel('Keypad', '0,0')}')?.classList.contains('filled')`, 'page loaded', 10000);
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('bad')`, 'no live connection yet', 10000);
      await selectSlot(b, 'Keypad', '0,0');
      await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field');
      await d.stop();
      await typeTitle(b, 'Kept early');
      await b.waitFor(`!document.querySelector('#editor .ed-unsaved').hidden`, '"Not saved yet" badge');
      await d.start();
      await b.eval('window.__wsAllowed = true');
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'connected', 20000);
      await until(async () => (await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'Kept early', 'kept edit saved on the first connect', 6000);
      await b.waitFor(`document.querySelector('#editor .ed-unsaved').hidden`, 'badge gone');
    } finally {
      await b.send('Page.removeScriptToEvaluateOnNewDocument', { identifier });
      if (!(await d.state().catch(() => null))) await d.start();
      b.offline = false;
    }
    await b.reload();
  });

  await test('offline: after an outage without edits, an edit during the next outage is still saved', async () => {
    await selectSlot(b, 'Keypad', '0,0');
    await b.waitFor(`!!document.querySelector('#editor .ed-titlecol textarea')`, 'title field');
    b.offline = true;
    try {
      // First outage: nothing to keep (the retry finds an empty queue).
      await d.stop();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('bad')`, 'disconnected', 10000);
      await d.start();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'reconnected', 15000);
      // Second outage with an edit.
      await d.stop();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('bad')`, 'disconnected again', 10000);
      await typeTitle(b, 'Second outage');
      await b.waitFor(`!document.querySelector('#editor .ed-unsaved').hidden`, '"Not saved yet" badge');
      await d.start();
      await b.waitFor(`document.getElementById('conn-indicator').classList.contains('ok')`, 'reconnected again', 15000);
      await until(async () => (await d.action('Keypad', '0,0'))?.States?.[0]?.Title === 'Second outage', 'edit of the second outage saved', 6000)
        .catch(async (e) => { throw new Error(`${e.message} ${await b.diag()}`); });
      await b.waitFor(`document.querySelector('#editor .ed-unsaved').hidden`, 'badge gone');
    } finally {
      if (!(await d.state().catch(() => null))) await d.start();
      b.offline = false;
    }
  });
}

/** Profiles: create, switch, rename, smart profile, delete, export; samples; conversion; backups. */
async function suiteProfiles(d, b) {
  await b.open(await d.loginUrl());
  const st0 = await d.state();
  const DEFAULT = st0.devices[0].profile;
  const profile = async (id) => (await d.state()).profiles.find((p) => p.id === id);
  const active = async () => (await d.dev()).profile;
  const selectedProfile = `document.getElementById('profile-select').selectedOptions[0]?.textContent`;

  await test('profiles: the only profile of a device cannot be deleted', async () => {
    await b.click('#profile-menu-btn');
    await b.waitFor(`!!document.querySelector('.ctxmenu')`, 'menu');
    const labels = await b.eval(`[...document.querySelectorAll('.ctxmenu .ctx-item')].map((i) => i.querySelector('span').textContent)`);
    eq(labels, ['New profile…', 'Add a sample profile…', 'Rename…', 'Smart Profile (per application)…', 'Delete…', 'Export', 'Import (profile / plugin / icon pack)…', 'Manage icon packs…']);
    const del = await b.eval(`(() => { const i = [...document.querySelectorAll('.ctxmenu .ctx-item')].find((x) => x.textContent === 'Delete…'); return [i.disabled, i.title]; })()`);
    eq(del, [true, 'This is the only profile for the device. Create another profile first.']);
    // Arrow keys walk through the menu.
    const first = await b.eval(`document.activeElement.textContent`);
    eq(first, 'New profile…', 'first entry focused');
    await b.key('ArrowDown', { keyCode: KEYS.ArrowDown });
    eq(await b.eval(`document.activeElement.textContent`), 'Add a sample profile…');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.querySelector('.ctxmenu')`, 'menu closed');
  });

  let created;
  await test('profiles: a new profile is created from the menu and becomes active', async () => {
    await b.profileMenu('New profile…');
    await b.waitDialog('New profile');
    eq(await b.eval(`document.querySelector('dialog[open] input').value`), 'New profile');
    assert((await b.text('dialog[open] .modal-text')).includes('For Stream Deck Neo'), 'for the connected deck');
    await b.eval(`document.querySelector('dialog[open] input').value = 'UI Profile'`);
    await b.dialogButton('Create');
    created = await until(async () => { const id = await active(); return id !== DEFAULT && id; }, 'new profile active');
    eq((await profile(created)).name, 'UI Profile');
    await b.waitFor(`${selectedProfile} === 'UI Profile'`, 'shown in the list');
    await b.toast('Profile "UI Profile" created');
    eq(await b.count('#device .slot.filled[data-controller="Keypad"]'), 0, 'empty profile');
  });

  await test('profiles: the profile list switches the profile on the device', async () => {
    await b.set('#profile-select', DEFAULT);
    await until(async () => (await active()) === DEFAULT, 'default profile');
    await b.waitFor(`document.querySelectorAll('#device .slot.filled[data-controller="Keypad"]').length === 8`, 'its keys');
    await b.set('#profile-select', created);
    await until(async () => (await active()) === created, 'back to the new profile');
    await b.waitFor(`${selectedProfile} === 'UI Profile'`, 'selected');
  });

  await test('profiles: rename (Enter confirms)', async () => {
    await b.profileMenu('Rename…');
    await b.waitDialog('Rename profile');
    await b.eval(`document.querySelector('dialog[open] input').value = 'UI Renamed'`);
    await b.key('Enter', { keyCode: KEYS.Enter });
    await until(async () => (await profile(created)).name === 'UI Renamed', 'renamed');
    await b.waitFor(`${selectedProfile} === 'UI Renamed'`, 'list updated');
    await b.waitFor(`document.querySelector('#crumbs .crumb')?.textContent === 'UI Renamed'`, 'breadcrumb updated');
  });

  await test('profiles: link to an application (Smart Profile) and remove the link', async () => {
    await b.profileMenu('Smart Profile (per application)…');
    await b.waitDialog('Smart Profile – UI Renamed');
    await b.eval(`document.querySelector('dialog[open] input').value = 'firefox'`);
    await b.dialogButton('Save');
    await until(async () => (await profile(created)).app === 'firefox', 'linked');
    await b.waitFor(`${selectedProfile} === 'UI Renamed ⟳ firefox'`, 'shown in the list');
    await b.profileMenu('Smart Profile: firefox…');
    await b.waitDialog('Smart Profile – UI Renamed');
    await b.dialogButton('Remove link');
    await until(async () => !(await profile(created)).app, 'unlinked');
    await b.toast('Link removed');
  });

  await test('profiles: export downloads the profile', async () => {
    const before = fs.readdirSync(b.downloads);
    await b.profileMenu('Export');
    const f = await b.download(before);
    assert(path.basename(f).endsWith('.streamDeckProfile'), path.basename(f));
    assert(fs.readFileSync(f).readUInt32LE(0) === 0x04034b50, 'a zip');
  });

  await test('profiles: delete asks first; afterwards the device shows another profile', async () => {
    await b.profileMenu('Delete…');
    await b.waitDialog('Delete profile');
    assert((await b.text('dialog[open] .modal-text')).includes('"UI Renamed"'), 'names the profile');
    eq(await b.eval(`document.activeElement.textContent`), 'Cancel', 'Cancel is focused (no accidental delete)');
    await b.dialogButton('Cancel');
    await b.waitNoDialog();
    assert(await profile(created), 'kept');
    await b.profileMenu('Delete…');
    await b.waitDialog('Delete profile');
    await b.dialogButton('Delete');
    await until(async () => !(await profile(created)), 'deleted');
    await until(async () => (await active()) === DEFAULT, 'default profile shown');
    await b.waitFor(`document.querySelectorAll('#profile-select option').length === 1`, 'list updated');
  });

  await test('samples: the picker shows Standard, Work, Play and Streaming; "Work" is created and activated', async () => {
    await b.profileMenu('Add a sample profile…');
    await b.waitDialog('Add a sample profile');
    await b.waitFor(`document.querySelectorAll('dialog[open] .sample-card').length === 4`, 'four cards');
    eq(await b.eval(`[...document.querySelectorAll('dialog[open] .sample-name')].map((x) => x.textContent)`), ['Standard', 'Work', 'Play', 'Streaming']);
    eq(await b.eval(`[...document.querySelectorAll('dialog[open] .sample-card')].map((c) => c.querySelectorAll('.sample-keys img').length)`), [8, 8, 8, 8], 'previews of eight keys');
    eq(await b.eval(`[...document.querySelectorAll('dialog[open] .sample-card')].map((c) => c.classList.contains('selected'))`), [true, false, false, false], 'first one preselected');
    await b.clickText('dialog[open] .sample-card', 'Work');
    eq(await b.eval(`[...document.querySelectorAll('dialog[open] .sample-card')].map((c) => c.classList.contains('selected'))`), [false, true, false, false], 'Work selected');
    await b.dialogButton('Add');
    const id = await until(async () => { const a = await active(); return a !== DEFAULT && a; }, 'sample profile active', 10000);
    const prof = await profile(id);
    eq([prof.name, prof.pages.length], ['Work', 2]);
    const keys = (await d.page(id, prof.pages[0])).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq(Object.keys(keys).length, 8, 'eight keys');
    assert(Object.values(keys).every((a) => /^data:image\/svg/.test(a.States[0].Image || '')), 'every key has its icon');
    await b.waitFor(`${selectedProfile} === 'Work' && document.querySelectorAll('#device .slot.filled[data-controller="Keypad"]').length === 8`, 'shown');
    eq(await b.count('#pagebar .page-tab'), 2, 'two pages');
    assert(await b.eval(`document.getElementById('compat-banner').hidden`), 'no conversion banner for a sample');
    // Cancel in the picker creates nothing.
    const n = (await d.state()).profiles.length;
    await b.profileMenu('Add a sample profile…');
    await b.waitDialog('Add a sample profile');
    await b.dialogButton('Cancel');
    await b.waitNoDialog();
    eq((await d.state()).profiles.length, n, 'nothing added');
  });

  // A profile as exported by the official app on Windows: built here through the API
  // (keys with Windows paths and Windows-only plugins), exported and deleted again.
  const windowsProfile = async () => {
    const p = await d.api('/api/profiles', 'POST', { name: 'From Windows', model: '20GBJ9901' });
    const pg = (await profile(p.id)).pages[0];
    const key = (uuid, title, settings = {}) => ({ UUID: uuid, Name: title || uuid, Settings: settings, State: 0, States: [{ Title: title }] });
    const paste = (coord, inst) => d.api(`/api/pages/${p.id}/${pg}/paste`, 'POST', { controller: 'Keypad', coord, instance: inst, from_profile: p.id, from_page: pg });
    await paste('0,0', key('com.elgato.streamdeck.system.open', 'Microsoft\nOutlook', { path: '"C:\\Program Files\\Microsoft Office\\root\\Office16\\OUTLOOK.EXE"' }));
    await paste('1,0', key('com.elgato.streamdeck.system.open', 'Riot Games', { path: '"C:\\\\ProgramData\\\\Riot Games\\\\RiotClientServices.exe"' }));
    await paste('2,0', key('com.barraider.spotifyplaypause', ''));
    await paste('3,0', key('com.microsoft.teams.button.togglemute', ''));
    await paste('0,1', key('com.elgato.twitch.marker', ''));
    const file = tmpFile('From Windows.streamDeckProfile', await d.api(`/api/profiles/${p.id}/export`));
    await d.api(`/api/profiles/${p.id}`, 'DELETE');
    return file;
  };
  const windowsFile = await windowsProfile();
  const banner = `document.getElementById('compat-banner')`;

  let converted;
  await test('convert: importing a Windows profile offers the conversion; Convert rewrites the keys', async () => {
    const before = new Set((await d.state()).profiles.map((p) => p.id));
    b.chooseFiles(windowsFile);
    await b.profileMenu('Import (profile / plugin / icon pack)…');
    await b.toast('Profile imported');
    await b.waitDialog('Convert for Linux?', 10000);
    converted = (await d.state()).profiles.find((p) => !before.has(p.id))?.id;
    assert(converted, 'imported');
    await until(async () => (await active()) === converted, 'imported profile active');
    const sections = await b.eval(`[...document.querySelectorAll('dialog[open] .convert-item')].map((s) => s.querySelector('.convert-head').textContent)`);
    eq(sections, ['1 key opens the web version', '2 keys switch to Linux actions',
      '2 keys have no Linux counterpart: they keep their title and icon and wait for a command of your own']);
    const chips = await b.eval(`[...document.querySelectorAll('dialog[open] .convert-list li')].map((x) => x.textContent)`);
    assert(chips.includes('Microsoft Outlook → outlook.office.com/mail'), J(chips));
    assert(chips.includes('Riot Games'), J(chips));
    assert((await b.text('dialog[open] .convert')).includes('A backup is made first'), 'backup note');
    await b.dialogButton('Convert');
    await b.toast('Profile converted. 2 keys still need a command.', 8000);
    const kp = (await d.page(converted, (await profile(converted)).pages[0])).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq([kp['0,0'].UUID, kp['0,0'].Settings.path, kp['0,0'].States[0].Title], ['com.elgato.streamdeck.system.website', 'https://outlook.office.com/mail/', 'Microsoft\nOutlook']);
    eq([kp['2,0'].UUID, kp['3,0'].UUID], ['com.elgato.streamdeck.system.multimedia', 'com.elgato.streamdeck.system.hotkey']);
    eq((await d.api(`/api/profiles/${converted}/conversion`)).needed, false);
    await b.waitFor(`${banner}.hidden`, 'no banner after converting');
  });

  await test('convert: "Not now" leaves a banner that offers it again; closing it hides it for good', async () => {
    const before = new Set((await d.state()).profiles.map((p) => p.id));
    b.chooseFiles(windowsFile);
    await b.profileMenu('Import (profile / plugin / icon pack)…');
    await b.waitDialog('Convert for Linux?', 10000);
    const id = (await d.state()).profiles.find((p) => !before.has(p.id))?.id;
    await b.dialogButton('Not now');
    await b.waitNoDialog();
    await b.waitFor(`!${banner}.hidden`, 'banner');
    eq(await b.text('#compat-banner .compat-text'), 'This profile comes from Windows or macOS.');
    eq((await d.api(`/api/profiles/${id}/conversion`)).needed, true, 'not converted');
    // The banner's button opens the same dialog; Escape keeps everything as it is.
    await b.clickText('#compat-banner button', 'Convert for Linux…');
    await b.waitDialog('Convert for Linux?');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitNoDialog();
    assert(!(await b.eval(`${banner}.hidden`)), 'banner stays');
    await b.click('#compat-banner .compat-close');
    await b.waitFor(`${banner}.hidden`, 'banner hidden');
    eq(await b.eval(`localStorage.getItem('tuxdeck.convert-dismissed.${id}')`), '1', 'remembered');
    await b.reload();
    assert(await b.eval(`${banner}.hidden`), 'still hidden after reloading');
    eq((await d.api(`/api/profiles/${id}/conversion`)).needed, true, 'still not converted');
  });

  await test('convert: an ordinary profile shows neither banner nor dialog', async () => {
    // Also with built-in actions that belong to a plugin of their own (multi action, pages).
    const pg = (await profile(DEFAULT)).pages[0];
    for (const [coord, uuid] of [['0,1', 'com.elgato.streamdeck.multiactions.routine'], ['1,1', 'com.elgato.streamdeck.page.next'], ['2,1', 'com.elgato.streamdeck.soundboard.stopaudioplay']]) {
      await d.api(`/api/pages/${DEFAULT}/${pg}/slot`, 'POST', { controller: 'Keypad', coord, uuid });
    }
    eq((await d.api(`/api/profiles/${DEFAULT}/conversion`)).needed, false, 'nothing to convert');
    const from = b.responses.length;
    await b.set('#profile-select', DEFAULT);
    await until(async () => (await active()) === DEFAULT, 'default profile');
    await b.response(new RegExp(`/api/profiles/${DEFAULT}/conversion`), from);
    await b.eval('0'); // one more round trip: the answer has been handled
    assert(await b.eval(`${banner}.hidden`), 'no banner');
    // Importing an ordinary profile (dropped onto the window) offers nothing.
    const before = new Set((await d.state()).profiles.map((p) => p.id));
    const data = (await d.api(`/api/profiles/${DEFAULT}/export`)).toString('base64');
    const from2 = b.responses.length;
    await b.eval(`(() => {
      const bytes = Uint8Array.from(atob(${J(data)}), (c) => c.charCodeAt(0));
      const dt = new DataTransfer(); dt.items.add(new File([bytes], 'Plain.streamDeckProfile'));
      document.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt }));
      document.body.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt }));
    })()`);
    await b.toast('Profile imported');
    const id = await until(async () => (await d.state()).profiles.find((p) => !before.has(p.id))?.id, 'imported');
    await b.response(new RegExp(`/api/profiles/${id}/conversion`), from2);
    await b.eval('0');
    assert(!(await b.exists('dialog[open]')), 'no dialog');
    assert(await b.eval(`${banner}.hidden`), 'no banner');
  });

  await test('backups: the backup made before converting has its own label', async () => {
    await b.click('#settings-btn');
    await b.waitFor(`document.querySelectorAll('#backup-list li a').length > 1`, 'backup list');
    const rows = await b.eval(`[...document.querySelectorAll('#backup-list li .grow')].map((x) => x.textContent)`);
    assert(rows.some((r) => r.includes('Before converting a profile')), J(rows));
    assert(rows.some((r) => r.includes('Daily backup')), J(rows));
    assert(!rows.some((r) => / · Backup · /.test(r)), `every kind is named: ${J(rows)}`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed');
  });

  await test('backups: restoring a stored backup asks first, then reloads the UI', async () => {
    const names = (await d.state()).profiles.map((p) => p.name).sort();
    await b.click('#settings-btn');
    await b.waitFor(`document.querySelectorAll('#backup-list li button').length > 0`, 'backup list');
    // The daily backup from the start: only the default profile.
    const idx = await b.eval(`[...document.querySelectorAll('#backup-list li')].findIndex((li) => li.textContent.includes('Daily backup'))`);
    assert(idx >= 0, 'daily backup listed');
    await b.eval(`document.querySelectorAll('#backup-list li')[${idx}].querySelector('button').click()`);
    await b.waitDialog('Restore backup');
    await b.dialogButton('Cancel');
    await b.waitFor(`document.querySelectorAll('dialog[open]').length === 1`, 'confirmation closed');
    eq((await d.state()).profiles.map((p) => p.name).sort(), names, 'nothing restored after Cancel');
    await b.eval(`document.querySelectorAll('#backup-list li')[${idx}].querySelector('button').click()`);
    await b.waitDialog('Restore backup');
    // Until the UI reloads, it may still ask for profiles that the restore just removed.
    b.offline = true;
    try {
      await b.reload(async () => {
        await b.dialogButton('Restore');
        await until(async () => (await d.state()).profiles.length === 1, 'restored', 10000);
      });
    } finally {
      b.offline = false;
    }
    eq((await d.state()).profiles.map((p) => p.name), ['Default Profile']);
    await b.waitFor(`document.querySelectorAll('#profile-select option').length === 1`, 'UI shows the restored profiles');
  });
}

/** Plugins view with the fake catalogue; installed plugins and icon packs; property inspector. */
async function suiteMarket(d, b) {
  await d.api('/api/import', 'POST', PLUGIN, { 'x-filename': 'com.ui.counter.streamDeckPlugin', 'content-type': 'application/octet-stream' });
  await d.api('/api/import', 'POST', ICONPACK, { 'x-filename': 'com.ui.icons.streamDeckIconPack', 'content-type': 'application/octet-stream' });
  await b.open(await d.loginUrl());
  const cards = `document.querySelectorAll('#market .mk-grid > .mk-card')`;
  const cardNames = `[...${cards}].map((c) => c.querySelector('.mk-card-name').textContent)`;
  const card = (id) => `#market .mk-card[data-id="${id}"]`;

  await test('plugins view: opens from the top bar and loads the catalogue page by page', async () => {
    await b.click('#market-btn');
    await b.waitFor(`!document.getElementById('market').hidden && location.hash === '#market'`, 'plugins view');
    assert(await b.eval(`document.activeElement.classList.contains('mk-search')`), 'search focused');
    await b.waitFor(`${cards}.length === 30`, 'all 30 entries', 10000);
    assert(b.requests.includes('GET /api/catalogue?offset=0&limit=24'), 'first page of 24');
    assert(b.requests.some((r) => r.startsWith('GET /api/catalogue?offset=24')), 'next page');
    await b.waitFor(`document.querySelector('#market .mk-status')?.textContent === '30 entries'`, 'count');
    eq((await b.eval(cardNames)).slice(0, 2), ['Tool 30', 'Tool 29'], 'catalogue order');
    assert((await b.text(card('com.ui.cat03'))).includes('AI-assisted'), 'AI badge');
  });

  await test('plugins view: search and sort order', async () => {
    await b.eval(`(() => { const s = document.querySelector('#market .mk-search'); s.value = 'zebra'; s.dispatchEvent(new Event('input')); s.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' })); })()`);
    await b.waitFor(`${cards}.length === 1`, 'one result');
    eq(await b.eval(cardNames), ['Tool 24']);
    await b.eval(`(() => { const s = document.querySelector('#market .mk-search'); s.value = 'nothing like this'; s.dispatchEvent(new Event('input')); s.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' })); })()`);
    await b.waitFor(`document.querySelector('#market .mk-status')?.textContent === 'No results for "nothing like this".'`, 'no results');
    await b.eval(`(() => { const s = document.querySelector('#market .mk-search'); s.value = ''; s.dispatchEvent(new Event('input')); s.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' })); })()`);
    await b.set('#market .mk-sort', 'name');
    await b.waitFor(`${cards}.length === 30 && ${cardNames}[0] === 'Tool 02'`, 'sorted by name', 10000);
    eq((await b.eval(cardNames)).slice(0, 3), ['Tool 02', 'Tool 03', 'Tool 04']);
    await b.set('#market .mk-sort', '');
    await b.waitFor(`${cards}.length === 30 && ${cardNames}[0] === 'Tool 30'`, 'catalogue order again', 10000);
  });

  await test('plugins view: Linux check badges (own and published results)', async () => {
    const badge = (id) => b.eval(`(() => { const x = document.querySelector('${card(id)} .mk-badge:last-child'); return x ? [x.textContent, x.className] : null; })()`);
    eq(await badge('com.ui.cat01'), ['Tested on Linux', 'mk-badge ok']);
    eq(await badge('com.ui.cat02'), ['Issues on Linux', 'mk-badge warn']);
    eq(await badge('com.ui.cat04'), ['Fails on Linux', 'mk-badge bad']);
    eq(await badge('com.ui.cat05'), ['Not for Linux', 'mk-badge bad']);
    eq(await badge('com.ui.cat06'), ['Tested on Linux', 'mk-badge ok']);
    assert((await b.eval(`document.querySelector('${card('com.ui.cat06')} .mk-badge.ok').title`)).includes('tested by the TuxDeck plugin test'), 'published result');
    assert((await b.eval(`document.querySelector('${card('com.ui.cat01')} .mk-badge.ok').title`)).includes('tested on this computer'), 'own result');
    assert(!(await b.exists(`${card('com.ui.cat08')} .mk-badge`)), 'untested: no badge');
  });

  await test('plugins view: the detail panel shows the Linux check and links; Escape closes panel, then view', async () => {
    await b.click(card('com.ui.cat02'));
    await b.waitFor(`!document.querySelector('#market .mk-detail').hidden && !!document.querySelector('#market .mk-detail .mk-meta, #market .mk-detail .mk-section h3')`, 'details');
    await b.waitFor(`!document.querySelector('#market .mk-install-btn').disabled`, 'Install enabled after loading');
    eq(await b.text('#market .mk-detail-name'), 'Tool 29');
    eq(await b.text('#market .mk-install-btn'), 'Install');
    assert((await b.text('#market .mk-linux-verdict')).startsWith('Works on Linux, with issues'), 'verdict');
    const checks = await b.eval(`[...document.querySelectorAll('#market .mk-linux-checks li')].map((li) => li.className + ' ' + li.querySelector('.mk-linux-mark').textContent)`);
    eq(checks.length, 7);
    eq(checks.filter((c) => c.startsWith('bad')), ['bad ✗'], 'one failed check');
    assert((await b.text('#market .mk-linux-checks li.bad')).includes('inspector failed in the test'), 'detail of the failed check');
    eq(await b.eval(`[...document.querySelectorAll('#market .mk-links a')].map((a) => [a.textContent, a.href, a.target])`),
      [['Source code and releases', 'https://github.com/ui-test/cat2', '_blank']]);
    assert(await b.exists(`${card('com.ui.cat02')}.selected`), 'card selected');
    eq(await b.eval(UNLABELLED_BUTTONS), [], 'buttons labelled');
    await b.eval(`document.querySelector('#market .mk-search').focus()`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.querySelector('#market .mk-detail').hidden`, 'panel closed');
    assert(!(await b.eval(`document.getElementById('market').hidden`)), 'view still open');
    await b.click(card('com.ui.cat08'));
    await b.waitFor(`!document.querySelector('#market .mk-detail').hidden`, 'details');
    assert((await b.text('#market .mk-linux')).includes('Not tested on Linux yet.'), 'untested note');
    await b.click('#market .mk-detail-close');
    await b.waitFor(`document.querySelector('#market .mk-detail').hidden`, 'closed with ×');
    await b.eval(`document.querySelector('#market .mk-search').focus()`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.getElementById('market').hidden && location.hash === ''`, 'view closed');
  });

  await test('plugins view: an installed plugin offers the newest version and uninstalling', async () => {
    await b.click('#market-btn');
    await b.waitFor(`!!document.querySelector('${card('com.ui.counter')}')`, 'entry', 10000);
    assert((await b.text(card('com.ui.counter'))).includes('Installed'), 'installed badge');
    await b.click(card('com.ui.counter'));
    await b.waitFor(`!document.querySelector('#market .mk-install-btn')?.disabled`, 'details loaded');
    eq(await b.text('#market .mk-install-btn'), 'Install newest version');
    assert(await b.exists('#market .mk-install .danger-outline'), 'Uninstall button');
    eq(await b.text('#market .mk-detail dl.mk-meta dd:last-child'), '1.0', 'installed version');
    await b.click('#market .mk-detail-close');
  });

  await test('plugins view: a catalogue that cannot be reached shows an error with "Try again"', async () => {
    b.offline = true; // the browser logs the failed request
    try {
      await b.intercept('*/api/catalogue?*', { respond: () => ({ status: 502, body: { error: 'catalogue down (test)' } }) });
      await b.set('#market .mk-sort', 'name');
      await b.waitFor(`!!document.querySelector('#market .mk-error-box')`, 'error box');
      eq(await b.text('#market .mk-error'), 'The plugin catalogue is not reachable. Check the internet connection and try again.');
      eq(await b.text('#market .mk-error-details code'), 'catalogue down (test)', 'technical detail on request');
      await b.clearIntercepts();
      await b.clickText('#market .mk-status button', 'Try again');
      await b.waitFor(`${cards}.length === 30`, 'loaded again', 10000);
      await b.set('#market .mk-sort', '');
      await b.waitFor(`${cards}.length === 30 && ${cardNames}[0] === 'Tool 30'`, 'catalogue order', 10000);
    } finally {
      await b.clearIntercepts();
      b.offline = false;
    }
  });

  await test('plugins view: the Elgato Marketplace help explains how to get plugins', async () => {
    await b.clickText('#market .mk-header button', 'Elgato Marketplace…');
    await b.waitDialog('Plugins from the Elgato Marketplace');
    eq(await b.count('dialog[open] .mk-help ol li'), 4);
    eq(await b.eval(`[...document.querySelectorAll('dialog[open] .mk-help a')].map((a) => [a.href, a.target, a.rel])`), [['https://marketplace.elgato.com/', '_blank', 'noopener noreferrer']]);
    await b.dialogButton('Close');
    await b.waitNoDialog();
    assert(!(await b.eval(`document.getElementById('market').hidden`)), 'view still open');
  });

  await test('plugins view: "Install from file…" installs an icon pack; the Installed tab lists everything', async () => {
    const packs0 = (await d.api('/api/iconpacks')).length; // the bundled pack and UI Icons
    b.chooseFiles(tmpFile('com.ui.more.streamDeckIconPack', ICONPACK2));
    await b.clickText('#market .mk-header button', 'Install from file…');
    await b.toast('Icon pack installed');
    await until(async () => (await d.api('/api/iconpacks')).length === packs0 + 1, 'one more icon pack');
    await b.click('#market .mk-tab[data-tab="installed"]');
    await b.waitFor(`document.querySelectorAll('#market .mk-plugin').length === ${packs0 + 2}`, 'plugin and the packs');
    eq(await b.text('#market .mk-status'), `1 plugin, ${packs0 + 1} icon packs installed`);
    const rows = await b.eval(`[...document.querySelectorAll('#market .mk-plugin-name')].map((x) => x.firstChild.textContent)`);
    eq(rows[0], 'UI Counter', 'plugins first');
    assert(['More Icons', 'UI Icons'].every((n) => rows.includes(n)), J(rows));
    assert(await b.eval(`document.querySelector('#market .mk-search').hidden`), 'no search on the Installed tab');
    eq(await b.eval(UNLABELLED_BUTTONS), [], 'buttons labelled');
    await b.click('#market .mk-tab[data-tab="catalogue"]');
    await b.waitFor(`${cards}.length === 30`, 'catalogue again', 10000);
    await b.eval(`document.querySelector('#market .mk-search').focus()`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.getElementById('market').hidden`, 'closed');
  });

  await test('editor: a plugin action opens its property inspector', async () => {
    await b.waitFor(`!!document.querySelector('.action-item[data-uuid="com.ui.counter.count"]')`, 'plugin action listed');
    // The inspector's refused API request is logged by the browser: expected here.
    b.quiet = true;
    await placeAndEdit(b, d, 'Keypad', '3,1', 'com.ui.counter.count');
    await b.waitFor(`!!document.querySelector('#editor iframe.pi-frame')`, 'property inspector');
    assert((await b.eval(`document.querySelector('#editor iframe.pi-frame').src`)).includes('com.ui.counter.sdPlugin/pi/pi.html'), 'PI url');
    await b.waitFor(`document.querySelector('#editor iframe.pi-frame')?.dataset.state === 'connected'`, 'PI connected', 10000);
    eq(await b.text('#editor .ed-name'), 'Counter');
    // It runs on the plugin file origin: the UI's API is out of reach (and that origin has none).
    assert(!(await b.eval(`document.querySelector('#editor iframe.pi-frame').src`)).startsWith(d.url('/')), 'own origin');
    await until(async () => (await d.action('Keypad', '3,1'))?.Settings?.probe, 'the inspector registered and reported', 10000);
    const { probe, own } = (await d.action('Keypad', '3,1')).Settings;
    eq([probe, own], ['blocked', 404]);
    await until(async () => Object.keys((await d.api('/api/plugins/com.ui.counter/diagnostics')).stats?.inspectors || {}).length > 0, 'counted as connected');
    b.quiet = false;
  });

  await test('editor: an inspector made of sdpi-components gets its list from a library plugin and saves the pick', async () => {
    // The other test inspector (key 3,1) loads again on the way and logs its refused API probe.
    b.quiet = true;
    await d.api('/api/import', 'POST', SDPI_PLUGIN(), { 'x-filename': 'lib3.streamDeckPlugin', 'content-type': 'application/octet-stream' });
    await b.waitFor(`!!document.querySelector('.action-item[data-uuid="com.e2e.lib3.count"]')`, 'library action listed', 10000);
    await placeAndEdit(b, d, 'Keypad', '2,1', 'com.e2e.lib3.count');
    await b.waitFor(`document.querySelector('#editor iframe.pi-frame')?.dataset.state === 'connected'`, 'inspector connected', 10000);
    // Data source round trip (sendToPlugin → sendToPropertyInspector), then setSettings.
    await until(async () => (await d.action('Keypad', '2,1'))?.Settings?.pick === 'b', 'the inspector listed the plugin\'s items and saved the pick', 15000);
    await d.clear('Keypad', '2,1');
    await d.api('/api/plugins/com.e2e.lib3', 'DELETE');
    // Back to the key the next tests start from.
    await selectSlot(b, 'Keypad', '3,1');
    await b.waitFor(`document.querySelector('#editor iframe.pi-frame')?.dataset.state === 'connected'`, 'inspector of 3,1', 10000);
    await sleep(500);
    b.quiet = false;
  });

  await test('editor: an icon from an icon pack (search, pick)', async () => {
    await selectSlot(b, 'Keypad', '3,1');
    await b.clickText('#editor .ed-imgbtns button', 'Icon…');
    await b.waitDialog('Icon from an icon pack');
    await b.waitFor(`document.querySelectorAll('dialog[open] .icon-cell').length > 0`, 'icons');
    const packs = await b.eval(`[...document.querySelectorAll('dialog[open] select option')].map((o) => o.textContent)`);
    assert(['More Icons (1)', 'UI Icons (3)'].every((n) => packs.includes(n)), J(packs));
    await b.set('dialog[open] select', 'com.ui.icons');
    await b.waitFor(`document.querySelectorAll('dialog[open] .icon-cell').length === 3`, 'three icons');
    eq(await b.text('dialog[open] .icon-picker-foot .field-help'), '3 icons');
    await b.set('dialog[open] input[type="search"]', 'sport');
    await b.waitFor(`document.querySelectorAll('dialog[open] .icon-cell').length === 1`, 'search by tag');
    // Images of keys are stored as files next to the page (Images/<hash>.svg).
    const before = (await d.action('Keypad', '3,1'))?.States?.[0]?.Image;
    await b.click('dialog[open] .icon-cell');
    await b.waitNoDialog();
    const img = await until(async () => { const x = (await d.action('Keypad', '3,1'))?.States?.[0]?.Image; return x && x !== before && x; }, 'icon saved');
    eq(fs.readFileSync(d.file(path.basename(img)), 'utf8'), SVG('#0000ff'), 'the bike icon');
  });

  await test('icon packs: the manage dialog removes a pack', async () => {
    await b.profileMenu('Manage icon packs…');
    await b.waitDialog('Icon packs');
    const n = (await d.api('/api/iconpacks')).length;
    const names = await b.eval(`[...document.querySelectorAll('dialog[open] .iconpack-list b')].map((x) => x.textContent)`);
    eq(names.length, n);
    assert(['More Icons', 'UI Icons'].every((x) => names.includes(x)), J(names));
    await b.eval(`document.querySelector('dialog[open] .iconpack-list button[value="del:com.ui.more"]').click()`);
    await b.waitDialog('Remove icon pack');
    await b.dialogButton('Remove');
    await until(async () => !(await d.api('/api/iconpacks')).some((p) => p.id === 'com.ui.more'), 'removed');
    await b.waitFor(`document.querySelectorAll('dialog[open] .iconpack-list li').length === ${n - 1}`, 'list shown again');
    await b.dialogButton('Close');
    await b.waitNoDialog();
  });

  await test('plugins view: a plugin is removed from the Installed tab', async () => {
    await b.click('#market-btn');
    await b.click('#market .mk-tab[data-tab="installed"]');
    const packs = (await d.api('/api/iconpacks')).length;
    await b.waitFor(`document.querySelectorAll('#market .mk-plugin').length === ${packs + 1}`, 'plugin and packs');
    await b.eval(`[...document.querySelectorAll('#market .mk-plugin')].find((r) => r.textContent.includes('UI Counter')).querySelector('.danger-outline').click()`);
    await b.waitDialog('Remove plugin');
    await b.dialogButton('Remove');
    await until(async () => !(await d.api('/api/plugins')).length, 'uninstalled');
    await b.toast('"UI Counter" removed');
    await b.waitFor(`document.querySelectorAll('#market .mk-plugin').length === ${packs}`, 'only the packs left');
    await b.click('#market .mk-tab[data-tab="catalogue"]');
    await b.eval(`document.querySelector('#market .mk-search').focus()`);
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`document.getElementById('market').hidden`, 'closed');
    await b.waitFor(`!document.querySelector('.action-item[data-uuid="com.ui.counter.count"]')`, 'its action is gone from the list');
    await b.waitFor(`(async () => { const { S } = await import('/store.js'); return S.selected?.coord === '3,1'; })()`, 'still selected');
    await b.waitFor(`!!document.querySelector('#editor .badge.warn')`, 'editor marks the missing plugin');
    eq(await b.text('#editor .badge.warn'), 'Plugin not installed');
  });
}

async function suitePlus(d, b) {
  await test('without the token the UI explains how to open it', async () => {
    b.quiet = true;
    b.origin = d.url('').replace(/\/$/, '');
    await b.send('Page.navigate', { url: d.url('/') });
    await b.waitFor(`!!document.querySelector('.no-access')`, 'no-access notice');
    // With the token (as `tuxdeck ui` opens it) the UI works.
    await b.open(await d.loginUrl());
    b.quiet = false;
  });

  await test('plus: keys and dials render', async () => {
    assert(await b.count(`#device .slot[data-controller="Keypad"]`) === 8, '8 keys');
    assert(await b.count(`#device .slot[data-controller="Encoder"]`) === 4, '4 dials');
    assert(!(await b.exists('#device .neo-row')), 'no info bar');
    await b.click(slotSel('Encoder', '0,0'));
    await b.waitFor(`!!document.querySelector('#editor .ed-header')`, 'dial editor');
  });

  await test('plus: the touch strip above each dial, arrow keys reach the dials', async () => {
    eq(await b.count('#device .encoder-row .slot .strip-seg img'), 4, 'strip segments');
    assert((await b.eval(`document.querySelector('${slotSel('Encoder', '2,0')} .strip-seg img').src`)).includes('/Encoder/2%2C0.png'), 'strip preview of dial 3');
    await b.click(slotSel('Keypad', '2,1'));
    await b.key('ArrowDown', { keyCode: KEYS.ArrowDown });
    eq(await b.eval(`document.activeElement.dataset.controller + ' ' + document.activeElement.dataset.coord`), 'Encoder 2,0');
    await b.key('ArrowUp', { keyCode: KEYS.ArrowUp });
    eq(await b.eval(`document.activeElement.dataset.coord`), '2,1');
  });

  await test('plus: dial actions; keys-only actions are refused on a dial', async () => {
    // Info bar actions are not offered on a Plus; dial actions are.
    assert(!(await b.exists('.action-item[data-uuid="com.elgato.streamdeck.system.digitaltime"]')), 'no info bar actions');
    await selectSlot(b, 'Encoder', '3,0');
    await placeOnSelected(b, 'com.elgato.streamdeck.system.website');
    await b.toast('This action does not fit the selected spot.');
    await placeOnSelected(b, 'com.elgato.streamdeck.system.hotkey');
    await until(async () => (await d.action('Encoder', '3,0'))?.UUID === 'com.elgato.streamdeck.system.hotkey', 'hotkey on the dial');
    await b.waitFor(`document.querySelector('#editor .ed-name')?.textContent === 'Hotkey'`, 'dial editor');
    assert((await b.text('#editor .ed-sub')).includes('Dial 4'), 'labelled as dial 4');
    assert(!(await b.exists('#editor .ed-bg')), 'no key background for dials');
    // Dragging a key-only action marks the dials as refused.
    await b.eval(`document.querySelector('.action-item[data-uuid="com.elgato.streamdeck.system.website"]').dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: new DataTransfer() }))`);
    eq(await b.count('#device .slot[data-controller="Encoder"].drop-denied'), 4, 'dials refuse it');
    eq(await b.count('#device .slot[data-controller="Keypad"].drop-candidate'), 8, 'keys accept it');
    await b.eval(`document.dispatchEvent(new DragEvent('dragend', { bubbles: true }))`);
  });

  await test('plus: no touch colour setting without touch points', async () => {
    await b.click('#settings-btn');
    await b.waitFor(`document.getElementById('settings-dialog').open`, 'settings');
    assert(await b.eval(`document.getElementById('set-touch-row').hidden`), 'hidden');
    await b.key('Escape', { keyCode: KEYS.Escape });
    await b.waitFor(`!document.getElementById('settings-dialog').open`, 'closed');
  });
}

/** Decks without info bar or dials: geometry, arrow keys, catalogue. */
function suiteGrid(cols, rows) {
  return async (d, b) => {
    await b.open(await d.loginUrl());
    const model = d.model;
    await test(`${model}: ${cols}×${rows} keys, no info bar, no dials`, async () => {
      eq(await b.count('#device .keygrid > .slot'), cols * rows);
      eq(await b.count('#device .neo-row, #device .encoder-row'), 0);
      eq(await b.eval(`getComputedStyle(document.getElementById('device')).getPropertyValue('--cols').trim()`), String(cols));
      // All keys fit into the window and are laid out in rows of `cols`.
      const tops = await b.eval(`[...document.querySelectorAll('#device .keygrid > .slot')].map((s) => Math.round(s.getBoundingClientRect().top))`);
      eq(new Set(tops).size, rows, 'rows');
      assert(await b.eval(`(() => { const r = document.querySelector('#device').getBoundingClientRect(); return r.right <= innerWidth && r.bottom <= innerHeight; })()`), 'fits the window');
    });
    await test(`${model}: arrow keys stop at the edges`, async () => {
      await b.click(slotSel('Keypad', `${cols - 1},0`));
      await b.key('ArrowRight', { keyCode: KEYS.ArrowRight });
      eq(await b.eval(`document.activeElement.dataset.coord`), `${cols - 1},0`);
      await b.key('ArrowDown', { keyCode: KEYS.ArrowDown });
      eq(await b.eval(`document.activeElement.dataset.coord`), `${cols - 1},1`);
      await b.key('Home', { keyCode: KEYS.Home });
      eq(await b.eval(`document.activeElement.dataset.coord`), '0,1');
    });
    await test(`${model}: no info bar actions in the list; an action placed on the last key`, async () => {
      assert(!(await b.exists('.action-item[data-uuid="com.elgato.streamdeck.system.pagination"]')), 'no info bar actions');
      const last = `${cols - 1},${rows - 1}`;
      await placeAndEdit(b, d, 'Keypad', last, 'com.elgato.streamdeck.system.text');
      assert((await b.text('#editor .ed-sub')).includes(`Key ${rows}/${cols}`), 'editor label');
    });
  };
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
tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'sd-ui-'));
shotDir = opt('--screenshots', path.join(tmp, 'screenshots'));
fs.mkdirSync(shotDir, { recursive: true });
await CATALOGUE.start();
// Linux check results of this computer and published ones (badges in the plugins view).
const seedChecks = (data) => {
  fs.writeFileSync(path.join(data, 'linux-checks.json'), J({
    'com.ui.cat01': report('com.ui.cat01', 'works'),
    'com.ui.cat02': report('com.ui.cat02', 'issues', ['inspector']),
    'com.ui.cat04': report('com.ui.cat04', 'fails', ['starts', 'actions']),
    'com.ui.cat05': report('com.ui.cat05', 'unsupported', ['linux_runtime']),
  }));
  fs.writeFileSync(path.join(data, 'linux-checks-remote.json'), J({ plugins: { 'com.ui.cat06': report('com.ui.cat06', 'works') } }));
};
const SUITES = [
  { name: 'neo', model: 'neo', run: suiteNeo },
  { name: 'profiles', model: 'neo', run: suiteProfiles },
  { name: 'market', model: 'neo', run: suiteMarket, prep: seedChecks },
  { name: 'plus', model: 'plus', run: suitePlus },
  { name: 'mini', model: 'mini', run: suiteGrid(3, 2) },
  { name: 'xl', model: 'xl', run: suiteGrid(8, 4) },
];
for (const { name, model, run, prep } of SUITES.filter((s) => !ONLY.length || ONLY.includes(s.name))) {
  console.log(`\n# ${name}`);
  const d = new Daemon(model, path.join(tmp, name), { prep });
  const b = new Browser(browserBin, path.join(tmp, `${name}-browser`));
  try {
    await d.start();
    await b.start();
    current = b;
    await run(d, b);
    await test(`${name}: no JS errors in the UI`, async () => {
      if (b.errors.length) throw new Error(b.errors.join('\n'));
    });
  } catch (e) {
    failures++;
    console.log(`  FAIL  ${name}: ${e.stack || e}`);
  } finally {
    current = null;
    await b.stop();
    await d.stop();
  }
}
CATALOGUE.server.close();
if (!KEEP && !failures) fs.rmSync(tmp, { recursive: true, force: true });
console.log(`\n${passes} passed, ${failures} failed`);
process.exit(failures ? 1 : 0);
