// Linux check of a Stream Deck plugin (embedded in the tuxdeck binary, see
// linuxcheck.rs). The plugin runs in an isolated TuxDeck instance: simulated
// decks, dry run (built-in actions don't act), its own HOME and data, so neither the
// user's decks nor files are touched. Prints one JSON report on stdout.
//
//   node linux-check.mjs --bin <streamdeck> (--plugin <file.streamDeckPlugin> | --plugin-dir <dir>)
//                        [--browser <chromium>] [--elgato <resources dir>]
//
// Node.js >= 22, no dependencies.

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const opt = (n) => (args.includes(n) ? args[args.indexOf(n) + 1] : undefined);
const BIN = opt('--bin');
const debug = (...m) => { if (args.includes('--debug')) process.stderr.write(`[linux-check] ${m.join(' ')}\n`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const started = Date.now();
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'sd-linux-check-'));
const report = { checks: [], tested_at: new Date().toISOString() };
const daemons = [];

function check(id, ok, detail = '') {
  // ok: true = passed, false = failed, null = not tested
  const prev = report.checks.find((c) => c.id === id);
  if (prev) {
    if (prev.ok !== false) Object.assign(prev, { ok, detail: detail || prev.detail });
    else if (detail && ok === false) prev.detail += `; ${detail}`;
  } else report.checks.push({ id, ok, detail });
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

function copyDir(from, to) {
  fs.mkdirSync(to, { recursive: true });
  for (const e of fs.readdirSync(from, { withFileTypes: true })) {
    const a = path.join(from, e.name);
    const b = path.join(to, e.name);
    if (e.isDirectory()) copyDir(a, b);
    else if (e.isFile()) fs.copyFileSync(a, b);
  }
}

// ------------------------------------------------------------------------------- daemon

let instances = 0;

class Daemon {
  constructor(model) {
    this.model = model;
    this.dir = path.join(tmp, `${model}-${++instances}`);
    this.data = path.join(this.dir, 'data');
    this.lines = [];
  }

  async start(pluginDir) {
    const config = path.join(this.dir, 'config');
    const home = path.join(this.dir, 'home');
    for (const d of [this.data, config, home]) fs.mkdirSync(d, { recursive: true });
    if (opt('--elgato')) fs.symlinkSync(opt('--elgato'), path.join(this.data, 'elgato'));
    if (pluginDir) copyDir(pluginDir, path.join(this.data, 'Plugins', path.basename(pluginDir)));
    this.port = await freePort();
    this.sdk = await freePort();
    fs.writeFileSync(path.join(config, 'settings.json'), JSON.stringify({ ui_port: this.port, sdk_port: this.sdk, checks_repo: '' }));
    const env = {
      ...process.env,
      TUXDECK_VIRTUAL_DEVICE: this.model, TUXDECK_DRY_RUN: '1', TUXDECK_NO_URL_HANDLER: '1', TUXDECK_NO_WINDOW_WATCH: '1',
      TUXDECK_DATA_DIR: this.data, TUXDECK_CONFIG_DIR: config, HOME: home, RUST_LOG: 'info', NO_COLOR: '1',
      LANGUAGE: '', LC_ALL: '', LC_MESSAGES: '', LANG: 'C.UTF-8',
    };
    delete env.HYPRLAND_INSTANCE_SIGNATURE;
    this.proc = spawn(BIN, ['run', '--no-browser'], { env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
    const onData = (b) => this.lines.push(...b.toString().split('\n').filter((l) => l.trim()));
    this.proc.stdout.on('data', onData);
    this.proc.stderr.on('data', onData);
    daemons.push(this);
    for (let i = 0; i < 150; i++) {
      try {
        this.token ??= fs.readFileSync(path.join(config, 'api-token'), 'utf8').trim();
        const st = await this.api('/api/state');
        if (st.devices.length && st.devices[0].profile) {
          this.dev = st.devices[0];
          return;
        }
      } catch { /* not up yet */ }
      await sleep(100);
    }
    throw new Error('the test instance did not start');
  }

  async stop() {
    if (this.proc && this.proc.exitCode === null) {
      try { process.kill(-this.proc.pid, 'SIGTERM'); } catch { /* gone */ }
      for (let i = 0; i < 50 && this.proc.exitCode === null; i++) await sleep(100);
      try { process.kill(-this.proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
  }

  url(p) { return `http://127.0.0.1:${this.port}${p}`; }

  async api(p, method = 'GET', body, headers = {}) {
    const init = { method, headers: { 'x-tuxdeck-token': this.token, ...headers } };
    if (body !== undefined) {
      if (Buffer.isBuffer(body)) init.body = body;
      else { init.body = JSON.stringify(body); init.headers['content-type'] = 'application/json'; }
    }
    const r = await fetch(this.url(p), init);
    const out = r.headers.get('content-type')?.includes('json') ? await r.json() : null;
    if (!r.ok) throw new Error(out?.error || `${method} ${p}: HTTP ${r.status}`);
    return out;
  }

  diag(uuid) { return this.api(`/api/plugins/${encodeURIComponent(uuid)}/diagnostics`); }

  pluginLog(uuid) {
    try { return fs.readFileSync(path.join(this.data, 'logs', `${uuid}.log`), 'utf8'); } catch { return ''; }
  }
}

// ------------------------------------------------------------------------------ browser

function findBrowser() {
  for (const c of [opt('--browser'), 'google-chrome', 'chromium', 'chromium-browser', 'brave'].filter(Boolean)) {
    try {
      const p = execFileSync('sh', ['-c', `command -v "${c}"`]).toString().trim();
      if (p) return p;
    } catch { /* next */ }
  }
  return null;
}

class Browser {
  constructor(bin) {
    this.bin = bin;
    this.dir = path.join(tmp, 'browser');
    this.id = 0;
    this.pending = new Map();
    this.errors = [];
  }

  async start() {
    // The test browser only talks to the local test instance (no proxy detection, which
    // fails without a desktop session, e.g. in the sandbox).
    this.proc = spawn(this.bin, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${this.dir}`, '--no-first-run', '--no-proxy-server',
      '--no-default-browser-check', '--disable-gpu', '--no-sandbox', 'about:blank'], { detached: true, stdio: 'ignore' });
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
    this.ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m.result);
        this.pending.delete(m.id);
      } else if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params.exceptionDetails;
        const url = d.url || d.stackTrace?.callFrames?.[0]?.url || '';
        // Only the plugin's own pages count here.
        if (/\/(plugins|plugin-files)\//.test(url)) this.errors.push(`${d.exception?.description?.split('\n')[0] || d.text} (${url.split('/').pop()})`);
      }
    };
    await this.send('Runtime.enable');
    await this.send('Page.enable');
  }

  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res) => this.pending.set(id, res));
  }

  async eval(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
    return r?.result?.value;
  }

  async stop() {
    try { this.ws?.close(); } catch { /* closed */ }
    if (this.proc && this.proc.exitCode === null) {
      try { process.kill(-this.proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
  }
}

// --------------------------------------------------------------------------------- check

async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await sleep(200);
  }
  return null;
}

/** Install the plugin into a fresh instance and exercise the given actions. */
async function exercise(model, actions, browser) {
  const d = new Daemon(model);
  await d.start(opt('--plugin-dir'));
  if (opt('--plugin')) {
    await d.api('/api/import', 'POST', fs.readFileSync(opt('--plugin')), { 'x-filename': path.basename(opt('--plugin')) });
  }
  const plugins = await d.api('/api/plugins');
  const uuid = report.uuid;
  const p = plugins.find((x) => x.uuid === uuid);
  if (!p) throw new Error('the plugin was not installed');
  if (p.runs_on_linux === false) {
    check('linux_runtime', false, p.reason || 'macOS/Windows only');
    return d;
  }
  check('linux_runtime', true);

  const registered = await waitFor(async () => (await d.diag(uuid)).stats?.registrations > 0, 25000);
  if (!registered) {
    const log = d.pluginLog(uuid).trim().split('\n').slice(-4).join(' | ');
    check('starts', false, log ? `does not connect; log: ${log}` : 'does not connect to TuxDeck');
    debug('daemon log:', d.lines.slice(-25).join('\n'));
    return d;
  }
  check('starts', true);

  // Place every action where it belongs (keys, dials, Neo info bar).
  const st = await d.api('/api/state');
  const profile = st.devices[0].profile;
  const page = st.devices[0].page;
  const placed = [];
  let key = 0;
  let dial = 0;
  for (const a of actions) {
    const c = a.controllers || ['Keypad'];
    let slot;
    if (model === 'plus' && c.includes('Encoder') && dial < 4) slot = { controller: 'Encoder', coord: `${dial++},0` };
    else if (model === 'neo' && c.includes('Neo') && !c.includes('Keypad')) slot = { controller: 'Neo', coord: '1,0' };
    else if (c.includes('Keypad') && key < 8) slot = { controller: 'Keypad', coord: `${key % 4},${Math.floor(key++ / 4)}` };
    if (!slot) continue;
    try {
      await d.api(`/api/pages/${profile}/${page}/slot`, 'POST', { ...slot, uuid: a.uuid });
      placed.push({ ...slot, action: a });
    } catch (e) {
      check('actions', false, `${a.name}: ${e.message}`);
    }
  }
  await sleep(2500);
  let diag = await d.diag(uuid);
  const alive = (dg) => dg.running !== false && dg.restarts === 0 && dg.connected;
  const shownActions = new Set(diag.shown.map((s) => s.action));
  const missing = placed.filter((s) => !shownActions.has(s.action.uuid)).map((s) => s.action.name);
  if (!alive(diag)) check('actions', false, 'the plugin stopped after its actions appeared');
  else if (missing.length) check('actions', false, `not shown: ${missing.join(', ')}`);
  else check('actions', true, `${placed.length} placed`);

  // Use them like a user would.
  for (const s of placed) {
    if (s.controller === 'Encoder') {
      const idx = Number(s.coord.split(',')[0]);
      await d.api(`/api/devices/${d.dev.id}/input`, 'POST', { type: 'dial', index: idx, ticks: 2 }).catch(() => {});
      await d.api(`/api/devices/${d.dev.id}/input`, 'POST', { type: 'dial_press', index: idx, down: true }).catch(() => {});
      await d.api(`/api/devices/${d.dev.id}/input`, 'POST', { type: 'dial_press', index: idx, down: false }).catch(() => {});
    } else if (s.controller === 'Keypad') {
      await d.api(`/api/pages/${profile}/${page}/press`, 'POST', { controller: s.controller, coord: s.coord, hold_ms: 100 }).catch(() => {});
    }
    await sleep(300);
  }
  await sleep(2000);
  diag = await d.diag(uuid);
  if (alive(diag)) check('input', true);
  else check('input', false, 'the plugin stopped while its actions were used');

  // Reacting at all (title, image, settings ...) to its actions.
  const contexts = diag.stats?.contexts || {};
  const silent = diag.shown.filter((s) => !contexts[s.context]).map((s) => actions.find((a) => a.uuid === s.action)?.name || s.action);
  check('responds', silent.length === 0 ? true : null, silent.length ? `no reaction from: ${[...new Set(silent)].join(', ')}` : '');

  // Property inspectors in a real browser, like in the configuration UI.
  const withPi = placed.filter((s) => s.action.pi);
  if (!withPi.length) {
    check('inspector', true, 'no settings');
  } else if (!browser) {
    check('inspector', null, 'no Chromium-based browser to test with');
  } else {
    await browser.send('Page.navigate', { url: d.url(`/?code=${(await d.api('/api/login-code', 'POST')).code}`) });
    const loaded = await waitFor(() => browser.eval(`document.querySelectorAll('#device .slot').length > 0`), 15000);
    debug('UI loaded:', !!loaded, await browser.eval('location.href + " " + document.title + " " + (document.body?.innerText || "").slice(0, 200).replace(/\\s+/g, " ")'));
    const failed = [];
    for (const s of withPi) {
      browser.errors.length = 0;
      await browser.eval(`document.querySelector('#device .slot[data-controller="${s.controller}"][data-coord="${s.coord}"]')?.click()`);
      const ctx = diag.shown.find((x) => x.action === s.action.uuid && x.coordinates?.column === Number(s.coord.split(',')[0]))?.context;
      const connected = await waitFor(async () => {
        const dg = await d.diag(uuid);
        return ctx ? dg.stats?.inspectors?.[ctx] > 0 : Object.keys(dg.stats?.inspectors || {}).length > 0;
      }, 12000);
      await sleep(1000);
      debug('PI', s.action.name, 'connected:', !!connected, 'frames:', await browser.eval(`[...document.querySelectorAll('iframe')].map((f) => f.src).join(' ')`));
      if (!connected) failed.push(`${s.action.name}: settings did not connect`);
      if (browser.errors.length) failed.push(`${s.action.name}: ${browser.errors.slice(0, 2).join('; ')}`);
    }
    check('inspector', failed.length ? false : true, failed.join(' | '));
  }

  // Errors the plugin wrote to its log. The sandbox has no internet: failed lookups and
  // connections are expected there and say nothing about Linux support.
  const offline = /\b(EAI_AGAIN|ENOTFOUND|ENETUNREACH|ECONNREFUSED|EHOSTUNREACH|getaddrinfo|fetch failed)\b/;
  const errors = d.pluginLog(uuid).split('\n').filter((l) => /\b(Error|TypeError|ReferenceError|SyntaxError|Unhandled|ENOENT|EACCES|panic)\b/.test(l) && !offline.test(l));
  check('log', errors.length === 0, errors.slice(0, 3).map((l) => l.trim().slice(0, 200)).join(' | '));
  return d;
}

async function main() {
  if (!BIN || !(opt('--plugin') || opt('--plugin-dir'))) throw new Error('usage: --bin <tuxdeck> --plugin <file> | --plugin-dir <dir>');
  // Find out what the plugin offers (first instance installs it).
  const probe = new Daemon('neo');
  await probe.start(opt('--plugin-dir'));
  if (opt('--plugin')) {
    const r = await probe.api('/api/import', 'POST', fs.readFileSync(opt('--plugin')), { 'x-filename': path.basename(opt('--plugin')) });
    report.uuid = r.id;
  } else {
    const m = JSON.parse(fs.readFileSync(path.join(opt('--plugin-dir'), 'manifest.json'), 'utf8'));
    report.uuid = m.UUID || path.basename(opt('--plugin-dir')).replace(/\.sdPlugin$/, '');
  }
  const p = (await probe.api('/api/plugins')).find((x) => x.uuid === report.uuid);
  Object.assign(report, { name: p?.name, version: p?.version });
  const actions = (await probe.api('/api/actions')).flatMap((c) => c.actions).filter((a) => a.plugin === report.uuid);
  await probe.stop();

  const browserBin = findBrowser();
  const browser = browserBin ? new Browser(browserBin) : null;
  if (browser) await browser.start().catch((e) => { debug('browser failed:', e.message); report.browser_error = true; });
  debug('browser:', browserBin, report.browser_error ? 'failed' : 'ok');
  try {
    const encoders = actions.filter((a) => (a.controllers || []).includes('Encoder'));
    const neoOnly = actions.filter((a) => (a.controllers || []).includes('Neo'));
    // Keys and dials on a Stream Deck +, the info bar on a Neo.
    const first = encoders.length ? 'plus' : 'neo';
    const d = await exercise(first, actions, report.browser_error ? null : browser);
    await d.stop();
    if (first === 'plus' && neoOnly.length && report.checks.every((c) => c.ok !== false)) {
      const d2 = await exercise('neo', neoOnly, report.browser_error ? null : browser);
      await d2.stop();
    }
  } finally {
    await browser?.stop();
  }
  const failed = (id) => report.checks.some((c) => c.id === id && c.ok === false);
  report.verdict = failed('linux_runtime') ? 'unsupported'
    : failed('starts') || failed('actions') || failed('input') ? 'fails'
      : failed('inspector') || failed('log') ? 'issues' : 'works';
}

try {
  await main();
} catch (e) {
  report.verdict = 'error';
  report.error = String(e.message || e);
} finally {
  for (const d of daemons) await d.stop();
  fs.rmSync(tmp, { recursive: true, force: true });
  report.duration_ms = Date.now() - started;
  process.stdout.write(JSON.stringify(report) + '\n');
}
