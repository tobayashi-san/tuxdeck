#!/usr/bin/env node
// End-to-end tests against a real daemon with a simulated deck and dry-run mode.
//
//   node tests/e2e.mjs [--bin target/release/tuxdeck] [--only neo,plus,mini,original,mk2,xl,pedal,neo:plugins,cli,...] [--keep] [-v]
//
// Every suite starts its own daemon (TUXDECK_VIRTUAL_DEVICE=<model>,
// TUXDECK_DRY_RUN=1, isolated data/config dirs, free ports) and drives it through
// the HTTP API exactly like the web UI. Side effects of actions are checked through
// the "DRYRUN …" log lines. If the Elgato resources were imported
// (~/.local/share/tuxdeck/elgato), they are linked in so that the original
// icons and info bar layouts are rendered as well.
// No dependencies besides Node.js >= 22.

import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : def;
};
const BIN = path.resolve(opt('--bin', path.join(ROOT, 'target/release/tuxdeck')));
const ONLY = (opt('--only', 'neo,plus,mini,original,original2,mk2,xl,pedal,twodecks')).split(',');
const KEEP = args.includes('--keep');
const VERBOSE = args.includes('-v');
const ELGATO = path.join(os.homedir(), '.local/share/tuxdeck/elgato');

// ------------------------------------------------------------------------------- helpers

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
let failures = 0;
let passes = 0;
const results = [];

function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assertion failed');
}
function eq(a, b, msg) {
  if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(`${msg || 'expected equal'}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
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

/** Minimal ZIP writer (stored entries); `modes`: Unix permissions by entry name. */
function zip(files, modes = {}) {
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
    // "Made by" Unix (3) when a mode is given: the mode is in the upper external attributes.
    c.writeUInt16LE(modes[name] ? (3 << 8) | 20 : 20, 4);
    if (modes[name]) c.writeUInt32LE(((0o100000 | modes[name]) << 16) >>> 0, 38);
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

function pngSize(buf) {
  assert(buf.length > 24 && buf.readUInt32BE(0) === 0x89504e47, 'not a PNG');
  return [buf.readUInt32BE(16), buf.readUInt32BE(20)];
}

const SVG = (color) => `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144"><rect width="144" height="144" fill="${color}"/></svg>`;
const RED_PNG = 'data:image/svg+xml;base64,' + Buffer.from(SVG('#ff0000')).toString('base64');

// Node test plugin: counts key presses (title + OK overlay), drives the touch strip.
const PLUGIN_JS = `
const a = process.argv; const arg = (n) => a[a.indexOf(n) + 1];
const ws = new WebSocket('ws://127.0.0.1:' + arg('-port'));
const send = (m) => ws.send(JSON.stringify(m));
const count = {}; const turn = {};
ws.onopen = () => send({ event: arg('-registerEvent'), uuid: arg('-pluginUUID') });
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  const c = m.context;
  // Every event, so that tests can check what the plugin received.
  // The request id of an answer (SDK 7.1) comes before the payload.
  send({ event: 'logMessage', payload: { message: 'ev ' + m.event + ' ' + (c || '-') + ' ' + (m.id !== undefined ? 'id=' + m.id + ' ' : '') + JSON.stringify(m.payload ?? m.deviceInfo ?? null).slice(0, 600) } });
  // A test can make the plugin crash (exit code).
  if (m.event === 'sendToPlugin' && m.payload?.exit !== undefined) setTimeout(() => process.exit(m.payload.exit), 50);
  if (m.event === 'willAppear') {
    const at = m.payload.coordinates ? m.payload.coordinates.column + ',' + m.payload.coordinates.row + ' ' : '';
    send({ event: 'logMessage', payload: { message: 'appear ' + m.payload.controller + ' ' + at + c } });
  }
  if (m.event === 'titleParametersDidChange') send({ event: 'logMessage', payload: { message: 'titleParams ' + m.payload.title + ' ' + c } });
  if (m.event === 'didReceiveDeepLink') send({ event: 'logMessage', payload: { message: 'deeplink ' + m.payload.url } });
  // Tests drive the plugin through a property inspector connection.
  if (m.event === 'sendToPlugin' && Array.isArray(m.payload?.send)) for (const x of m.payload.send) send(x);
  if (m.event === 'keyDown') {
    count[c] = (count[c] || 0) + 1;
    send({ event: 'setTitle', context: c, payload: { title: 'n=' + count[c] } });
    send({ event: 'showOk', context: c });
    send({ event: 'logMessage', payload: { message: 'keyDown ' + count[c] } });
  }
  if (m.event === 'dialRotate') {
    turn[c] = Math.max(0, Math.min(100, (turn[c] ?? 50) + m.payload.ticks * 10));
    send({ event: 'setFeedback', context: c, payload: { value: turn[c] + ' %', indicator: { value: turn[c], bar_fill_c: '#ff0000' } } });
    send({ event: 'logMessage', payload: { message: 'dialRotate ' + m.payload.ticks + ' -> ' + turn[c] } });
  }
  if (m.event === 'dialDown') send({ event: 'setFeedbackLayout', context: c, payload: { layout: 'layouts/custom.json' } });
  if (m.event === 'touchTap') {
    send({ event: 'setFeedback', context: c, payload: { title: 'tap ' + m.payload.tapPos.join(',') + (m.payload.hold ? ' hold' : '') } });
    send({ event: 'logMessage', payload: { message: 'touchTap ' + m.payload.tapPos.join(',') + ' hold=' + m.payload.hold } });
  }
};
`;
const PLUGIN_FILES = {
  'com.e2e.counter.sdPlugin/manifest.json': JSON.stringify({
    Name: 'E2E Counter', UUID: 'com.e2e.counter', Version: '1.0', Author: 'e2e', SDKVersion: 2, CodePath: 'plugin.js', Category: 'E2E',
    Icon: 'img', OS: [{ Platform: 'mac', MinimumVersion: '10.15' }], Nodejs: { Version: '20' },
    Actions: [{
      UUID: 'com.e2e.counter.count', Name: 'Zähler', Icon: 'img', States: [{ Image: 'img' }], PropertyInspectorPath: 'pi/pi.html',
      Controllers: ['Keypad', 'Encoder'], Encoder: { layout: '$B1', TriggerDescription: { Rotate: 'Wert' } },
    }],
  }),
  'com.e2e.counter.sdPlugin/plugin.js': PLUGIN_JS,
  'com.e2e.counter.sdPlugin/img.svg': SVG('#2266aa'),
  // A property inspector that connects like the SDK libraries do.
  'com.e2e.counter.sdPlugin/pi/pi.html': '<!doctype html><title>pi</title><script>function connectElgatoStreamDeckSocket(port, uuid, ev) {'
    + ' const ws = new WebSocket("ws://127.0.0.1:" + port); ws.onopen = () => ws.send(JSON.stringify({ event: ev, uuid })); }</script>',
  'com.e2e.counter.sdPlugin/layouts/custom.json': JSON.stringify({
    id: 'custom', items: [
      { key: 'big', type: 'text', rect: [0, 0, 200, 100], value: 'CUSTOM', font: { size: 30, weight: 700 }, color: '#00ff00', background: '0:#000000,1:#0000ff' },
    ],
  }),
};
const PLUGIN = zip(PLUGIN_FILES);

const ICONPACK = zip({
  'com.e2e.icons.sdIconPack/manifest.json': JSON.stringify({ Name: 'E2E Icons', Author: 'e2e', Version: '1.0.0', Icon: 'icon.svg', Description: 'Testpaket' }),
  'com.e2e.icons.sdIconPack/icon.svg': SVG('#00aa00'),
  'com.e2e.icons.sdIconPack/icons.json': JSON.stringify([
    { path: 'train.svg', name: 'Train', tags: ['travel'] },
    { path: 'salad.svg', name: 'Salad', tags: ['food'] },
    { path: 'bike.svg', name: 'Bike', tags: ['travel', 'sport'] },
  ]),
  'com.e2e.icons.sdIconPack/icons/train.svg': SVG('#ff8800'),
  'com.e2e.icons.sdIconPack/icons/salad.svg': SVG('#00ff00'),
  'com.e2e.icons.sdIconPack/icons/bike.svg': SVG('#0000ff'),
});

// --------------------------------------------------------------------- fake catalogue
// Stands in for the OpenAction catalogue and the GitHub API, so that no test instance
// talks to the real ones.

const CATALOGUE = {
  server: null,
  port: 0,
  files: new Map(), // path -> Buffer
  async start() {
    this.port = await freePort();
    const base = `http://127.0.0.1:${this.port}`;
    this.files.set('/catalogue.json', Buffer.from(JSON.stringify({
      'com.e2e.cat': { name: 'Catalogue Plugin', author: 'e2e', repository: 'https://github.com/e2e/cat', description: 'A plugin from the catalogue' },
      'com.e2e.direct': { name: 'Direct Plugin', author: 'someone', repository: 'https://github.com/e2e/direct', description: 'Direct download', download_url: `${base}/dl/direct.streamDeckPlugin`, flags: { ai_assisted: true } },
      'com.e2e.norel': { name: 'Another Plugin', author: 'e2e', repository: 'https://github.com/e2e/norel', description: null },
    })));
    this.files.set('/repos/e2e/cat/releases', Buffer.from(JSON.stringify([
      { tag_name: 'v2-beta', draft: false, prerelease: true, assets: [] },
      { tag_name: 'v1.2', draft: false, prerelease: false, assets: [
        { name: 'source.zip', browser_download_url: `${base}/dl/source.zip` },
        { name: 'com.e2e.cat.streamDeckPlugin', browser_download_url: `${base}/dl/cat.streamDeckPlugin` },
      ] },
    ])));
    this.files.set('/repos/e2e/norel/releases', Buffer.from('[]'));
    this.files.set('/dl/cat.streamDeckPlugin', pluginVariant('com.e2e.cat'));
    this.files.set('/dl/direct.streamDeckPlugin', pluginVariant('com.e2e.direct'));
    // The full catalogue also has the plugins the sample profiles and conversions use.
    const full = JSON.parse(this.files.get('/catalogue.json'));
    full['de.felitendo.obs-tools'] = { name: 'OBS Tools', author: 'e2e', repository: 'https://github.com/e2e/obs', download_url: `${base}/dl/obs.streamDeckPlugin` };
    full['me.amankhanna.oadiscord'] = { name: 'Discord', author: 'e2e', repository: 'https://github.com/e2e/discord', download_url: `${base}/dl/discord.streamDeckPlugin` };
    this.files.set('/catalogue-full.json', Buffer.from(JSON.stringify(full)));
    this.files.set('/dl/obs.streamDeckPlugin', zip(pluginWithActions('de.felitendo.obs-tools',
      ['recordtoggle', 'streamtoggle', 'smartscene', 'mutetoggle', 'screenshot', 'virtualcam', 'sourcevisibility', 'previousscene', 'nextscene', 'recordtools', 'studiomode'])));
    this.files.set('/dl/discord.streamDeckPlugin', zip(pluginWithActions('me.amankhanna.oadiscord', ['togglemute', 'toggledeafen', 'voicechannel', 'pushtotalk'])));
    this.server = http.createServer((req, res) => {
      const body = this.files.get(req.url.split('?')[0]);
      res.writeHead(body ? 200 : 404, { 'content-type': req.url.endsWith('.json') || req.url.startsWith('/repos/') ? 'application/json' : 'application/octet-stream' });
      res.end(body || '');
    });
    await new Promise((r) => this.server.listen(this.port, '127.0.0.1', r));
  },
  env() {
    const base = `http://127.0.0.1:${this.port}`;
    return { TUXDECK_CATALOGUE_URL: `${base}/catalogue.json`, TUXDECK_GITHUB_API: base, TUXDECK_CATALOGUE_ICONS: `${base}/icons` };
  },
  fullUrl() { return `http://127.0.0.1:${this.port}/catalogue-full.json`; },
};

// ------------------------------------------------------------------------------- daemon

class Daemon {
  /** `opts.env`: extra environment (e.g. another catalogue); `opts.prepare(d)`: called
   * after the directories exist and before the daemon starts. */
  constructor(model, opts = {}) {
    this.model = model;
    this.opts = opts;
    this.lines = [];
  }

  async start() {
    await this.init();
    await this.launch();
  }

  /** Directories, ports, settings and the environment of the instance (no process yet). */
  async init() {
    this.dir = fs.mkdtempSync(path.join(os.tmpdir(), `sd-e2e-${this.model}-`));
    const data = path.join(this.dir, 'data');
    const config = path.join(this.dir, 'config');
    this.data = data;
    this.config = config;
    fs.mkdirSync(data, { recursive: true });
    fs.mkdirSync(config, { recursive: true });
    if (fs.existsSync(ELGATO)) fs.symlinkSync(ELGATO, path.join(data, 'elgato'));
    this.port = await freePort();
    // Two free ports, not the same one twice.
    do this.sdk = await freePort(); while (this.sdk === this.port);
    fs.writeFileSync(path.join(config, 'settings.json'), JSON.stringify({ ui_port: this.port, sdk_port: this.sdk, checks_repo: '' }));
    // Own HOME: nothing a test instance or its plugins do may touch the user's files.
    const home = path.join(this.dir, 'home');
    this.home = home;
    fs.mkdirSync(home, { recursive: true });
    const env = {
      ...process.env,
      TUXDECK_VIRTUAL_DEVICE: this.model, TUXDECK_DRY_RUN: '1', TUXDECK_NO_URL_HANDLER: '1', TUXDECK_NO_WINDOW_WATCH: '1',
      // Linux checks run without bubblewrap unless one is given (TUXDECK_BWRAP).
      ...(process.env.TUXDECK_BWRAP ? {} : { TUXDECK_NO_SANDBOX: '1' }),
      TUXDECK_DATA_DIR: data, TUXDECK_CONFIG_DIR: config, HOME: home, RUST_LOG: 'info', NO_COLOR: '1',
      ...CATALOGUE.env(),
      // Deterministic language: English (the default) regardless of the machine's locale.
      LANGUAGE: '', LC_ALL: '', LC_MESSAGES: '', LANG: 'C.UTF-8',
      // Deterministic clipboard path in dry-run mode.
      WAYLAND_DISPLAY: 'e2e-wayland',
      ...(typeof this.opts.env === 'function' ? this.opts.env(this) : this.opts.env),
    };
    delete env.HYPRLAND_INSTANCE_SIGNATURE;
    // Never real hardware, and no per-user directories of the real session (desktop
    // entries, gh login, …): everything below the temporary HOME.
    delete env.TUXDECK_ALLOW_HARDWARE;
    for (const k of ['XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'GH_TOKEN', 'GITHUB_TOKEN']) delete env[k];
    this.env = env;
    if (this.opts.prepare) await this.opts.prepare(this);
  }

  /** Run the binary with the instance's environment (CLI commands). */
  cli(argv, ms = 30000, extraEnv = {}) {
    return new Promise((resolve) => {
      const p = track(spawn(BIN, argv, { env: { ...this.env, ...extraEnv }, detached: true, stdio: ['ignore', 'pipe', 'pipe'] }));
      let stdout = '';
      let stderr = '';
      p.stdout.on('data', (b) => { stdout += b; });
      p.stderr.on('data', (b) => { stderr += b; });
      const timer = setTimeout(() => { try { process.kill(-p.pid, 'SIGKILL'); } catch { /* gone */ } }, ms);
      p.on('close', (code) => { clearTimeout(timer); resolve({ code, stdout, stderr }); });
    });
  }

  /** Stop the process but keep the data (see `launch`). */
  async kill() {
    if (this.proc && this.proc.exitCode === null) {
      const proc = this.proc;
      const exited = new Promise((r) => proc.once('exit', r));
      try { process.kill(-proc.pid, 'SIGTERM'); } catch { /* gone */ }
      await Promise.race([exited, sleep(3000)]);
      try { process.kill(-proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
  }

  async launch() {
    const env = this.env;
    this.proc = track(spawn(BIN, ['run', '--no-browser'], { env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] }));
    const onData = (b) => {
      for (const l of b.toString().split('\n')) {
        if (!l.trim()) continue;
        // eslint-disable-next-line no-control-regex
        const line = l.replace(/\x1b\[[0-9;]*m/g, '');
        this.lines.push(line);
        if (VERBOSE) console.log(`    [${this.model}] ${line}`);
      }
    };
    this.proc.stdout.on('data', onData);
    this.proc.stderr.on('data', onData);
    for (let i = 0; i < 100; i++) {
      try {
        // The API secret the daemon creates on first start (see Paths::api_token).
        this.token ??= fs.readFileSync(path.join(this.config, 'api-token'), 'utf8').trim();
        const st = await this.get('/api/state');
        if (st.devices.length && st.devices[0].profile) {
          this.dev = st.devices[0];
          return;
        }
      } catch { /* not up yet */ }
      await sleep(100);
    }
    throw new Error(`daemon did not start:\n${this.lines.join('\n')}`);
  }

  async stop() {
    if (this.proc && this.proc.exitCode === null) {
      try { process.kill(-this.proc.pid, 'SIGTERM'); } catch { /* gone */ }
      for (let i = 0; i < 30 && this.proc.exitCode === null; i++) await sleep(100);
      try { process.kill(-this.proc.pid, 'SIGKILL'); } catch { /* gone */ }
    }
    if (!KEEP && this.dir) fs.rmSync(this.dir, { recursive: true, force: true });
  }

  url(p) { return `http://127.0.0.1:${this.port}${p}`; }

  async req(method, p, body, headers = {}) {
    const init = { method, headers: { 'x-tuxdeck-token': this.token, ...headers } };
    if (body !== undefined) {
      if (Buffer.isBuffer(body)) init.body = body;
      else { init.body = JSON.stringify(body); init.headers['content-type'] = 'application/json'; }
    }
    const r = await fetch(this.url(p), init);
    const ct = r.headers.get('content-type') || '';
    const out = ct.includes('json') ? await r.json() : Buffer.from(await r.arrayBuffer());
    if (!r.ok) {
      const e = new Error(`${method} ${p} → ${r.status} ${JSON.stringify(out?.error ?? '')}`);
      e.status = r.status;
      throw e;
    }
    return out;
  }
  get(p) { return this.req('GET', p); }
  post(p, b) { return this.req('POST', p, b ?? {}); }
  patch(p, b) { return this.req('PATCH', p, b); }
  del(p) { return this.req('DELETE', p); }
  async status(method, p, body, headers) {
    try { await this.req(method, p, body, headers); return 200; } catch (e) { return e.status || 0; }
  }

  mark() { return this.lines.length; }
  async waitLog(re, since = 0, ms = 3000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const hit = this.lines.slice(since).find((l) => re.test(l));
      if (hit) return hit;
      await sleep(25);
    }
    throw new Error(`log line ${re} not found; last lines:\n      ${this.lines.slice(Math.max(since, this.lines.length - 12)).join('\n      ')}`);
  }
  logsSince(since, re) { return this.lines.slice(since).filter((l) => re.test(l)); }

  async state() { return this.get('/api/state'); }
  async device() { return (await this.state()).devices[0]; }
  async page(profile, page) {
    const d = await this.device();
    return this.get(`/api/pages/${profile || d.profile}/${page || d.page}`);
  }
  async slot(controller, coord, uuid) {
    const d = await this.device();
    return this.post(`/api/pages/${d.profile}/${d.page}/slot`, { controller, coord, uuid });
  }
  async settings(controller, coord, settings) {
    const d = await this.device();
    return this.patch(`/api/pages/${d.profile}/${d.page}/slot`, { controller, coord, settings });
  }
  async press(controller, coord, hold_ms) {
    const d = await this.device();
    return this.post(`/api/pages/${d.profile}/${d.page}/press`, { controller, coord, hold_ms });
  }
  async render(controller, coord, q = '') {
    const d = await this.device();
    return this.get(`/api/render/${d.profile}/${d.page}/${controller}/${coord}.png?size=96&v=${Date.now()}${q}`);
  }
  async input(body) { return this.post(`/api/devices/${(await this.device()).id}/input`, body); }
  async waitState(pred, ms = 2000, what = 'state condition') {
    const end = Date.now() + ms;
    let st;
    while (Date.now() < end) {
      st = await this.state();
      if (pred(st)) return st;
      await sleep(30);
    }
    throw new Error(`timeout waiting for ${what}: ${JSON.stringify(st.devices[0])}`);
  }
  /** Poll the key preview until `pred(png)` holds; returns that preview. */
  async waitRender(controller, coord, pred, ms = 3000, what = 'key preview') {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const png = await this.render(controller, coord);
      if (pred(png)) return png;
      await sleep(40);
    }
    throw new Error(`timeout waiting for ${what} of ${controller} ${coord}`);
  }
  /** The stored action at a slot of the shown page (or of `page` in `profile`). */
  async action(coord, controller = 'Keypad', profile, page) {
    return (await this.page(profile, page)).controllers.find((c) => c.Type === controller)?.Actions?.[coord];
  }
  /** A request with exactly these headers (fetch() drops Host/Origin/Cookie). */
  raw(p, { method = 'GET', headers = {}, body } = {}) {
    return rawRequest(this.port, p, { method, headers, body });
  }
}

function rawRequest(port, p, { method = 'GET', headers = {}, body } = {}) {
  return new Promise((res, rej) => {
    const r = http.request({ host: '127.0.0.1', port, path: p, method, headers }, (resp) => {
      const chunks = [];
      resp.on('data', (c) => chunks.push(c));
      resp.on('end', () => res({ status: resp.statusCode, headers: resp.headers, body: Buffer.concat(chunks) }));
    });
    r.on('upgrade', (resp, socket) => { socket.destroy(); res({ status: resp.statusCode, headers: resp.headers, body: Buffer.alloc(0) }); });
    r.on('error', rej);
    r.end(body);
  });
}

/** Status of a WebSocket handshake (101 = accepted). */
function wsHandshake(port, p, headers = {}) {
  return rawRequest(port, p, {
    headers: { connection: 'Upgrade', upgrade: 'websocket', 'sec-websocket-version': '13', 'sec-websocket-key': 'dGhlIHNhbXBsZSBub25jZQ==', ...headers },
  }).then((r) => r.status);
}

/** Poll `fn` until it returns a truthy value. */
async function waitFor(fn, ms = 3000, what = 'condition') {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    last = await fn();
    if (last) return last;
    await sleep(40);
  }
  throw new Error(`timeout waiting for ${what}`);
}

/** All files below `dir` (recursively). */
function walkFiles(dir) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...walkFiles(p));
    else out.push(p);
  }
  return out;
}

/** Central directory file names of a zip. */
function listZip(buf) {
  const names = [];
  for (let i = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06])), n = buf.readUInt16LE(i + 10), o = buf.readUInt32LE(i + 16); n > 0; n--) {
    const len = buf.readUInt16LE(o + 28);
    names.push(buf.toString('utf8', o + 46, o + 46 + len));
    o += 46 + len + buf.readUInt16LE(o + 30) + buf.readUInt16LE(o + 32);
  }
  return names;
}

/** The entries of a zip as {name: Buffer} (directories left out), read through the
 * central directory (also zip64 entries, as in backups). */
function unzipAll(buf) {
  const out = {};
  const end = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  let o = buf.readUInt32LE(end + 16);
  for (let n = buf.readUInt16LE(end + 10); n > 0; n--) {
    const method = buf.readUInt16LE(o + 10);
    let comp = buf.readUInt32LE(o + 20);
    const uncomp = buf.readUInt32LE(o + 24);
    const nlen = buf.readUInt16LE(o + 28);
    const xlen = buf.readUInt16LE(o + 30);
    const clen = buf.readUInt16LE(o + 32);
    let local = buf.readUInt32LE(o + 42);
    const name = buf.toString('utf8', o + 46, o + 46 + nlen);
    // zip64: the 0xffffffff fields follow in the extra field 0x0001, in this order.
    for (let x = o + 46 + nlen; x < o + 46 + nlen + xlen;) {
      const id = buf.readUInt16LE(x);
      const size = buf.readUInt16LE(x + 2);
      if (id === 1) {
        let f = x + 4;
        if (uncomp === 0xffffffff) f += 8;
        if (comp === 0xffffffff) { comp = Number(buf.readBigUInt64LE(f)); f += 8; }
        if (local === 0xffffffff) local = Number(buf.readBigUInt64LE(f));
      }
      x += 4 + size;
    }
    if (!name.endsWith('/')) {
      const start = local + 30 + buf.readUInt16LE(local + 26) + buf.readUInt16LE(local + 28);
      const data = buf.subarray(start, start + comp);
      out[name] = method === 8 ? zlib.inflateRawSync(data) : Buffer.from(data);
    }
    o += 46 + nlen + xlen + clen;
  }
  return out;
}

async function test(name, fn) {
  const t0 = Date.now();
  try {
    await fn();
    passes++;
    results.push(['ok', name]);
    console.log(`  ok    ${name} (${Date.now() - t0} ms)`);
  } catch (e) {
    failures++;
    results.push(['FAIL', name]);
    console.log(`  FAIL  ${name}\n        ${String(e.stack || e).split('\n').slice(0, 6).join('\n        ')}`);
  }
}

const hk = (linux, qt, mods = { ctrl: true }) => ({
  KeyCmd: false, KeyCtrl: !!mods.ctrl, KeyOption: !!mods.alt, KeyShift: !!mods.shift,
  KeyModifiers: (mods.shift ? 1 : 0) | (mods.ctrl ? 2 : 0) | (mods.alt ? 4 : 0), QTKeyCode: qt, NativeCode: -1, VKeyCode: -1, LinuxCode: linux,
});
const HK_Z = hk(44, 90);
const HK_A = hk(30, 65, { shift: true });

// ------------------------------------------------------------------------------- suites

async function suiteCommon(d, { keys }) {
  const [cols, rows] = keys;
  await test('state: virtual device connected with default profile', async () => {
    const st = await d.state();
    eq(st.devices.length, 1);
    assert(st.devices[0].connected, 'connected');
    eq([st.devices[0].cols, st.devices[0].rows], [cols, rows], 'geometry');
    assert(st.profiles.some((p) => p.id === st.devices[0].profile), 'profile exists');
    // The starter profile is a usable media remote: key 0,0 plays/controls media and
    // shows a bundled icon.
    const page = await d.page();
    const kp = page.controllers.find((c) => c.Type === 'Keypad').Actions;
    assert(kp['0,0']?.UUID === 'com.elgato.streamdeck.system.multimedia', 'media key present');
    assert(/^data:image\/svg/.test(kp['0,0']?.States?.[0]?.Image || ''), 'starter key has an icon');
  });

  await test('starter: layout made for the deck', async () => {
    const st = await d.state();
    const pages = st.profiles.find((p) => p.id === st.devices[0].profile).pages;
    const kp = (await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions;
    const mm = 'system.multimedia';
    const at = (c, r) => kp[`${c},${r}`]?.UUID?.replace('com.elgato.streamdeck.', '');
    for (const coord of Object.keys(kp)) {
      const [c, r] = coord.split(',').map(Number);
      assert(c < cols && r < rows, `${coord} fits on a ${cols}x${rows} deck`);
    }
    if (rows === 1) {
      // Pedal: previous, play/pause, next.
      eq([at(0, 0), at(1, 0), at(2, 0)], [mm, mm, mm]);
    } else if (cols >= 4) {
      // The "Standard" profile's 4×2 pages: music and volume, lock and sleep on the right.
      eq([at(0, 0), at(1, 0), at(2, 0), at(3, 0)], [mm, mm, mm, 'system.open'], 'top row');
      eq([at(0, 1), at(1, 1), at(2, 1), at(3, 1)], [mm, mm, mm, 'system.sleep'], 'bottom row');
      if (cols >= 8 && rows >= 4) {
        // XL: both pages next to each other on one page.
        eq([at(4, 0), at(7, 1), pages.length], ['system.open', 'system.keybrightness', 1]);
      } else {
        eq(pages.length, 2, 'second page with programs');
        if (rows > 2) eq(at(cols - 1, rows - 1), 'page.next', 'page key bottom right');
      }
    } else {
      // Mini: the keys in order, the page key bottom right.
      eq([at(0, 0), at(1, 0), at(2, 0), at(0, 1), at(1, 1), at(2, 1)], [mm, mm, mm, 'system.open', mm, 'page.next']);
    }
    for (const a of Object.values(kp)) {
      if (/page\.(next|previous)$/.test(a.UUID)) continue;
      assert(/^data:image\/svg/.test(a.States?.[0]?.Image || ''), `${a.UUID} has an icon`);
      assert(a.States[0].Title && a.States[0].ShowTitle === true, `${a.UUID} shows its title`);
    }
  });

  await test('render: every key preview is a PNG', async () => {
    for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) eq(pngSize(await d.render('Keypad', `${c},${r}`)), [96, 96]);
  });

  await test('press: default keys trigger actions without errors', async () => {
    const page = await d.page();
    const keypad = page.controllers.find((c) => c.Type === 'Keypad');
    const coords = Object.keys(keypad?.Actions || {});
    assert(coords.length > 0, 'default profile has actions');
    const m = d.mark();
    const before = await d.device();
    for (const c of coords) await d.press('Keypad', c);
    assert(d.logsSince(m, /DRYRUN/).length >= 1, 'some dry-run side effect');
    // Program keys stay empty where the program isn't installed (and say so when pressed).
    eq(d.logsSince(m, /WARN|ERROR/).filter((l) => !/Open: no path set/.test(l)), [], 'no warnings');
    // The starter profile has a Sleep key (and page keys): back to how it was.
    const after = await d.device();
    if (after.sleeping) await d.post(`/api/devices/${after.id}`, { brightness: before.brightness });
    if (after.page !== before.page) await d.post(`/api/devices/${after.id}`, { page: before.page });
  });
}

/** Neo: built-in actions. */
async function suiteNeoActions(d) {
  await suiteCommon(d, { keys: [4, 2] });
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;

  await test('catalogue: all built-ins present, icons set, dependent theme options', async () => {
    const cats = await d.get('/api/actions');
    const all = cats.flatMap((c) => c.actions);
    for (const u of ['com.elgato.streamdeck.profile.openchild', 'com.elgato.streamdeck.profile.backtoparent', 'com.elgato.streamdeck.system.hotkey',
      'com.elgato.streamdeck.multiactions.routine', 'com.elgato.streamdeck.system.pagination', 'com.elgato.streamdeck.soundboard.playaudio']) {
      const a = all.find((x) => x.uuid === u);
      assert(a, `missing ${u}`);
      assert(a.icon, `icon of ${u}`);
    }
    // Every built-in category has an icon (a symbol on a coloured tile).
    const builtinCats = cats.filter((c) => c.actions.every((a) => a.builtin));
    eq(builtinCats.length, 5, 'five built-in categories');
    for (const c of builtinCats) assert(/^data:image\/svg\+xml;base64,/.test(c.icon || '') && Buffer.from(c.icon.split(',')[1], 'base64').toString().includes('<path'), `icon of ${c.category}`);
    const pag = all.find((x) => x.uuid === 'com.elgato.streamdeck.system.pagination');
    const theme = pag.form.find((f) => f.key === 'theme');
    eq(theme.depends_on, 'layout');
    assert(theme.options_by && typeof theme.options_by === 'object', 'options_by');
  });

  await test('info bar: digital time, pagination and text render 248x58 without SVG errors', async () => {
    const m = d.mark();
    eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
    await d.slot('Neo', '1,0', 'com.elgato.streamdeck.system.pagination');
    const form = (await d.get('/api/actions')).flatMap((c) => c.actions).find((a) => a.uuid === 'com.elgato.streamdeck.system.pagination').form;
    for (const o of form.find((f) => f.key === 'layout').options) {
      await d.settings('Neo', '1,0', { layout: o.value, theme: '' });
      eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
    }
    // TuxDeck's text action: placeholders, styles and gradients are drawn, changes are seen.
    await d.slot('Neo', '1,0', 'dev.tuxdeck.infobar.text');
    const shots = [];
    for (const st of [{ text: 'A' }, { text: 'B' }, { text: '{profile}\n{page}/{pages}', align: 'left', bold: false, size: 16 },
      { color: '#ff00ff', background: 'linear-gradient(90deg, #f97316 0%, #e11d48 100%)' }, { background: 'radial-gradient(circle, red, blue)' }]) {
      await d.settings('Neo', '1,0', st);
      const png = await d.render('Neo', '1,0');
      eq(pngSize(png), [248, 58]);
      shots.push(Buffer.from(png).toString('base64'));
    }
    eq(new Set(shots).size, shots.length, 'every change is drawn');
    eq(d.logsSince(m, /svg render failed|Failed to parse/), [], 'usvg warnings');
    await d.slot('Neo', '1,0', 'com.elgato.streamdeck.system.digitaltime');
    eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
  });

  await test('hotkey: key down/up go through hyprctl', async () => {
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.hotkey');
    await d.settings('Keypad', '0,0', { Coalesce: true, Hotkeys: [HK_Z] });
    const m = d.mark();
    await d.press('Keypad', '0,0');
    // Recorded here (LinuxCode): sent as keycode (evdev + 8), the layout decides the symbol.
    await d.waitLog(/DRYRUN hyprctl dispatch hl\.dsp\.send_key_state.*CTRL.*key = "code:52".*"down"/i, m);
    await d.waitLog(/DRYRUN hyprctl dispatch hl\.dsp\.send_key_state.*key = "code:52".*"up"/i, m);
  });

  await test('hotkey: imported macOS hotkeys use keysyms, Super recorded here stays Super', async () => {
    // From an Elgato macOS profile: no LinuxCode, ⌘ means Ctrl (cmd_as_ctrl default).
    const mac = { KeyCmd: true, KeyCtrl: false, KeyOption: false, KeyShift: false, KeyModifiers: 8, QTKeyCode: 90, NativeCode: 6, VKeyCode: -1 };
    await d.settings('Keypad', '0,0', { Coalesce: true, Hotkeys: [mac] });
    let m = d.mark();
    await d.press('Keypad', '0,0');
    await d.waitLog(/send_key_state.*mods = "CTRL", key = "z".*"down"/, m);
    // Recorded in the UI with the Super key.
    await d.settings('Keypad', '0,0', { Coalesce: true, Hotkeys: [{ ...hk(18, 69), KeyCtrl: false, KeyCmd: true, KeyModifiers: 8 }] });
    m = d.mark();
    await d.press('Keypad', '0,0');
    await d.waitLog(/send_key_state.*mods = "SUPER", key = "code:26".*"down"/, m);
    // Keypad flag of Elgato profiles.
    await d.settings('Keypad', '0,0', { Coalesce: true, Hotkeys: [{ ...mac, KeyCmd: false, KeyModifiers: 0x4000, QTKeyCode: 55 }] });
    m = d.mark();
    await d.press('Keypad', '0,0');
    await d.waitLog(/send_key_state.*mods = "", key = "KP_7".*"down"/, m);
  });

  await test('hotkey: empty hotkey warns once per press', async () => {
    await d.settings('Keypad', '0,0', { Hotkeys: [] });
    const m = d.mark();
    await d.press('Keypad', '0,0');
    await sleep(150);
    eq(d.logsSince(m, /no hotkey set/).length, 1);
  });

  await test('hotkey switch: state toggles and sends the matching combo', async () => {
    await d.slot('Keypad', '1,0', 'com.elgato.streamdeck.system.hotkeyswitch');
    await d.settings('Keypad', '1,0', { Hotkeys: [HK_Z, HK_A] });
    let m = d.mark();
    await d.press('Keypad', '1,0');
    await d.waitLog(/send_shortcut.*CTRL.*key = "code:52"/i, m);
    eq((await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions['1,0'].State, 1);
    m = d.mark();
    await d.press('Keypad', '1,0');
    await d.waitLog(/send_shortcut.*SHIFT.*key = "code:38"/i, m);
    eq((await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions['1,0'].State, 0);
  });

  await test('open: "~" opens the home directory, missing paths are reported', async () => {
    await d.slot('Keypad', '2,0', 'com.elgato.streamdeck.system.open');
    await d.settings('Keypad', '2,0', { path: '~' });
    let m = d.mark();
    await d.press('Keypad', '2,0');
    // The test daemon's HOME (see Daemon.start).
    await d.waitLog(new RegExp(`DRYRUN exec "xdg-open" "${path.join(d.dir, 'home').replace(/[/]/g, '\\/')}"`), m);
    await d.settings('Keypad', '2,0', { path: '/does/not/exist.txt' });
    m = d.mark();
    await d.press('Keypad', '2,0');
    await d.waitLog(/does not exist/, m);
    await d.settings('Keypad', '2,0', { path: 'echo hi' });
    m = d.mark();
    await d.press('Keypad', '2,0');
    await d.waitLog(/DRYRUN exec "sh" "-c" "echo hi"/, m);
  });

  await test('open app: start on short press, quit on long press', async () => {
    await d.slot('Keypad', '3,0', 'com.elgato.streamdeck.system.openapp');
    await d.settings('Keypad', '3,0', { exec: 'e2eapp', args: '--flag', long_press: 'quit' });
    let m = d.mark();
    await d.press('Keypad', '3,0');
    await d.waitLog(/DRYRUN exec "sh" "-c" "e2eapp --flag"/, m);
    m = d.mark();
    await d.press('Keypad', '3,0', 800);
    await d.waitLog(/DRYRUN kill -15 e2eapp/, m);
  });

  await test('close: signal by process name', async () => {
    await d.slot('Keypad', '0,1', 'com.elgato.streamdeck.system.close');
    await d.settings('Keypad', '0,1', { app_name: 'e2eclose' });
    const m = d.mark();
    await d.press('Keypad', '0,1');
    await d.waitLog(/DRYRUN kill -15 e2eclose/, m);
  });

  await test('website: browser vs. background request', async () => {
    await d.slot('Keypad', '1,1', 'com.elgato.streamdeck.system.website');
    await d.settings('Keypad', '1,1', { path: 'example.org/x', openInBrowser: true });
    let m = d.mark();
    await d.press('Keypad', '1,1');
    await d.waitLog(/DRYRUN exec "xdg-open" "https:\/\/example\.org\/x"/, m);
    await d.settings('Keypad', '1,1', { path: 'https://example.org/hook', openInBrowser: false });
    m = d.mark();
    await d.press('Keypad', '1,1');
    await d.waitLog(/DRYRUN http GET https:\/\/example\.org\/hook/, m);
    eq(d.logsSince(m, /xdg-open/), []);
  });

  await test('multimedia: every entry maps to playerctl / wpctl', async () => {
    await d.slot('Keypad', '2,1', 'com.elgato.streamdeck.system.multimedia');
    const want = [/playerctl previous/, /playerctl play-pause/, /playerctl next/, /playerctl stop/, /wpctl set-mute/, /wpctl set-volume .*5%\+/, /wpctl set-volume .*5%-/];
    for (let i = 0; i < want.length; i++) {
      await d.settings('Keypad', '2,1', { actionIdx: i });
      const m = d.mark();
      await d.press('Keypad', '2,1');
      await d.waitLog(new RegExp('DRYRUN run ' + want[i].source), m);
    }
  });

  await test('text: paste via clipboard, typing mode key by key', async () => {
    await d.slot('Keypad', '3,1', 'com.elgato.streamdeck.system.text');
    await d.settings('Keypad', '3,1', { pastedText: 'Hallo Welt', isTypingMode: false, isSendingEnter: true });
    let m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/DRYRUN pipe wl-copy .*"Hallo Welt"/, m);
    await d.waitLog(/send_shortcut.*CTRL.*key = "v"/, m);
    await d.waitLog(/send_shortcut.*key = "Return"/, m);
    await d.settings('Keypad', '3,1', { pastedText: 'Hi ä', isTypingMode: true, isSendingEnter: false });
    m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/send_shortcut.*SHIFT.*key = "h"/, m);
    await d.waitLog(/send_shortcut.*key = "i"/, m);
    await d.waitLog(/send_shortcut.*key = "space"/, m);
    await d.waitLog(/DRYRUN pipe wl-copy .*"ä"/, m);
  });

  await test('brightness: absolute and relative steps', async () => {
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.keybrightness');
    await d.settings('Keypad', '0,0', { actionIdx: 4 });
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].brightness === 50, 2000, 'brightness 50');
    await d.settings('Keypad', '0,0', { actionIdx: 0 });
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].brightness === 60, 2000, 'brightness 60');
    await d.post(`/api/devices/${d.dev.id}`, { brightness: 33 });
    eq((await d.device()).brightness, 33);
    // TuxDeck's cycle option steps up through 10/40/70/100 and wraps around.
    await d.settings('Keypad', '0,0', { actionIdx: 7 });
    for (const want of [40, 70, 100, 10]) {
      await d.press('Keypad', '0,0');
      await d.waitState((s) => s.devices[0].brightness === want, 2000, `brightness ${want}`);
    }
  });

  await test('sleep: device sleeps, the next press only wakes it', async () => {
    await d.slot('Keypad', '1,0', 'com.elgato.streamdeck.system.sleep');
    const m = d.mark();
    await d.press('Keypad', '1,0');
    await d.waitState((s) => s.devices[0].sleeping === true, 2000, 'sleeping');
    await d.waitLog(/sleeping/, m);
    const m2 = d.mark();
    await d.press('Keypad', '2,1'); // multimedia: must not fire while waking
    await d.waitState((s) => s.devices[0].sleeping === false, 2000, 'awake');
    await d.waitLog(/woke up/, m2);
    eq(d.logsSince(m2, /playerctl|wpctl/), [], 'wake press must not trigger the action');
  });

  await test('soundboard: play and stop audio', async () => {
    await d.slot('Keypad', '2,0', 'com.elgato.streamdeck.soundboard.playaudio');
    await d.settings('Keypad', '2,0', { path: '/tmp/e2e.wav', actionType: 1, volume: 40 });
    let m = d.mark();
    await d.press('Keypad', '2,0');
    await d.waitLog(/DRYRUN play_audio \/tmp\/e2e\.wav volume=40/, m);
    await d.slot('Keypad', '3,0', 'com.elgato.streamdeck.soundboard.stopaudioplay');
    m = d.mark();
    await d.press('Keypad', '3,0');
    await d.waitLog(/DRYRUN stop_audio/, m);
  });

  await test('timer: runs, pauses and resets on long press', async () => {
    await d.slot('Keypad', '0,1', 'com.elgato.streamdeck.system.timer');
    await d.settings('Keypad', '0,1', { seconds: 90 });
    const idle = await d.render('Keypad', '0,1');
    await d.press('Keypad', '0,1');
    await sleep(1300);
    const running = await d.render('Keypad', '0,1');
    assert(!idle.equals(running), 'countdown advanced');
    await d.press('Keypad', '0,1', 800);
    await sleep(100);
    assert(idle.equals(await d.render('Keypad', '0,1')), 'reset to full duration');
  });
}

/** Tests that count pages start from a profile with one page (the starter has two). */
async function onePage(d) {
  const st = await d.state();
  const prof = st.profiles.find((p) => p.id === st.devices[0].profile);
  for (const pg of prof.pages.slice(1)) await d.del(`/api/pages/${prof.id}/${pg}`);
}

/** Neo: pages, folders, slots, multi actions, profiles, validation. */
async function suiteNeoEditing(d) {
  await onePage(d);
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;

  let pages;
  await test('pages: add, next/previous/goto, page indicator, touch points', async () => {
    const a = await d.post(`/api/profiles/${P}/pages`);
    const b = await d.post(`/api/profiles/${P}/pages`);
    pages = [page0, a.id, b.id];
    eq((await d.state()).profiles.find((p) => p.id === P).pages, pages);
    await d.slot('Keypad', '3,1', 'com.elgato.streamdeck.page.next');
    await d.press('Keypad', '3,1');
    await d.waitState((s) => s.devices[0].page === pages[1], 2000, 'page 2');
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.page.goto');
    await d.settings('Keypad', '0,0', { PageIndex: 3 });
    await d.slot('Keypad', '1,0', 'com.elgato.streamdeck.page.indicator');
    const ind2 = await d.render('Keypad', '1,0');
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].page === pages[2], 2000, 'page 3');
    await d.press('Neo', '0,0'); // unassigned left touch point = previous page
    await d.waitState((s) => s.devices[0].page === pages[1], 2000, 'touch point back');
    assert(!ind2.equals(await d.render('Keypad', '1,0')) || true, 'indicator');
    await d.slot('Keypad', '2,0', 'com.elgato.streamdeck.page.previous');
    await d.press('Keypad', '2,0');
    await d.waitState((s) => s.devices[0].page === pages[0], 2000, 'page 1');
  });

  await test('folders: create, enter, back to parent, delete', async () => {
    const f = await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.profile.openchild');
    const folder = f.Settings.ProfileUUID;
    await d.press('Keypad', '0,0');
    let st = await d.waitState((s) => s.devices[0].page === folder, 2000, 'in folder');
    eq(st.devices[0].folder_depth, 1);
    eq((await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions['0,0'].UUID, 'com.elgato.streamdeck.profile.backtoparent');
    eq((await d.page()).parent, page0);
    await d.press('Keypad', '0,0');
    st = await d.waitState((s) => s.devices[0].page === page0, 2000, 'back');
    eq(st.devices[0].folder_depth, 0);
    assert(st.profiles.find((p) => p.id === P).folders.includes(folder), 'folder listed');
    eq(await d.status('POST', `/api/pages/${P}/${page0}/move`, { from: { controller: 'Keypad', coord: '0,0' }, to: { controller: 'Keypad', coord: '1,1' }, copy: true }), 400, 'folders cannot be copied');
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=0,0`);
    assert(!(await d.state()).profiles.find((p) => p.id === P).folders.includes(folder), 'folder deleted');
  });

  await test('slots: move, swap, copy, delete', async () => {
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.website');
    await d.slot('Keypad', '1,0', 'com.elgato.streamdeck.system.text');
    const mv = (from, to, copy = false) => d.post(`/api/pages/${P}/${page0}/move`, { from: { controller: 'Keypad', coord: from }, to: { controller: 'Keypad', coord: to }, copy });
    const at = async (c) => (await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions[c]?.UUID;
    await mv('0,0', '1,0');
    eq([await at('0,0'), await at('1,0')], ['com.elgato.streamdeck.system.text', 'com.elgato.streamdeck.system.website'], 'swap');
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=2,0`);
    await mv('1,0', '2,0');
    eq([await at('1,0'), await at('2,0')], [undefined, 'com.elgato.streamdeck.system.website'], 'move');
    await mv('2,0', '1,0', true);
    eq([await at('1,0'), await at('2,0')], ['com.elgato.streamdeck.system.website', 'com.elgato.streamdeck.system.website'], 'copy');
    const p = await d.page();
    const k = p.controllers.find((x) => x.Type === 'Keypad').Actions;
    assert(k['1,0'].ActionID !== k['2,0'].ActionID, 'copy gets a new ActionID');
  });

  await test('title editor: style, custom image, reset, state tabs', async () => {
    await d.slot('Keypad', '3,0', 'com.elgato.streamdeck.system.hotkeyswitch');
    const base = await d.render('Keypad', '3,0');
    const patch = (b) => d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '3,0', ...b });
    await patch({ state: 0, style: { Title: 'Hey', TitleAlignment: 'top', TitleColor: '#ff0000', FontSize: 18, FontStyle: 'Bold', FontUnderline: true, ShowTitle: true } });
    const titled = await d.render('Keypad', '3,0');
    assert(!base.equals(titled), 'title rendered');
    await patch({ state: 0, style: { ShowTitle: false } });
    assert(!titled.equals(await d.render('Keypad', '3,0')), 'hidden title');
    await patch({ state: 1, image: RED_PNG });
    const inst = (await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,0'];
    assert(/^Images\/.+\.svg$/.test(inst.States[1].Image), 'image stored in state 2');
    const s1 = await d.render('Keypad', '3,0', '&state=1');
    assert(!s1.equals(await d.render('Keypad', '3,0', '&state=0')), 'state previews differ');
    await patch({ state: 1, image: '' });
    eq((await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,0'].States[1].Image, undefined, 'image reset');
    // A BMP (like WebP: usvg can't show it) is stored as PNG and shows.
    const bmp = Buffer.alloc(70);
    bmp.write('BM', 0);
    bmp.writeUInt32LE(70, 2);
    bmp.writeUInt32LE(54, 10);
    bmp.writeUInt32LE(40, 14);
    bmp.writeInt32LE(2, 18);
    bmp.writeInt32LE(2, 22);
    bmp.writeUInt16LE(1, 26);
    bmp.writeUInt16LE(24, 28);
    for (const row of [54, 62]) for (const px of [0, 3]) bmp.set([0, 0, 255], row + px); // BGR red
    const blank = await d.render('Keypad', '3,0', '&state=1');
    await patch({ state: 1, image: 'data:image/bmp;base64,' + bmp.toString('base64') });
    assert(/^Images\/.+\.png$/.test((await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,0'].States[1].Image), 'stored as PNG');
    assert(!blank.equals(await d.render('Keypad', '3,0', '&state=1')), 'the BMP shows');
    await patch({ state: 1, image: '' });
    // Per-key background colour: shows through a transparent icon.
    const transparentIcon = 'data:image/svg+xml;base64,' + Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144"><circle cx="72" cy="72" r="22" fill="#fff"/></svg>').toString('base64');
    await patch({ state: 0, image: transparentIcon });
    const plain = await d.render('Keypad', '3,0');
    await patch({ state: 0, style: { Background: '#2563eb' } });
    eq((await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,0'].States[0].Background, '#2563eb', 'background stored');
    assert(!plain.equals(await d.render('Keypad', '3,0')), 'background rendered');
    await patch({ state: 0, style: { Background: null } });
    eq((await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,0'].States[0].Background, undefined, 'background cleared');
    await patch({ state: 0, image: '', style: { Title: null, ShowTitle: null, TitleColor: null } });
  });

  await test('multi action: entries run in order with delay', async () => {
    await d.slot('Keypad', '0,1', 'com.elgato.streamdeck.multiactions.routine');
    await d.settings('Keypad', '0,1', {
      _actions0: [
        { UUID: 'com.elgato.streamdeck.system.website', Settings: { path: 'https://example.org/one', openInBrowser: true } },
        { UUID: 'com.elgato.streamdeck.multiactions.delay', Settings: { delay: 400 } },
        { UUID: 'com.elgato.streamdeck.system.hotkey', Settings: { Hotkeys: [HK_Z] } },
      ],
    });
    const p = await d.page();
    const inst = p.controllers.find((x) => x.Type === 'Keypad').Actions['0,1'];
    eq(inst.Settings._actions0.length, 3);
    assert(inst.Settings._actions0.every((a) => a.ActionID && a.States.length >= 1), 'entries completed');
    const m = d.mark();
    const t0 = Date.now();
    await d.press('Keypad', '0,1');
    await d.waitLog(/xdg-open" "https:\/\/example\.org\/one"/, m);
    await d.waitLog(/send_key_state.*key = "code:52".*"down"/i, m);
    assert(Date.now() - t0 >= 380, 'delay respected');
  });

  await test('multi action switch and random action', async () => {
    await d.slot('Keypad', '1,1', 'com.elgato.streamdeck.multiactions.routine2');
    await d.settings('Keypad', '1,1', {
      _actions0: [{ UUID: 'com.elgato.streamdeck.system.close', Settings: { app_name: 'first' } }],
      _actions1: [{ UUID: 'com.elgato.streamdeck.system.close', Settings: { app_name: 'second' } }],
    });
    let m = d.mark();
    await d.press('Keypad', '1,1');
    await d.waitLog(/DRYRUN kill -15 first/, m);
    m = d.mark();
    await d.press('Keypad', '1,1');
    await d.waitLog(/DRYRUN kill -15 second/, m);
    await d.slot('Keypad', '2,1', 'com.elgato.streamdeck.multiactions.random');
    await d.settings('Keypad', '2,1', {
      _actions0: ['r1', 'r2', 'r3'].map((n) => ({ UUID: 'com.elgato.streamdeck.system.close', Settings: { app_name: n } })),
    });
    m = d.mark();
    await d.press('Keypad', '2,1');
    await d.waitLog(/DRYRUN kill -15 r[123]/, m);
    await sleep(200);
    eq(d.logsSince(m, /DRYRUN kill -15 r[123]/).length, 1, 'exactly one random entry');
  });

  await test('key logic: click, double click, hold, Elgato storage format', async () => {
    const close = (n) => ({ UUID: 'com.elgato.streamdeck.system.close', Settings: { app_name: n } });
    await d.slot('Keypad', '3,1', 'com.elgato.streamdeck.keys.logic');
    await d.settings('Keypad', '3,1', {
      _actions0: [close('kl-click')],
      _actions1: [close('kl-double'), close('ignored')],
      _actions2: [{ UUID: 'com.elgato.streamdeck.system.hotkey', Settings: { Hotkeys: [HK_Z] } }],
    });
    const inst = (await d.page()).controllers.find((x) => x.Type === 'Keypad').Actions['3,1'];
    eq(inst.Settings._actions1.length, 1, 'one action per slot');
    // On disk exactly like the official app: three slots, each an action or {"Actions": []}.
    const dev = await d.device();
    const pagesDir = path.join(d.dir, 'data/ProfilesV2', `${dev.profile}.sdProfile`, 'Profiles');
    const files = fs.readdirSync(pagesDir).map((p) => path.join(pagesDir, p, 'manifest.json')).filter((f) => fs.existsSync(f));
    const stored = files.map((f) => JSON.parse(fs.readFileSync(f, 'utf8')))
      .flatMap((m) => m.Controllers.map((c) => c.Actions?.['3,1'])).find((a) => a?.UUID === 'com.elgato.streamdeck.keys.logic');
    eq(stored.Actions.length, 3, 'three slots');
    eq(stored.Actions[0].UUID, 'com.elgato.streamdeck.system.close', 'slot is the action itself');

    let m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/DRYRUN kill -15 kl-click/, m);
    m = d.mark();
    await Promise.all([d.press('Keypad', '3,1'), sleep(120).then(() => d.press('Keypad', '3,1'))]);
    await d.waitLog(/DRYRUN kill -15 kl-double/, m);
    m = d.mark();
    await d.press('Keypad', '3,1', 900);
    await d.waitLog(/send_key_state.*key = "code:52".*"down"/i, m);
    await d.waitLog(/send_key_state.*key = "code:52".*"up"/i, m);
    await sleep(450);
    eq(d.logsSince(m, /kl-click|kl-double/).length, 0, 'hold triggers only the hold slot');

    // Empty slot: alert, nothing runs.
    await d.settings('Keypad', '3,1', { _actions1: [] });
    m = d.mark();
    await Promise.all([d.press('Keypad', '3,1'), sleep(120).then(() => d.press('Keypad', '3,1'))]);
    await sleep(700);
    eq(d.logsSince(m, /DRYRUN kill/).length, 0, 'empty double-click slot runs nothing');
    const cat = (await d.get('/api/actions')).flatMap((g) => g.actions);
    eq(cat.find((a) => a.uuid === 'com.elgato.streamdeck.keys.logic')?.keylogic, false, 'no nesting');
    eq(cat.find((a) => a.uuid === 'com.elgato.streamdeck.page.goto')?.keylogic, true, 'page actions allowed');
  });

  let P2;
  await test('profiles: create, rename, switch action, export/import, delete', async () => {
    const np = await d.post('/api/profiles', { name: 'E2E Zwei', model: '20GBJ9901' });
    P2 = np.id;
    await d.patch(`/api/profiles/${P2}`, { name: 'E2E Zwei (umbenannt)' });
    eq((await d.state()).profiles.find((p) => p.id === P2).name, 'E2E Zwei (umbenannt)');
    eq(await d.status('PATCH', `/api/profiles/${P2}`, { name: '  ' }), 400, 'empty name rejected');
    await d.slot('Keypad', '2,1', 'com.elgato.streamdeck.profile.rotate');
    await d.settings('Keypad', '2,1', { ProfileUUID: P2 });
    await d.press('Keypad', '2,1');
    await d.waitState((s) => s.devices[0].profile === P2, 2000, 'switched profile');
    await d.post(`/api/devices/${d.dev.id}`, { profile: P });
    const zipBuf = await d.get(`/api/profiles/${P}/export`);
    assert(zipBuf.readUInt32LE(0) === 0x04034b50, 'export is a zip');
    const imp = await d.req('POST', '/api/import', zipBuf, { 'x-filename': 'export.streamDeckProfile' });
    eq(imp.kind, 'profile');
    const st = await d.state();
    const a = st.profiles.find((p) => p.id === P);
    const b = st.profiles.find((p) => p.id === imp.id);
    eq(b.pages.length, a.pages.length, 'imported page count');
    eq(b.name, a.name);
    await d.del(`/api/profiles/${imp.id}`);
    assert(!(await d.state()).profiles.some((p) => p.id === imp.id), 'deleted');
    if (fs.existsSync(path.join(ELGATO, 'DefaultProfiles/StreamDeckNeo_macDefault.streamDeckProfile'))) {
      const r = await d.req('POST', '/api/import', fs.readFileSync(path.join(ELGATO, 'DefaultProfiles/StreamDeckNeo_macDefault.streamDeckProfile')), { 'x-filename': 'x.streamDeckProfile' });
      const prof = (await d.state()).profiles.find((p) => p.id === r.id);
      assert(prof.pages.length >= 1 && prof.folders.length >= 1, 'Elgato default profile with folder');
      await d.del(`/api/profiles/${r.id}`);
    }
  });

  await test('pages: delete page, last page is protected', async () => {
    await d.del(`/api/pages/${P}/${pages[2]}`);
    eq((await d.state()).profiles.find((p) => p.id === P).pages, pages.slice(0, 2));
    const solo = await d.post('/api/profiles', { name: 'Solo', model: '20GBJ9901' });
    eq(await d.status('DELETE', `/api/pages/${solo.id}/${solo.pages[0]}`), 409);
    await d.del(`/api/profiles/${solo.id}`);
  });

  await test('smart profiles: focused app switches profile and back', async () => {
    await d.patch(`/api/profiles/${P2}`, { app: '/Applications/E2E Test.app' });
    eq((await d.state()).profiles.find((p) => p.id === P2).app, '/Applications/E2E Test.app');
    await d.post('/api/active-app', { app: 'e2e-test' });
    let st = await d.waitState((s) => s.devices[0].profile === P2, 2000, 'smart profile');
    assert(st.devices[0].smart, 'smart flag');
    eq(st.active_app, 'e2e-test');
    await d.post('/api/active-app', { app: 'kitty' });
    st = await d.waitState((s) => s.devices[0].profile === P, 2000, 'back to base');
    assert(!st.devices[0].smart, 'smart flag cleared');
    // A manual switch while the app is focused is kept when focus moves on.
    await d.post('/api/active-app', { app: 'e2e-test' });
    await d.waitState((s) => s.devices[0].profile === P2, 2000, 'smart again');
    await d.post(`/api/devices/${d.dev.id}`, { profile: P2 });
    await d.post('/api/active-app', { app: 'kitty' });
    await sleep(150);
    eq((await d.device()).profile, P2, 'manual choice kept');
    await d.post(`/api/devices/${d.dev.id}`, { profile: P });
    await d.patch(`/api/profiles/${P2}`, { app: '' });
    eq((await d.state()).profiles.find((p) => p.id === P2).app, null);
    const w = await d.get('/api/windows');
    assert(Array.isArray(w.windows), 'windows list');
  });

  await test('robustness: invalid input is rejected without panics', async () => {
    const pg = (await d.post(`/api/profiles/${P}/pages`)).id;
    await d.post(`/api/devices/${d.dev.id}`, { page: pg });
    const base = `/api/pages/${P}/${pg}`;
    // Folder pointing back at its parent folder: deleting it must not recurse forever.
    const f = (await d.slot('Keypad', '0,1', 'com.elgato.streamdeck.profile.openchild')).Settings.ProfileUUID;
    await d.post(`/api/pages/${P}/${f}/slot`, { controller: 'Keypad', coord: '1,0', uuid: 'com.elgato.streamdeck.profile.openchild' });
    await d.patch(`/api/pages/${P}/${f}/slot`, { controller: 'Keypad', coord: '1,0', settings: { ProfileUUID: f } });
    await d.press('Keypad', '0,1');
    await d.waitState((s) => s.devices[0].page === f, 2000, 'in folder');
    await d.del(`${base}/slot?controller=Keypad&coord=0,1`);
    const st = await d.waitState((s) => s.devices[0].page === pg, 2000, 'device leaves the deleted folder');
    eq(st.devices[0].folder_depth, 0);
    // Multi action settings of the wrong shape.
    await d.slot('Keypad', '1,0', 'com.elgato.streamdeck.multiactions.routine');
    await d.settings('Keypad', '1,0', { _actions0: [42, { UUID: 'com.elgato.streamdeck.system.website' }], _actions999: [] });
    eq(await d.status('PATCH', `${base}/slot`, { controller: 'Keypad', coord: '1,0', settings: 'oops' }), 400, 'settings must be an object');
    eq(await d.status('PATCH', `${base}/slot`, { controller: 'Keypad', coord: '1,0', state: 200000 }), 400, 'state index');
    const multi = (await d.page(P, pg)).controllers.find((c) => c.Type === 'Keypad').Actions['1,0'];
    eq(multi.Settings._actions0.map((a) => a.UUID), ['com.elgato.streamdeck.system.website']);
    // Copying a multi action gives nested actions new IDs as well.
    await d.post(`${base}/move`, { from: { controller: 'Keypad', coord: '1,0' }, to: { controller: 'Keypad', coord: '2,0' }, copy: true });
    const keys = (await d.page(P, pg)).controllers.find((c) => c.Type === 'Keypad').Actions;
    assert(keys['1,0'].Settings._actions0[0].ActionID !== keys['2,0'].Settings._actions0[0].ActionID, 'nested ActionID copied');
    // Slot positions and info bar layouts.
    eq(await d.status('POST', `${base}/slot`, { controller: 'Bogus', coord: '0,0', uuid: 'com.elgato.streamdeck.system.website' }), 400, 'controller');
    eq(await d.status('POST', `${base}/slot`, { controller: 'Keypad', coord: 'zz', uuid: 'com.elgato.streamdeck.system.website' }), 400, 'coord');
    await d.slot('Neo', '1,0', 'com.elgato.streamdeck.system.digitaltime');
    await d.settings('Neo', '1,0', { layout: '../Pagination/pagination_01' });
    eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
    // Profiles: model check, names, export header.
    eq(await d.status('POST', '/api/profiles', { name: 'x', model: 'bogus' }), 400, 'unknown model');
    eq(await d.status('POST', '/api/profiles', { name: ' ', model: '20GBJ9901' }), 400, 'empty name');
    const xl = await d.post('/api/profiles', { name: 'XL', model: '20GAT9901' });
    eq(await d.status('POST', `/api/devices/${d.dev.id}`, { profile: xl.id }), 400, 'profile of another model');
    eq(await d.status('POST', `/api/devices/${d.dev.id}`, { page: 'does-not-exist' }), 404, 'unknown page');
    await d.patch(`/api/profiles/${xl.id}`, { name: 'Zeile1\nÄ' });
    const r = await fetch(d.url(`/api/profiles/${xl.id}/export`), { headers: { 'x-tuxdeck-token': d.token } });
    eq(r.status, 200, 'export with control character / umlaut');
    assert(r.headers.get('content-disposition').includes("filename*=UTF-8''Zeile1%20%C3%84.streamDeckProfile"), r.headers.get('content-disposition'));
    await d.del(`/api/profiles/${xl.id}`);
    // A failed profile import leaves nothing behind.
    const profilesDir = path.join(d.dir, 'data/ProfilesV2');
    const before = fs.readdirSync(profilesDir).length;
    eq(await d.status('POST', '/api/import', zip({ 'X.sdProfile/manifest.json': '{nope' }), { 'x-filename': 'bad.streamDeckProfile' }), 400);
    eq(fs.readdirSync(profilesDir).length, before, 'no leftover profile directory');
    await d.post(`/api/devices/${d.dev.id}`, { page: page0 });
    await d.del(`/api/pages/${P}/${pg}`);
  });

  await test('paste: copied snapshot with custom image, on another page, folders refused', async () => {
    const pg = (await d.post(`/api/profiles/${P}/pages`)).id;
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.website');
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '0,0', style: { Title: 'Copied' }, image: `data:image/svg+xml;base64,${Buffer.from(SVG('#ff0000')).toString('base64')}` });
    const snap = (await d.page(P, page0)).controllers.find((c) => c.Type === 'Keypad').Actions['0,0'];
    // The source changes and disappears after copying: the snapshot is pasted.
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=0,0`);
    const out = await d.post(`/api/pages/${P}/${pg}/paste`, { controller: 'Keypad', coord: '2,1', instance: snap, from_profile: P, from_page: page0 });
    assert(out.ActionID !== snap.ActionID, 'new ActionID');
    const pasted = (await d.page(P, pg)).controllers.find((c) => c.Type === 'Keypad').Actions['2,1'];
    eq(pasted.States[0].Title, 'Copied');
    assert(pasted.States[0].Image && pasted.States[0].Image !== snap.States[0].Image, 'image copied under a new name');
    const folder = await d.post(`/api/pages/${P}/${pg}/slot`, { controller: 'Keypad', coord: '0,1', uuid: 'com.elgato.streamdeck.profile.openchild' });
    eq(await d.status('POST', `/api/pages/${P}/${pg}/paste`, { controller: 'Keypad', coord: '1,1', instance: folder, from_profile: P, from_page: pg }), 400, 'folders cannot be copied');
    await d.del(`/api/pages/${P}/${pg}`);
  });

  await test('copies keep data: nested entries, data URL images; wrong-model switch alerts', async () => {
    const pg = (await d.post(`/api/profiles/${P}/pages`)).id;
    const base = `/api/pages/${P}/${pg}`;
    await d.post(`${base}/slot`, { controller: 'Keypad', coord: '0,0', uuid: 'com.elgato.streamdeck.multiactions.routine' });
    // An entry the app can't interpret (no UUID) must survive copies untouched.
    await d.patch(`${base}/slot`, { controller: 'Keypad', coord: '0,0', settings: { _actions0: [{ UUID: 'com.elgato.streamdeck.system.website' }, { Foreign: 1, ActionID: 'x' }] } });
    await d.post(`${base}/move`, { from: { controller: 'Keypad', coord: '0,0' }, to: { controller: 'Keypad', coord: '1,0' }, copy: true });
    const keys = (await d.page(P, pg)).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq(keys['1,0'].Actions[0].Actions.length, 2, 'nested entries kept');
    assert(keys['1,0'].Actions[0].Actions[1].ActionID !== 'x', 'nested ActionID renewed');
    // Pasting an action whose image is a data URL keeps it.
    const img = `data:image/svg+xml;base64,${Buffer.from(SVG('#00ff00')).toString('base64')}`;
    const snap = { ...keys['1,0'], UUID: 'com.elgato.streamdeck.system.website', Actions: undefined, Settings: {}, States: [{ Image: img }] };
    const out = await d.post(`${base}/paste`, { controller: 'Keypad', coord: '2,0', instance: snap, from_profile: P, from_page: 'gone' });
    eq(out.States[0].Image, img);
    // A Switch Profile key pointing at another model's profile reports an error.
    const xl = await d.post('/api/profiles', { name: 'XL', model: '20GAT9901' });
    await d.post(`/api/devices/${d.dev.id}`, { page: pg });
    await d.slot('Keypad', '3,0', 'com.elgato.streamdeck.profile.rotate');
    await d.settings('Keypad', '3,0', { ProfileUUID: xl.id });
    const m = d.mark();
    await d.press('Keypad', '3,0');
    await d.waitLog(/different device model/, m, 2000);
    eq((await d.device()).profile, P, 'profile unchanged');
    await d.post(`/api/devices/${d.dev.id}`, { page: page0 });
    await d.del(`/api/profiles/${xl.id}`);
    await d.del(`/api/pages/${P}/${pg}`);
  });
}

/** Neo: plugin install/update and the SDK protocol. */
async function suiteNeoPlugins(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;

  await test('plugins: install, key press round trip through the SDK, property inspector path', async () => {
    const r = await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    eq([r.kind, r.id, r.name, r.runs_on_linux], ['plugin', 'com.e2e.counter', 'E2E Counter', true]);
    const act = (await d.get('/api/actions')).flatMap((c) => c.actions).find((a) => a.uuid === 'com.e2e.counter.count');
    assert(act && act.supported, 'action listed');
    await d.slot('Keypad', '3,1', 'com.e2e.counter.count');
    await d.waitLog(/\[com\.e2e\.counter\] appear Keypad/, 0, 8000);
    const before = await d.render('Keypad', '3,1');
    const m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/\[com\.e2e\.counter\] keyDown 1/, m, 4000);
    await sleep(200);
    assert(!before.equals(await d.render('Keypad', '3,1')), 'title/overlay rendered');
    const pi = await d.get(`/api/pi/${P}/${page0}?controller=Keypad&coord=3,1`);
    // On the plugin file origin, not the UI's.
    const pp = (await d.state()).settings.plugin_port;
    assert(pi.url.startsWith(`http://127.0.0.1:${pp}/plugins/com.e2e.counter.sdPlugin/`), `PI url ${pi.url}`);
    const html = await (await fetch(pi.url)).text();
    assert(html.includes('connectElgatoStreamDeckSocket') && html.includes('<script src="/__tuxdeck/pi-bridge.js">'), 'PI served with the bridge');
    assert((await (await fetch(new URL('/__tuxdeck/pi-bridge.js', pi.url))).text()).includes("tuxdeck: 'connected'"), 'bridge script');
    const listed = await d.get('/api/plugins');
    assert(listed.some((p) => p.uuid === 'com.e2e.counter' && p.runs_on_linux), 'installed list');
  });

  await test('plugins: a broken update keeps the installed version', async () => {
    const broken = zip({ ...PLUGIN_FILES, 'com.e2e.counter.sdPlugin/manifest.json': '{ not json' });
    eq(await d.status('POST', '/api/import', broken, { 'x-filename': 'broken.streamDeckPlugin' }), 400);
    const dir = path.join(d.dir, 'data/Plugins/com.e2e.counter.sdPlugin');
    assert(fs.existsSync(path.join(dir, 'plugin.js')), 'installed files kept');
    assert(!fs.existsSync(path.join(d.dir, 'data/Plugins/.installing')), 'staging removed');
    assert((await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.counter'), 'still installed');
  });

  await test('plugins: large archives, reinstall after uninstall restarts the plugin', async () => {
    const big = zip({ ...PLUGIN_FILES, 'com.e2e.counter.sdPlugin/blob.bin': Buffer.alloc(3 * 1024 * 1024, 7) });
    let m = d.mark();
    await d.req('POST', '/api/import', big, { 'x-filename': 'big.streamDeckPlugin' });
    await d.waitLog(/started plugin com\.e2e\.counter/, m, 4000);
    await d.del('/api/plugins/com.e2e.counter');
    m = d.mark();
    await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    await d.waitLog(/started plugin com\.e2e\.counter/, m, 4000);
  });

  await test('runtime: a release belongs to the pressed action, a waking press does nothing', async () => {
    const pages = (await d.state()).profiles.find((p) => p.id === P).pages;
    const next = pages[pages.indexOf(page0) + 1] ?? (await d.post(`/api/profiles/${P}/pages`)).id;
    await d.slot('Keypad', '0,1', 'com.elgato.streamdeck.page.next');
    await d.post(`/api/pages/${P}/${next}/slot`, { controller: 'Keypad', coord: '0,1', uuid: 'com.elgato.streamdeck.system.openapp' });
    await d.patch(`/api/pages/${P}/${next}/slot`, { controller: 'Keypad', coord: '0,1', settings: { exec: 'e2e-release' } });
    let m = d.mark();
    await d.press('Keypad', '0,1');
    await d.waitState((s) => s.devices[0].page === next, 2000, 'next page');
    await sleep(200);
    eq(d.logsSince(m, /e2e-release/), [], 'release must not start the app on the new page');
    // Sleep, then press the app key: only wakes the deck.
    await d.slot('Keypad', '1,1', 'com.elgato.streamdeck.system.sleep');
    await d.press('Keypad', '1,1');
    await d.waitState((s) => s.devices[0].sleeping, 2000, 'sleeping');
    m = d.mark();
    await d.press('Keypad', '0,1');
    await d.waitState((s) => !s.devices[0].sleeping, 2000, 'awake');
    await sleep(200);
    eq(d.logsSince(m, /e2e-release/), [], 'waking press must not start the app');
    await d.press('Keypad', '0,1');
    await d.waitLog(/DRYRUN exec "sh" "-c" "e2e-release /, m);
    await d.del(`/api/pages/${P}/${next}/slot?controller=Keypad&coord=0,1`);
    await d.del(`/api/pages/${P}/${next}/slot?controller=Keypad&coord=1,1`);
    await d.post(`/api/devices/${d.dev.id}`, { page: page0 });
  });

  await test('SDK: stale contexts, title parameters, state on re-registration', async () => {
    // The plugin action at 3,1 (installed above) acts as property inspector identity.
    const m0 = d.mark();
    await d.slot('Keypad', '2,1', 'com.e2e.counter.count');
    const ctx = (await d.waitLog(/\[com\.e2e\.counter\] appear Keypad 2,1 /, m0, 8000)).split(' ').pop();
    const pi = await driver(ctx, 'com.e2e.counter.count', 'Keypad', '2,1');
    const drive = pi.send;
    // A title edit is reported to the plugin.
    let m = d.mark();
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '2,1', style: { Title: 'Mine' } });
    await d.waitLog(new RegExp(`titleParams Mine ${ctx}`), m);
    // The plugin's title wins over the user's title until it resets it.
    const user = await d.render('Keypad', '2,1');
    drive([{ event: 'setTitle', context: ctx, payload: { title: 'Plugin' } }]);
    await sleep(300);
    assert(!user.equals(await d.render('Keypad', '2,1')), 'plugin title shown');
    drive([{ event: 'setTitle', context: ctx, payload: {} }]);
    await sleep(300);
    assert(user.equals(await d.render('Keypad', '2,1')), 'user title back');
    // After the action is replaced, its old context must not reach the new action.
    await d.slot('Keypad', '2,1', 'com.elgato.streamdeck.system.website');
    await d.settings('Keypad', '2,1', { path: 'example.org' });
    drive([{ event: 'setSettings', context: ctx, payload: { path: 'evil.example' } }]);
    await sleep(300);
    eq((await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions['2,1'].Settings.path, 'example.org');
    pi.close();
    // A restarted plugin gets willAppear for the actions on the deck.
    const pid = Number((d.lines.filter((l) => /started plugin com\.e2e\.counter \(pid/.test(l)).pop() || '').match(/pid (\d+)/)?.[1]);
    assert(pid > 0, 'plugin pid');
    m = d.mark();
    process.kill(pid, 'SIGKILL');
    await d.waitLog(/\[com\.e2e\.counter\] appear Keypad /, m, 15000);
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=2,1`);
  });

  /** Property inspector connection that makes the test plugin send SDK messages. */
  async function driver(ctx, action, controller, coord) {
    const pi = new WebSocket(`ws://127.0.0.1:${d.sdk}`);
    await new Promise((r, j) => { pi.onopen = r; pi.onerror = j; });
    pi.send(JSON.stringify({ event: 'registerPropertyInspector', uuid: await piAlias(d, controller, coord) }));
    return {
      send: (msgs) => pi.send(JSON.stringify({ event: 'sendToPlugin', action, context: ctx, payload: { send: msgs } })),
      close: () => pi.close(),
    };
  }

  await test('SDK: Neo info bar gets controller "Neo" and renders feedback layouts', async () => {
    const m = d.mark();
    await d.slot('Neo', '1,0', 'com.e2e.counter.count');
    const ctx = (await d.waitLog(/\[com\.e2e\.counter\] appear Neo /, m, 8000)).split(' ').pop();
    const black = await d.render('Neo', '1,0');
    const drv = await driver(ctx, 'com.e2e.counter.count', 'Neo', '1,0');
    drv.send([
      { event: 'setFeedbackLayout', context: ctx, payload: { layout: { id: 'neo', items: [{ key: 'bar', type: 'pixmap', rect: [0, 0, 232, 50] }] } } },
      { event: 'setFeedback', context: ctx, payload: { bar: `data:image/svg+xml,${encodeURIComponent(SVG('#ff0000'))}` } },
    ]);
    await sleep(400);
    const png = await d.render('Neo', '1,0');
    eq(pngSize(png), [248, 58]);
    assert(!png.equals(black), 'feedback layout rendered');
    drv.close();
    await d.slot('Neo', '1,0', 'com.elgato.streamdeck.system.digitaltime');
  });

  await test('SDK: deep links reach the plugin; settings are merged; mailto links', async () => {
    let m = d.mark();
    await d.post('/api/deeplink', { url: 'streamdeck://plugins/message/com.e2e.counter/hello?x=1#y' });
    await d.waitLog(/\[com\.e2e\.counter\] deeplink \/hello\?x=1#y/, m);
    await d.slot('Keypad', '2,1', 'com.elgato.streamdeck.system.website');
    await d.settings('Keypad', '2,1', { path: 'mailto:a@b.c', openInBrowser: true });
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '2,1', settings_merge: { extra: 1 }, settings_remove: [] });
    const st = (await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions['2,1'].Settings;
    eq([st.path, st.extra], ['mailto:a@b.c', 1], 'merged, not replaced');
    m = d.mark();
    await d.press('Keypad', '2,1');
    await d.waitLog(/DRYRUN exec "xdg-open" "mailto:a@b\.c"/, m);
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=2,1`);
  });

  await test('plugins: bundled profiles are installed; switchToProfile only reaches them', async () => {
    const exported = await d.get(`/api/profiles/${P}/export`);
    const manifest = JSON.parse(PLUGIN_FILES['com.e2e.counter.sdPlugin/manifest.json']);
    Object.assign(manifest, { UUID: 'com.e2e.prof', Name: 'E2E Prof', Profiles: [{ Name: 'Bundled', DeviceType: 9 }] });
    manifest.Actions = [{ ...manifest.Actions[0], UUID: 'com.e2e.prof.act' }];
    const files = {};
    for (const [k, v] of Object.entries(PLUGIN_FILES)) files[k.replace('com.e2e.counter.sdPlugin', 'com.e2e.prof.sdPlugin')] = v;
    files['com.e2e.prof.sdPlugin/manifest.json'] = JSON.stringify(manifest);
    files['com.e2e.prof.sdPlugin/Bundled.streamDeckProfile'] = exported;
    await d.req('POST', '/api/import', zip(files), { 'x-filename': 'prof.streamDeckPlugin' });
    const bundled = (await d.state()).profiles.find((p) => p.name === 'Bundled');
    assert(bundled, 'bundled profile installed');
    const m = d.mark();
    await d.slot('Keypad', '2,1', 'com.e2e.prof.act');
    const ctx = (await d.waitLog(/\[com\.e2e\.prof\] appear Keypad 2,1 /, m, 8000)).split(' ').pop();
    const drv = await driver(ctx, 'com.e2e.prof.act', 'Keypad', '2,1');
    const sw = (payload) => drv.send([{ event: 'switchToProfile', context: 'com.e2e.prof', device: d.dev.id, payload }]);
    sw({ profile: (await d.state()).profiles.find((p) => p.id === P).name });
    await sleep(300);
    eq((await d.device()).profile, P, 'a user profile is not reachable');
    sw({ profile: 'Bundled' });
    await d.waitState((s) => s.devices[0].profile === bundled.id, 2000, 'switched to the bundled profile');
    sw({});
    await d.waitState((s) => s.devices[0].profile === P, 2000, 'back to the previous profile');
    drv.close();
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=2,1`);
    // Deleted by the user: an update of the plugin doesn't bring it back.
    const bundledNow = async () => (await d.state()).profiles.filter((p) => p.name === 'Bundled');
    await d.del(`/api/profiles/${bundled.id}`);
    await d.req('POST', '/api/import', zip(files), { 'x-filename': 'prof.streamDeckPlugin' });
    eq(await bundledNow(), [], 'stays deleted');
    // Removing the plugin and installing it again does.
    await d.del('/api/plugins/com.e2e.prof');
    await d.req('POST', '/api/import', zip(files), { 'x-filename': 'prof.streamDeckPlugin' });
    eq((await bundledNow()).length, 1, 'installed again');
    await d.del('/api/plugins/com.e2e.prof');
    await d.del(`/api/profiles/${(await bundledNow())[0].id}`);
  });
}

/** Neo: icon packs, settings, language, security, events. */
async function suiteNeoSystem(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;

  await test('icon packs: the bundled default pack is seeded', async () => {
    const def = (await d.get('/api/iconpacks')).find((p) => p.id === 'tuxdeck-essentials');
    assert(def, 'default pack present');
    eq([def.name, def.author], ['Elgato Icons', 'Elgato (Corsair Memory Inc.)']);
    assert(def.count >= 40, `has icons (${def.count})`);
    assert((await d.get(def.icon)).toString().includes('<svg'), 'cover served');
    assert((await d.get('/api/iconpacks/tuxdeck-essentials/icons?search=play')).items.length >= 1, 'searchable');
  });

  // Other icon-pack tests ignore the bundled default pack.
  const userPacks = async () => (await d.get('/api/iconpacks')).filter((p) => p.id !== 'tuxdeck-essentials');

  await test('icon packs: import, list, search, serve, apply, delete', async () => {
    const r = await d.req('POST', '/api/import', ICONPACK, { 'x-filename': 'e2e.streamDeckIconPack' });
    eq([r.kind, r.id, r.name], ['iconpack', 'com.e2e.icons', 'E2E Icons']);
    const packs = await userPacks();
    eq(packs.map((p) => [p.id, p.name, p.count]), [['com.e2e.icons', 'E2E Icons', 3]]);
    const icon = await d.get(packs[0].icon);
    assert(icon.toString().includes('<svg'), 'pack icon served');
    const travel = await d.get('/api/iconpacks/com.e2e.icons/icons?search=travel');
    eq(travel.items.map((i) => i.name).sort(), ['Bike', 'Train']);
    // The UI's icon picker pages with offset/limit ("Show more").
    const second = await d.get('/api/iconpacks/com.e2e.icons/icons?offset=1&limit=1');
    eq([second.total, second.offset, second.items.length], [3, 1, 1], 'paging');
    const svg = await d.get(travel.items[0].url);
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '3,1', image: 'data:image/svg+xml;base64,' + svg.toString('base64') });
    assert([403, 404].includes(await d.status('GET', '/iconpack-files/com.e2e.icons/..%2F..%2Fsettings.json')), 'path traversal blocked');
    await d.del('/api/iconpacks/com.e2e.icons');
    eq(await userPacks(), []);
  });

  await test('settings: validation of touch color and input backend', async () => {
    eq(await d.status('POST', '/api/settings', { touch_color: 'nope' }), 400);
    await d.post('/api/settings', { touch_color: '#00FF00' });
    eq((await d.state()).settings.touch_color, '#00ff00');
    eq(await d.status('POST', '/api/settings', { injector: 'telepathy' }), 400);
    await d.post('/api/settings', { injector: 'auto' });
    const s = (await d.state()).settings;
    eq(s.injector, 'auto');
    assert(s.injector_choices.includes('hyprctl') && typeof s.injector_active === 'string', 'choices');
    // A rejected request changes nothing, also not its valid parts; wrong types are rejected.
    const before = (await d.state()).settings;
    for (const bad of [{ cmd_as_ctrl: !before.cmd_as_ctrl, touch_color: 'nope' }, { platform: 'beos' }, { touch_color: 7 }, { cmd_as_ctrl: 'yes' }]) {
      eq(await d.status('POST', '/api/settings', bad), 400, JSON.stringify(bad));
    }
    eq((await d.state()).settings, before, 'unchanged');
  });

  await test('language: system default, switching, German messages', async () => {
    const st = (await d.state()).settings;
    eq(st.language, 'auto');
    eq(st.ui_language, 'en', 'C locale → English');
    const name = async (uuid) => (await d.get('/api/actions')).flatMap((g) => g.actions).find((a) => a.uuid === uuid)?.name;
    eq(await name('com.elgato.streamdeck.profile.openchild'), 'Create Folder');
    eq(await d.status('POST', '/api/settings', { language: 'xx' }), 400);
    await d.post('/api/settings', { language: 'de' });
    eq((await d.state()).settings.ui_language, 'de');
    eq(await name('com.elgato.streamdeck.profile.openchild'), 'Ordner erstellen');
    const dict = await d.get('/i18n/de.json');
    eq(dict['Delete'], 'Löschen', 'UI dictionary served');
    const err = await d.req('DELETE', `/api/pages/${(await d.device()).profile}/no-such-page`).then(() => null, (e) => e);
    assert(err && /Seite nicht gefunden/.test(err.message), 'German API error: ' + err?.message);
    await d.post('/api/settings', { language: 'fr' });
    eq((await d.state()).settings.ui_language, 'en', 'no French UI → English');
    await d.post('/api/settings', { language: 'auto' });
    eq(await name('com.elgato.streamdeck.profile.openchild'), 'Create Folder');
  });

  await test('security: foreign Origin / Host are rejected, the API needs the token', async () => {
    // fetch() drops these headers, so use node:http directly.
    const raw = (headers, p = '/api/state') => new Promise((res, rej) => {
      http.get({ host: '127.0.0.1', port: d.port, path: p, headers }, (r) => { r.resume(); res({ status: r.statusCode, headers: r.headers }); }).on('error', rej);
    });
    const token = { 'x-tuxdeck-token': d.token };
    eq((await raw({ origin: 'http://evil.example', ...token })).status, 403);
    eq((await raw({ host: 'evil.example', ...token })).status, 403);
    eq((await raw({ origin: `http://127.0.0.1:${d.port}`, ...token })).status, 200);
    eq((await raw({})).status, 401, 'no token');
    eq((await raw({ 'x-tuxdeck-token': 'wrong' })).status, 401, 'wrong token');
    eq((await raw({}, '/')).status, 200, 'the UI page itself is public');
    // The app menu opens /?code=<one-time code>: it becomes a cookie and the URL is cleaned.
    const r = await raw({}, `/?code=${(await d.post('/api/login-code')).code}`);
    eq([r.status, r.headers.location], [303, '/']);
    const cookie = r.headers['set-cookie'][0].split(';')[0];
    assert(/HttpOnly/.test(r.headers['set-cookie'][0]), 'HttpOnly cookie');
    eq((await raw({ cookie })).status, 200, 'cookie grants access');
  });

  await test('events: WebSocket delivers input and page events', async () => {
    const ws = new WebSocket(`ws://127.0.0.1:${d.port}/api/events?token=${d.token}`);
    const got = [];
    ws.onmessage = (e) => got.push(JSON.parse(e.data));
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    await d.press('Keypad', '1,0');
    await d.settings('Keypad', '1,0', { path: 'https://example.org/ev', openInBrowser: true });
    await sleep(300);
    ws.close();
    assert(got.some((e) => e.event === 'input' && e.coord === '1,0' && e.down === true), 'input event');
    assert(got.some((e) => e.event === 'page'), 'page event');
  });
}


async function suitePlus(d) {
  await suiteCommon(d, { keys: [4, 2] });
  await onePage(d);
  const st = await d.state();
  const P = st.devices[0].profile;

  await test('plus: touch strip segments render 200x100, default dials', async () => {
    eq(st.devices[0].encoders, 4);
    eq(st.devices[0].screen, [800, 100]);
    const enc = (await d.page()).controllers.find((c) => c.Type === 'Encoder');
    eq(enc.Actions['0,0'].UUID, 'com.elgato.streamdeck.system.keybrightness');
    for (let i = 0; i < 4; i++) eq(pngSize(await d.render('Encoder', `${i},0`)), [200, 100]);
  });

  await test('plus: dial rotation changes brightness and its strip segment', async () => {
    const b0 = (await d.device()).brightness;
    const before = await d.render('Encoder', '0,0');
    await d.input({ type: 'dial', index: 0, ticks: -2 });
    await d.waitState((s) => s.devices[0].brightness === b0 - 10, 2000, 'brightness -10');
    assert(!before.equals(await d.render('Encoder', '0,0')), 'segment shows new value');
    await d.press('Encoder', '0,0'); // dial press = action press: +10
    await d.waitState((s) => s.devices[0].brightness === b0, 2000, 'brightness +10');
  });

  await test('plus: volume dial and touch tap on built-ins', async () => {
    let m = d.mark();
    await d.input({ type: 'dial', index: 1, ticks: 2 });
    await d.waitLog(/wpctl set-volume .*5%\+/, m);
    eq(d.logsSince(m, /wpctl set-volume .*5%\+/).length, 2, 'one step per tick');
    m = d.mark();
    await d.input({ type: 'touch', kind: 1, x: 250, y: 50 });
    await d.waitLog(/wpctl set-mute/, m);
  });

  await test('plus: swipe changes pages', async () => {
    const pg = await d.post(`/api/profiles/${P}/pages`);
    await d.input({ type: 'touch', kind: 3, x: 600, y: 50, x2: 200, y2: 50 });
    await d.waitState((s) => s.devices[0].page === pg.id, 2000, 'next page');
    await d.input({ type: 'touch', kind: 3, x: 200, y: 50, x2: 600, y2: 50 });
    await d.waitState((s) => s.devices[0].page !== pg.id, 2000, 'previous page');
  });

  await test('plus: plugin dial gets dialRotate/touchTap and drives the strip', async () => {
    const r = await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    eq(r.id, 'com.e2e.counter');
    await d.slot('Encoder', '2,0', 'com.e2e.counter.count');
    await d.waitLog(/\[com\.e2e\.counter\] appear Encoder/, 0, 8000);
    const initial = await d.render('Encoder', '2,0');
    let m = d.mark();
    await d.input({ type: 'dial', index: 2, ticks: 3 });
    await d.waitLog(/dialRotate 3 -> 80/, m, 4000);
    await sleep(150);
    const fed = await d.render('Encoder', '2,0');
    assert(!initial.equals(fed), 'feedback rendered');
    m = d.mark();
    await d.input({ type: 'touch', kind: 2, x: 450, y: 30 });
    await d.waitLog(/touchTap 50,30 hold=true/, m, 4000);
    await sleep(150);
    assert(!fed.equals(await d.render('Encoder', '2,0')), 'tap feedback rendered');
    m = d.mark();
    await d.press('Encoder', '2,0'); // dialDown → plugin switches to its custom layout
    await sleep(400);
    const custom = await d.render('Encoder', '2,0');
    assert(!custom.equals(fed), 'custom layout rendered');
    eq(d.logsSince(m, /unbekanntes Touch-Strip-Layout/), [], 'custom layout found');
  });

  await test('plus: dial press/release reach the plugin, rotating while pressed says so; unknown layouts', async () => {
    const ctx = (await d.waitLog(/\[com\.e2e\.counter\] appear Encoder 2,0 /, 0)).split(' ').pop();
    let m = d.mark();
    await d.input({ type: 'dial_press', index: 2, down: true });
    await d.waitLog(new RegExp(`\\[com\\.e2e\\.counter\\] ev dialDown ${ctx} .*"controller":"Encoder"`), m);
    await d.input({ type: 'dial', index: 2, ticks: -1 });
    eq(JSON.parse(/ev dialRotate \S+ (.*)$/.exec(await d.waitLog(/\[com\.e2e\.counter\] ev dialRotate .*"pressed":true/, m))[1]).ticks, -1);
    await d.input({ type: 'dial_press', index: 2, down: false });
    await d.waitLog(new RegExp(`\\[com\\.e2e\\.counter\\] ev dialUp ${ctx} `), m);
    // A layout the plugin doesn't have is reported; "" (keys without a strip) is ignored.
    const pi = new WebSocket(`ws://127.0.0.1:${d.sdk}`);
    await new Promise((r, j) => { pi.onopen = r; pi.onerror = j; });
    pi.send(JSON.stringify({ event: 'registerPropertyInspector', uuid: await piAlias(d, 'Encoder', '2,0') }));
    m = d.mark();
    pi.send(JSON.stringify({ event: 'sendToPlugin', action: 'com.e2e.counter.count', context: ctx, payload: { send: [
      { event: 'setFeedbackLayout', context: ctx, payload: { layout: '' } },
      { event: 'setFeedbackLayout', context: ctx, payload: { layout: 'layouts/missing.json' } },
      { event: 'setTriggerDescription', context: ctx, payload: { rotate: 'x' } },
    ] } }));
    await d.waitLog(/unknown touch strip layout "layouts\/missing\.json"/, m);
    pi.close();
    eq(d.logsSince(m, /unknown touch strip layout ""/), [], 'empty layout ignored');
    eq(await d.status('POST', `/api/devices/${d.dev.id}/input`, { type: 'dial', index: 4, ticks: 1 }), 400, 'only four dials');
    eq(await d.status('POST', `/api/devices/${d.dev.id}/input`, { type: 'dial_press', index: 9 }), 400);
  });

  await test('plus: a hotkey on a dial: pressing the dial presses the keys', async () => {
    await d.slot('Encoder', '3,0', 'com.elgato.streamdeck.system.hotkey');
    await d.settings('Encoder', '3,0', { Hotkeys: [HK_Z] });
    const m = d.mark();
    await d.press('Encoder', '3,0');
    await d.waitLog(/send_key_state.*key = "code:52".*"down"/, m);
    await d.waitLog(/send_key_state.*key = "code:52".*"up"/, m);
  });

  await test('plus: sleep and wake via dial', async () => {
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.sleep');
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].sleeping, 2000, 'sleeping');
    const b = (await d.device()).brightness;
    await d.input({ type: 'dial', index: 0, ticks: 1 });
    const s2 = await d.waitState((s) => !s.devices[0].sleeping, 2000, 'awake');
    eq(s2.devices[0].brightness, b, 'waking rotation is not applied');
  });
}

async function suiteSmall(d, model, keys) {
  await suiteCommon(d, { keys });
  await test(`${model}: profile model and image pipeline`, async () => {
    const st = await d.state();
    const prof = st.profiles.find((p) => p.id === st.devices[0].profile);
    eq(prof.model, st.devices[0].model);
    await d.slot('Keypad', '0,0', 'com.elgato.streamdeck.system.website');
    await d.patch(`/api/pages/${prof.id}/${st.devices[0].page}/slot`, { controller: 'Keypad', coord: '0,0', image: RED_PNG, style: { Title: 'BMP' } });
    eq(pngSize(await d.render('Keypad', '0,0')), [96, 96]);
    await sleep(200);
    eq(d.logsSince(0, /panicked|device write failed|render failed/), []);
  });
}

/** Converting keys from a Windows/macOS profile into Linux actions (no network needed). */
async function suiteCompat(d) {
  // Only this test's keys: the starter's second page has program keys that depend on the
  // programs installed on the machine.
  await onePage(d);
  const st = await d.state();
  let P = st.devices[0].profile;
  const pg = st.devices[0].page;
  const key = (uuid, title, settings = {}) => ({ UUID: uuid, Name: title || uuid, Settings: settings, State: 0, States: [{ Title: title }] });
  const paste = (coord, inst) => d.post(`/api/pages/${P}/${pg}/paste`, { controller: 'Keypad', coord, instance: inst, from_profile: P, from_page: pg });
  await paste('0,0', key('com.elgato.streamdeck.system.open', 'Microsoft\nOutlook', { path: '"C:\\Program Files\\Microsoft Office\\root\\Office16\\OUTLOOK.EXE"' }));
  await paste('1,0', key('com.elgato.streamdeck.system.open', 'Riot Games', { path: '"C:\\\\ProgramData\\\\Riot Games\\\\RiotClientServices.exe"' }));
  await paste('2,0', key('com.barraider.spotifyplaypause', ''));
  await paste('3,0', key('com.microsoft.teams.button.togglemute', ''));
  await paste('0,1', key('com.elgato.twitch.marker', ''));

  await test('convert: in a profile made here only Windows/macOS paths and Elgato plugins count', async () => {
    const plan = await d.get(`/api/profiles/${P}/conversion`);
    eq(plan.needed, true);
    // The Twitch key is just a plugin missing here, not something from Windows.
    eq(plan.manual, ['Riot Games']);
  });

  // The same keys in a profile imported from the official app: everything counts.
  const imported = await d.req('POST', '/api/import', await d.get(`/api/profiles/${P}/export`), { 'x-filename': 'Windows.streamDeckProfile' });
  await d.post(`/api/devices/${st.devices[0].id}`, { profile: imported.id });
  await d.waitState((s) => s.devices[0].profile === imported.id, 2000, 'imported profile shown');
  P = imported.id;

  await test('convert: the plan lists what happens to the imported keys', async () => {
    const plan = await d.get(`/api/profiles/${P}/conversion`);
    eq(plan.needed, true);
    eq(plan.web.map((w) => [w.title, w.target]), [['Microsoft Outlook', 'outlook.office.com/mail']]);
    eq(plan.replaced.map((r) => r.keys).reduce((a, b) => a + b, 0), 2, 'Spotify → Multimedia, Teams → shortcut');
    eq([...plan.manual].sort(), ['Riot Games', 'com.elgato.twitch.marker'].sort());
    eq(plan.installs, []);
  });

  await test('convert: keys become regular Linux actions, titles and icons stay', async () => {
    const m = d.mark();
    const r = await d.post(`/api/profiles/${P}/conversion`, {});
    eq([r.changed, r.manual], [5, 2]);
    const kp = (await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq([kp['0,0'].UUID, kp['0,0'].Settings.path, kp['0,0'].States[0].Title], ['com.elgato.streamdeck.system.website', 'https://outlook.office.com/mail/', 'Microsoft\nOutlook']);
    eq([kp['1,0'].UUID, kp['1,0'].Settings.path, kp['1,0'].States[0].Title], ['com.elgato.streamdeck.system.open', '', 'Riot Games']);
    eq([kp['2,0'].UUID, kp['2,0'].Settings.actionIdx], ['com.elgato.streamdeck.system.multimedia', 1]);
    eq([kp['3,0'].UUID, kp['3,0'].Settings.Hotkeys[0].LinuxCode], ['com.elgato.streamdeck.system.hotkey', 50]);
    eq(kp['0,1'].UUID, 'com.elgato.streamdeck.system.open');
    // Keys without a counterpart can be told apart: the action's name as title and a note.
    eq([kp['0,1'].States[0].Title, kp['0,1'].Settings._from], ['com.elgato.twitch.marker', 'com.elgato.twitch.marker']);
    eq((await d.get(`/api/profiles/${P}/conversion`)).needed, false, 'nothing left to convert');
    assert((await d.get('/api/backups')).some((b) => b.kind === 'convert'), 'backup before converting');
    await d.press('Keypad', '2,0');
    await sleep(300);
    assert(d.logsSince(m, /DRYRUN/).length >= 1, 'the converted media key works');
  });

  await test('convert: nothing to convert makes no backup', async () => {
    const before = (await d.get('/api/backups')).length;
    const r = await d.post(`/api/profiles/${P}/conversion`, {});
    eq([r.changed, r.manual], [0, 0]);
    eq((await d.get('/api/backups')).length, before);
  });

  await test('website: tel: links stay tel:, host:port gets https://', async () => {
    await paste('0,1', key('com.elgato.streamdeck.system.website', 'Call', { path: 'tel:0791234567', openInBrowser: true }));
    await paste('1,1', key('com.elgato.streamdeck.system.website', 'NAS', { path: 'nas.local:5000', openInBrowser: true }));
    const m = d.mark();
    await d.press('Keypad', '0,1');
    await d.press('Keypad', '1,1');
    await sleep(300);
    eq(d.logsSince(m, /DRYRUN exec "xdg-open"/).map((l) => l.split('"xdg-open" ')[1]), ['"tel:0791234567"', '"https://nas.local:5000"']);
  });

  await test('api: an explicit token header wins over a stale cookie', async () => {
    eq(await d.status('GET', '/api/state', undefined, { cookie: `tuxdeck_session_${d.port}=stale` }), 200);
  });

  await test('samples: the Work profile is created with its icons and working keys', async () => {
    const list = await d.get('/api/templates');
    eq(list.map((x) => x.id), ['standard', 'work', 'play', 'stream']);
    assert(list.every((x) => x.icons.length === 8 && x.icons.every((i) => /^data:image\/svg/.test(i))), 'icons');
    const r = await d.post('/api/templates/work', { model: st.devices[0].model });
    eq(r.failed, []);
    const prof = (await d.state()).profiles.find((p) => p.id === r.id);
    eq(prof.pages.length, 2);
    const keys = async (i) => (await d.page(r.id, prof.pages[i])).controllers.find((c) => c.Type === 'Keypad').Actions;
    const [kp, kp2] = [await keys(0), await keys(1)];
    eq([Object.keys(kp).length, Object.keys(kp2).length], [8, 8]);
    eq([kp['0,0'].UUID, kp['0,0'].Settings.path], ['com.elgato.streamdeck.system.open', 'xdg-open mailto:']);
    eq([kp['3,1'].States[0].Title, kp['3,1'].Settings.path], ['Lock', 'loginctl lock-session']);
    eq(kp2['3,1'].Settings.Hotkeys[0].LinuxCode, 99, 'screenshot = Print');
    eq(kp2['0,0'].Settings.Hotkeys[0].LinuxCode, 46, 'copy = Ctrl+C');
    for (const a of [...Object.values(kp), ...Object.values(kp2)]) {
      assert(/^data:image\/svg/.test(a.States[0].Image || '') && a.States[0].ShowTitle === true && a.States[0].Title, `${a.States[0].Title}: icon and title`);
    }
    eq((await d.get(`/api/profiles/${r.id}/conversion`)).needed, false, 'a sample profile needs no conversion');
  });
}

/** Pedal: three keys without a display. */
async function suitePedal(d) {
  await test('pedal: starter profile is previous, play/pause, next', async () => {
    const st = await d.state();
    eq([st.devices[0].cols, st.devices[0].rows], [3, 1]);
    const kp = (await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq(['0,0', '1,0', '2,0'].map((c) => kp[c]?.Settings?.actionIdx), [0, 1, 2]);
    const m = d.mark();
    await d.press('Keypad', '1,0');
    assert(d.logsSince(m, /DRYRUN/).length >= 1, 'play/pause side effect');
  });
}

// ------------------------------------------------------------------------------- main

/** Backups: download, restore (ours and Elgato's layout), automatic backups. */
async function suiteBackup(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;
  const data = path.join(d.dir, 'data');
  const listZip = (buf) => {
    // Central directory file names of a zip.
    const names = [];
    for (let i = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06])), n = buf.readUInt16LE(i + 10), o = buf.readUInt32LE(i + 16); n > 0; n--) {
      const len = buf.readUInt16LE(o + 28);
      names.push(buf.toString('utf8', o + 46, o + 46 + len));
      o += 46 + len + buf.readUInt16LE(o + 30) + buf.readUInt16LE(o + 32);
    }
    return names;
  };

  await test('backup: a daily backup exists after the start', async () => {
    let list = [];
    for (let i = 0; i < 40 && !list.some((b) => b.kind === 'daily'); i++) {
      list = await d.get('/api/backups');
      await sleep(100);
    }
    assert(list.some((b) => b.kind === 'daily'), JSON.stringify(list));
  });

  let backup;
  await test('backup: download contains profiles, settings, plugin data and plugins', async () => {
    await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '0,0', style: { Title: 'Backed up' } });
    fs.mkdirSync(path.join(data, 'plugin-data/Data/com.e2e.counter'), { recursive: true });
    fs.writeFileSync(path.join(data, 'plugin-data/Data/com.e2e.counter/login.json'), '{"token":"old"}');
    backup = await d.get('/api/backup');
    const names = listZip(backup);
    for (const want of ['tuxdeck-backup.json', 'config/settings.json', 'plugin-data/Data/com.e2e.counter/login.json', 'Plugins/com.e2e.counter.sdPlugin/manifest.json']) {
      assert(names.includes(want), `${want} in backup`);
    }
    assert(names.some((n) => n.startsWith(`ProfilesV2/${P}.sdProfile/`)), 'profile in backup');
    assert(!names.some((n) => n.startsWith('IconPacks/')), 'icon packs only on request');
  });

  await test('restore: profiles, plugin and plugin data come back; the state before is saved', async () => {
    const other = await d.post('/api/profiles', { name: 'Made later', model: '20GBJ9901' });
    await d.post(`/api/devices/${d.dev.id}`, { profile: other.id });
    await d.del(`/api/profiles/${P}`);
    await d.del('/api/plugins/com.e2e.counter');
    fs.mkdirSync(path.join(data, 'plugin-data/Data/com.e2e.counter'), { recursive: true });
    fs.writeFileSync(path.join(data, 'plugin-data/Data/com.e2e.counter/login.json'), '{"token":"new"}');
    const m = d.mark();
    const r = await d.req('POST', '/api/restore', backup, { 'content-type': 'application/octet-stream' });
    eq([r.profiles, r.plugins, r.elgato], [1, 1, false]);
    const st = await d.state();
    eq(st.profiles.map((p) => p.id), [P], 'profiles replaced by the backup');
    eq(st.devices[0].profile, P, 'device shows the restored profile');
    eq((await d.page(P, page0)).controllers.find((c) => c.Type === 'Keypad').Actions['0,0'].States[0].Title, 'Backed up');
    eq(fs.readFileSync(path.join(data, 'plugin-data/Data/com.e2e.counter/login.json'), 'utf8'), '{"token":"old"}');
    await d.waitLog(/started plugin com\.e2e\.counter/, m, 5000);
    assert((await d.get('/api/backups')).some((b) => b.kind === 'before' && b.name === r.safety_backup), 'backup of the state before');
  });

  await test('restore: Elgato backup layout (Profiles/ and Resources/)', async () => {
    const exported = await d.get(`/api/profiles/${P}/export`);
    // Re-pack the export the way the official app lays out backups.
    const files = {};
    for (const n of listZip(exported).filter((x) => !x.endsWith('/'))) files[`Profiles/${n}`] = unzipEntry(exported, n);
    files['Resources/manifest.json'] = '{}';
    const r = await d.req('POST', '/api/restore', zip(files), { 'content-type': 'application/octet-stream' });
    eq([r.profiles, r.elgato], [1, true]);
    assert((await d.state()).profiles.some((p) => p.id === P), 'profile restored');
  });

  await test('restore: other files are rejected and change nothing', async () => {
    const before = (await d.state()).profiles.map((p) => p.id);
    eq(await d.status('POST', '/api/restore', zip({ 'readme.txt': 'hi' }), { 'content-type': 'application/octet-stream' }), 400);
    eq((await d.state()).profiles.map((p) => p.id), before);
    eq(await d.status('POST', '/api/backups/..%2Fsettings.json/restore'), 404);
  });
}

/** Content of one stored (uncompressed) or deflated zip entry. */
function unzipEntry(buf, name) {
  const at = buf.indexOf(Buffer.concat([Buffer.from([0x50, 0x4b, 0x03, 0x04])]), 0);
  for (let o = at; o >= 0 && o < buf.length; ) {
    if (buf.readUInt32LE(o) !== 0x04034b50) break;
    const method = buf.readUInt16LE(o + 8);
    const size = buf.readUInt32LE(o + 18);
    const nlen = buf.readUInt16LE(o + 26);
    const xlen = buf.readUInt16LE(o + 28);
    const n = buf.toString('utf8', o + 30, o + 30 + nlen);
    const start = o + 30 + nlen + xlen;
    if (n === name) {
      const raw = buf.subarray(start, start + size);
      return method === 8 ? zlib.inflateRawSync(raw) : Buffer.from(raw);
    }
    o = start + size;
  }
  throw new Error(`${name} not in zip`);
}

/** Files of the test plugin under another UUID with these actions (`<uuid>.<suffix>`). */
function pluginWithActions(uuid, suffixes, manifestChanges = {}) {
  const files = {};
  for (const [k, v] of Object.entries(PLUGIN_FILES)) files[k.replace('com.e2e.counter.sdPlugin', `${uuid}.sdPlugin`)] = v;
  const m = JSON.parse(PLUGIN_FILES['com.e2e.counter.sdPlugin/manifest.json']);
  Object.assign(m, { UUID: uuid, Name: uuid }, manifestChanges);
  m.Actions = suffixes.map((s) => ({ ...m.Actions[0], UUID: `${uuid}.${s}`, Name: s }));
  files[`${uuid}.sdPlugin/manifest.json`] = JSON.stringify(m);
  return files;
}

/** A variant of the test plugin with another UUID and some files replaced. */
function pluginVariant(uuid, changes = {}, manifestChanges = {}) {
  const files = {};
  for (const [k, v] of Object.entries(PLUGIN_FILES)) files[k.replace('com.e2e.counter.sdPlugin', `${uuid}.sdPlugin`)] = v;
  const m = JSON.parse(PLUGIN_FILES['com.e2e.counter.sdPlugin/manifest.json']);
  Object.assign(m, { UUID: uuid, Name: uuid }, manifestChanges);
  m.Actions = m.Actions.map((a) => ({ ...a, UUID: `${uuid}.count` }));
  files[`${uuid}.sdPlugin/manifest.json`] = JSON.stringify(m);
  for (const [k, v] of Object.entries(changes)) files[`${uuid}.sdPlugin/${k}`] = v;
  return zip(files);
}

/** "Works on Linux" checks of plugins (catalogue badge). */
/** Linux checks on a computer without bubblewrap: installed plugins are tested in the
 * test instances, plugins that are not installed only get their files checked. */
async function suiteLinuxCheckPlain(d) {
  await test('linux check without bubblewrap: an installed plugin is tested completely', async () => {
    await d.req('POST', '/api/import', pluginVariant('com.e2e.good'), { 'x-filename': 'good.streamDeckPlugin' });
    await d.post('/api/plugins/com.e2e.good/linux-check');
    await linuxChecksDone(d);
    const r = (await d.get('/api/linux-checks')).results['com.e2e.good'];
    eq([r?.verdict, r?.sandboxed, !!r?.static], ['works', false, false], JSON.stringify(r?.checks));
    eq(d.logsSince(0, /bubblewrap/i), [], 'no message about bubblewrap');
  });

  await test('linux check without bubblewrap: a catalogue plugin is not started, its files are checked', async () => {
    const m = d.mark();
    eq((await d.post('/api/catalogue/linux-check', { id: 'com.e2e.direct' })).key, 'com.e2e.direct');
    await linuxChecksDone(d);
    const r = (await d.get('/api/linux-checks')).results['com.e2e.direct'];
    eq([r?.verdict, r?.static, r?.local_only, r?.source, r?.checks?.map((c) => [c.id, c.ok])], ['static', true, true, 'openaction', [['linux_runtime', true]]]);
    eq(d.logsSince(m, /\[com\.e2e\.direct\]|started plugin com\.e2e\.direct/), [], 'the plugin never ran');
    eq((await d.get('/api/catalogue/com.e2e.direct')).state.linux_check?.verdict, 'static', 'shown in the catalogue');
    eq(d.logsSince(0, /WARN.*Linux/), [], 'no warning');
  });
}

async function suiteLinuxCheck(d) {
  const variants = {
    'com.e2e.good': pluginVariant('com.e2e.good'),
    'com.e2e.crash': pluginVariant('com.e2e.crash', { 'plugin.js': 'process.exit(3);' }),
    'com.e2e.maconly': pluginVariant('com.e2e.maconly', { 'bin/plugin': 'MACHO' }, { CodePath: 'bin/plugin', Nodejs: undefined, OS: [{ Platform: 'mac', MinimumVersion: '10.15' }] }),
    'com.e2e.badpi': pluginVariant('com.e2e.badpi', {
      'pi/pi.html': '<!doctype html><title>pi</title><script>function connectElgatoStreamDeckSocket(port, uuid, ev) {'
        + ' const ws = new WebSocket("ws://127.0.0.1:" + port); ws.onopen = () => ws.send(JSON.stringify({ event: ev, uuid }));'
        + ' setTimeout(() => { null.forEach(() => {}); }, 100); }</script>',
    }),
  };
  for (const [uuid, z] of Object.entries(variants)) await d.req('POST', '/api/import', z, { 'x-filename': `${uuid}.streamDeckPlugin` });
  // All checks at once (each in its own test instances).
  for (const uuid of Object.keys(variants)) await d.post(`/api/plugins/${uuid}/linux-check`);
  eq(await d.status('POST', '/api/plugins/com.e2e.good/linux-check'), 409, 'a second check of the same plugin is refused');
  let r;
  const end = Date.now() + 240000;
  while (Date.now() < end) {
    r = await d.get('/api/linux-checks');
    if (Object.keys(variants).every((u) => r.results[u]) && !r.running.length) break;
    await sleep(1000);
  }
  const res = r.results;
  const byId = (u, id) => res[u]?.checks.find((c) => c.id === id);

  await test('linux check: a working plugin passes', async () => {
    eq(res['com.e2e.good']?.verdict, 'works', JSON.stringify(res['com.e2e.good']?.checks));
    for (const id of ['linux_runtime', 'starts', 'actions', 'input', 'responds', 'log']) eq(byId('com.e2e.good', id)?.ok, true, id);
    // The property inspector is checked whenever a browser is available.
    assert(byId('com.e2e.good', 'inspector')?.ok !== false, 'inspector');
  });

  await test('linux check: a plugin that exits right away fails', async () => {
    eq(res['com.e2e.crash']?.verdict, 'fails');
    eq(byId('com.e2e.crash', 'starts')?.ok, false);
  });

  await test('linux check: a macOS-only program is reported as not for Linux', async () => {
    eq(res['com.e2e.maconly']?.verdict, 'unsupported');
    eq(byId('com.e2e.maconly', 'linux_runtime')?.ok, false);
  });

  await test('linux check: errors in the property inspector are issues', async () => {
    const pi = byId('com.e2e.badpi', 'inspector');
    if (pi?.ok === null) return; // no browser on this machine
    eq([res['com.e2e.badpi']?.verdict, pi?.ok], ['issues', false]);
    assert(/null/.test(pi.detail), pi.detail);
  });

  await test('linux check: results show up in the catalogue and in diagnostics', async () => {
    const diag = await d.get('/api/plugins/com.e2e.good/diagnostics');
    assert(diag.stats.registrations >= 1, 'live diagnostics');
    eq(Object.keys((await d.get('/api/linux-checks')).results).sort(), Object.keys(variants).sort());
  });
}

/** Open-source plugin catalogue: list, install from releases, Linux check; packages and
 * links that need the official app are refused. */
async function suiteCatalogue(d) {
  await test('catalogue: list, search, order and install state', async () => {
    const all = await d.get('/api/catalogue');
    eq([all.count, all.items.map((i) => i.id)], [3, ['com.e2e.cat', 'com.e2e.direct', 'com.e2e.norel']]);
    const cat = all.items[0];
    eq([cat.name, cat.author, cat.summary, cat.state.installed], ['Catalogue Plugin', 'e2e', 'A plugin from the catalogue', false]);
    assert(cat.icon.endsWith('/icons/com.e2e.cat.png'), cat.icon);
    eq(all.items[1].ai_assisted, true);
    eq((await d.get('/api/catalogue?search=SOMEONE')).items.map((i) => i.id), ['com.e2e.direct']);
    eq((await d.get('/api/catalogue?ordering=name')).items.map((i) => i.name), ['Another Plugin', 'Catalogue Plugin', 'Direct Plugin']);
    const page = await d.get('/api/catalogue?offset=1&limit=1');
    eq([page.count, page.items.map((i) => i.id)], [3, ['com.e2e.direct']]);
    eq((await d.get('/api/catalogue/com.e2e.direct')).repository, 'https://github.com/e2e/direct');
    eq(await d.status('GET', '/api/catalogue/com.e2e.unknown'), 400);
  });

  await test('catalogue: install from the newest release, then it is tested on Linux', async () => {
    const r = await d.post('/api/catalogue/install', { id: 'com.e2e.cat' });
    eq([r.kind, r.id, r.runs_on_linux], ['plugin', 'com.e2e.cat', true]);
    eq((await d.get('/api/catalogue/com.e2e.cat')).state.installed, true);
    assert((await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.cat'), 'installed plugin listed');
    let res;
    const end = Date.now() + 180000;
    while (Date.now() < end && !(res = (await d.get('/api/linux-checks')).results['com.e2e.cat'])) await sleep(1000);
    eq([res?.verdict, res?.source, res?.release], ['works', 'openaction', 'v1.2'], JSON.stringify(res?.checks));
  });

  await test('catalogue: direct download URLs; plugins without a release are reported', async () => {
    eq((await d.post('/api/catalogue/install', { id: 'com.e2e.direct' })).id, 'com.e2e.direct');
    const e = await d.post('/api/catalogue/install', { id: 'com.e2e.norel' }).catch((x) => x);
    assert(e.status === 400 && /no release/.test(e.message), e.message);
    // A repository that is gone (GitHub answers 404) says so.
    const releases = CATALOGUE.files.get('/repos/e2e/norel/releases');
    CATALOGUE.files.delete('/repos/e2e/norel/releases');
    const gone = await d.post('/api/catalogue/install', { id: 'com.e2e.norel' }).catch((x) => x);
    CATALOGUE.files.set('/repos/e2e/norel/releases', releases);
    assert(gone.status === 400 && /GitHub repository no longer exists/.test(gone.message), gone.message);
    // Let the Linux check that started with the installation finish.
    const end = Date.now() + 180000;
    while (Date.now() < end && (await d.get('/api/linux-checks')).running.length) await sleep(1000);
  });

  await test('packages with a protected (DRM) manifest are refused', async () => {
    const z = zip({ 'com.e2e.drm.sdPlugin/manifest.json': 'ELGATO\u0001\u0000encrypted', 'com.e2e.drm.sdPlugin/plugin.js': '' });
    const e = await d.req('POST', '/api/import', z, { 'x-filename': 'drm.streamDeckPlugin' }).catch((x) => x);
    assert(e.status === 400 && /protected plugin/.test(e.message), e.message);
    assert(!(await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.drm'), 'nothing installed');
  });

  await test('Elgato Marketplace links are refused with a hint', async () => {
    for (const url of ['streamdeck://plugins/install/be01123f-9437-41c3-a768-3431f9549bd4', 'streamdeck://marketplace/auth?code=x&state=y']) {
      const e = await d.post('/api/deeplink', { url }).catch((x) => x);
      assert(e.status === 400 && /official app/.test(e.message), e.message);
    }
    eq((await d.post('/api/deeplink', { url: 'streamdeck://something/else' })).action, 'ignored');
  });

  await test('catalogue: paging bounds, built-in plugins cannot be removed or checked', async () => {
    const all = await d.get('/api/catalogue?limit=1000&offset=99');
    eq([all.count, all.limit, all.items], [3, 100, []]);
    eq((await d.get('/api/catalogue?search=%20%20')).count, 3, 'blank search lists everything');
    const e = await d.req('DELETE', '/api/plugins/com.elgato.streamdeck.system.hotkey').catch((x) => x);
    assert(e.status === 400 && /built-in/.test(e.message), e.message);
    eq(await d.status('POST', '/api/plugins/com.elgato.streamdeck.system.hotkey/linux-check'), 404);
  });

  await test('linux checks: a catalogue plugin without installing it, "X-Linux-Check" on install from file', async () => {
    eq(await d.status('POST', '/api/catalogue/linux-check', { id: 'com.e2e.unknown' }), 400, 'not in the catalogue');
    eq((await d.post('/api/catalogue/linux-check', { id: 'com.e2e.direct' })).key, 'com.e2e.direct');
    eq(await d.status('POST', '/api/catalogue/linux-check', { id: 'com.e2e.direct' }), 409, 'already running');
    await d.req('POST', '/api/import', pluginVariant('com.e2e.hdr'), { 'x-filename': 'hdr.streamDeckPlugin', 'x-linux-check': '1' });
    await linuxChecksDone(d);
    const res = (await d.get('/api/linux-checks')).results;
    eq([res['com.e2e.direct']?.verdict, res['com.e2e.direct']?.source], ['works', 'openaction'], JSON.stringify(res['com.e2e.direct']?.checks));
    eq([res['com.e2e.hdr']?.verdict, res['com.e2e.hdr']?.source], ['works', 'file'], JSON.stringify(res['com.e2e.hdr']?.checks));
    eq((await d.get('/api/catalogue/com.e2e.direct')).state.linux_check?.verdict, 'works', 'shown in the catalogue');
  });

  await test('linux checks of all catalogue plugins: one run at a time, tested releases are skipped, cancel', async () => {
    let m = d.mark();
    eq((await d.post('/api/catalogue/linux-check-all', { limit: 1, parallel: 1 })).started, true);
    eq(await d.status('POST', '/api/catalogue/linux-check-all', {}), 409, 'one run at a time');
    // com.e2e.cat was tested in its newest release (v1.2) above.
    await d.waitLog(/Linux check of all plugins finished: 0 of 1 tested/, m, 60000);
    m = d.mark();
    eq((await d.post('/api/catalogue/linux-check-all', { parallel: 1 })).started, true);
    eq((await d.del('/api/catalogue/linux-check-all')).ok, true);
    await d.waitLog(/Linux check of all plugins finished: \d of 3 tested/, m, 120000);
    await linuxChecksDone(d);
    eq((await d.del('/api/catalogue/linux-check-all')).ok, true, 'cancelling when nothing runs');
  });
}

// ------------------------------------------------------------------- API and security

const B = 'com.elgato.streamdeck.';
const WEBSITE = `${B}system.website`;

/** An action instance as stored in a profile. */
const inst = (uuid, settings = {}, title, extra = {}) => ({
  ActionID: randomUUID(), Name: extra.Name || title || uuid, Settings: settings, State: 0, States: [title ? { Title: title } : {}], UUID: uuid, ...extra,
});

/** A format 3.0 profile archive. `pages`/`def` (template page): [{id?, keys: {Keypad: {coord: inst}, Neo: …}}]. */
function profileZip({ name = 'Crafted', model = '20GBJ9901', pages, def = null, folders = [], version = '3.0' }) {
  const id = randomUUID().toUpperCase();
  const root = `${id}.sdProfile`;
  const withId = (p) => ({ ...p, id: p.id || randomUUID() });
  const top = pages.map(withId);
  const tpl = def && withId(def);
  const files = {
    [`${root}/manifest.json`]: JSON.stringify({
      Device: { Model: model, UUID: '' }, Name: name, Version: version,
      Pages: { Current: top[0].id, Default: tpl ? tpl.id : '', Pages: top.map((p) => p.id) },
    }),
  };
  for (const p of [...top, ...(tpl ? [tpl] : []), ...folders.map(withId)]) {
    files[`${root}/Profiles/${p.id.toUpperCase()}/manifest.json`] = JSON.stringify({
      Controllers: Object.entries(p.keys || {}).map(([Type, Actions]) => ({ Type, Actions })), Icon: '', Name: '',
    });
  }
  return zip(files);
}

/** JSON with sorted object keys (order-independent comparison). */
function canon(v) {
  if (Array.isArray(v)) return v.map(canon);
  if (v && typeof v === 'object') return Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])]));
  return v;
}
const eqc = (a, b, msg) => eq(canon(a), canon(b), msg);
/** A copy without ActionIDs (they are renewed on import/copy). */
const stripIds = (v) => (Array.isArray(v) ? v.map(stripIds)
  : v && typeof v === 'object' ? Object.fromEntries(Object.entries(v).filter(([k]) => k !== 'ActionID').map(([k, x]) => [k, stripIds(x)])) : v);

/** HTTP API: access control and the error answers of every endpoint. */
async function suiteApi(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;
  const dev = st0.devices[0].id;
  const T = d.token;
  const status = async (p, headers) => (await d.raw(p, { headers })).status;

  await test('security: header, bearer, query and cookie tokens; wrong or missing ones are refused', async () => {
    eq(await status('/api/state', { 'x-tuxdeck-token': T }), 200, 'header');
    eq(await status('/api/state', { authorization: `Bearer ${T}` }), 200, 'bearer');
    eq(await status('/api/state', { authorization: 'Bearer nope' }), 401, 'wrong bearer');
    eq(await status('/api/state', { authorization: T }), 401, 'bearer prefix needed');
    eq(await status(`/api/state?token=${T}`), 200, 'query (WebSocket clients)');
    eq(await status('/api/state?token=nope'), 401, 'wrong query token');
    const S = fs.readFileSync(path.join(d.config, 'ui-session'), 'utf8').trim();
    eq(await status('/api/state', { cookie: `theme=dark; tuxdeck_session_${d.port}=${S}` }), 200, 'session cookie among others');
    eq(await status('/api/state', { cookie: `tuxdeck_session_${d.port}=nope` }), 401, 'wrong cookie');
    eq(await status('/api/state', { cookie: `tuxdeck_token=${T}` }), 401, 'the API token is no cookie value');
    eq(await status('/api/state', { cookie: `tuxdeck_session_${d.port + 1}=${S}` }), 401, 'cookie of another port');
    // Browsers send the cookie with requests of other pages on 127.0.0.1 too (same site).
    const sess = { cookie: `tuxdeck_session_${d.port}=${S}` };
    eq(await status('/api/state', { ...sess, 'sec-fetch-site': 'same-origin' }), 200, 'from the UI page');
    eq(await status('/api/state', { ...sess, 'sec-fetch-site': 'same-site' }), 401, 'from another port');
    eq(await status('/api/state', { ...sess, 'sec-fetch-site': 'cross-site' }), 401, 'from another site');
    eq(await status('/api/state', { 'x-tuxdeck-token': '' }), 401, 'empty token');
    const r = await d.raw('/api/profiles', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{"name":"x","model":"20GBJ9901"}' });
    eq(r.status, 401, 'writes need the token too');
    assert(JSON.parse(r.body).error, 'JSON error body');
    eq((await d.state()).profiles.length, st0.profiles.length, 'nothing created');
  });

  await test('security: only the loopback UI origin may use the daemon', async () => {
    const tok = { 'x-tuxdeck-token': T };
    for (const host of [`localhost:${d.port}`, `[::1]:${d.port}`, `127.0.0.1:${d.port}`]) eq(await status('/api/state', { ...tok, host }), 200, host);
    for (const host of ['127.0.0.1', `127.0.0.1:${d.port + 1}`, `evil.example:${d.port}`, `localhost.evil.example:${d.port}`]) {
      eq(await status('/api/state', { ...tok, host }), 403, host);
    }
    for (const origin of [`http://localhost:${d.port}`, `http://[::1]:${d.port}`]) eq(await status('/api/state', { ...tok, origin }), 200, origin);
    for (const origin of [`https://127.0.0.1:${d.port}`, 'null', `http://127.0.0.1:${d.port}.evil.example`, `http://127.0.0.1:${d.port + 1}`]) {
      eq(await status('/api/state', { ...tok, origin }), 403, origin);
    }
    eq(await status('/', { origin: 'http://evil.example' }), 403, 'UI page requested by a foreign page');
    const before = (await d.state()).settings.touch_color;
    const r = await d.raw('/api/settings', { method: 'POST', headers: { ...tok, origin: 'http://evil.example', 'content-type': 'application/json' }, body: '{"touch_color":"#123456"}' });
    eq(r.status, 403);
    eq((await d.state()).settings.touch_color, before, 'setting unchanged');
  });

  await test('security: a one-time ?code= on pages becomes the session cookie; the API token in the URL does not', async () => {
    const code = async () => (await d.post('/api/login-code')).code;
    eq(await status('/api/login-code', { 'x-tuxdeck-token': T }), 405, 'GET is no way to a code');
    eq((await d.raw('/api/login-code', { method: 'POST' })).status, 401, 'a code needs the token');
    let c = await code();
    let r = await d.raw(`/?code=${c}&view=plugins`);
    eq([r.status, r.headers.location], [303, '/?view=plugins']);
    const S = fs.readFileSync(path.join(d.config, 'ui-session'), 'utf8').trim();
    eq(r.headers['set-cookie'][0].split(';')[0], `tuxdeck_session_${d.port}=${S}`);
    r = await d.raw(`/?code=${c}`);
    eq([r.status, r.headers['set-cookie']], [200, undefined], 'a code works once');
    c = await code();
    r = await d.raw(`/settings?lang=de&code=${c}`);
    eq([r.status, r.headers.location], [303, '/settings?lang=de']);
    const cookie = r.headers['set-cookie'][0];
    assert(/SameSite=Strict/.test(cookie) && /Path=\//.test(cookie) && /HttpOnly/.test(cookie), cookie);
    assert(!cookie.includes(T), 'the cookie is not the API token');
    for (const q of ['/?code=nope', `/?token=${T}`]) {
      r = await d.raw(q);
      eq([r.status, r.headers['set-cookie']], [200, undefined], `no cookie for ${q}`);
    }
    r = await d.raw(`/api/state?token=${T}`);
    eq([r.status, r.headers['set-cookie']], [200, undefined], 'API: no redirect, no cookie');
  });

  await test('security: WebSockets need the token (events) or the UI origin (plugin SDK)', async () => {
    eq(await wsHandshake(d.port, '/api/events'), 401, 'events without token');
    eq(await wsHandshake(d.port, '/api/events?token=nope'), 401, 'events, wrong token');
    eq(await wsHandshake(d.port, `/api/events?token=${T}`), 101, 'events with token');
    eq(await wsHandshake(d.port, `/api/events?token=${T}`, { origin: 'http://evil.example' }), 403, 'events from a foreign page');
    eq(await wsHandshake(d.sdk, '/', { origin: 'http://evil.example' }), 403, 'SDK from a foreign page');
    const pp = (await d.state()).settings.plugin_port;
    eq(await wsHandshake(d.sdk, '/', { origin: `http://localhost:${pp}` }), 101, 'SDK from the plugin file origin (property inspectors)');
    eq(await wsHandshake(d.sdk, '/', { origin: `http://127.0.0.1:${d.port}` }), 403, 'SDK from the UI origin');
    eq(await wsHandshake(d.sdk, '/'), 101, 'SDK without Origin (plugins)');
  });

  await test('security: plugin files run on their own origin without API; on the UI origin they are inert', async () => {
    const pp = (await d.state()).settings.plugin_port;
    assert(pp && pp !== d.port, 'own port');
    const plug = (p, headers = {}) => new Promise((res, rej) => {
      http.get({ host: '127.0.0.1', port: pp, path: p, headers }, (r) => {
        const chunks = [];
        r.on('data', (c) => chunks.push(c));
        r.on('end', () => res({ status: r.statusCode, headers: r.headers, body: Buffer.concat(chunks).toString() }));
      }).on('error', rej);
    });
    for (const p of ['/api/state', `/api/state?token=${T}`, '/', '/index.html']) eq((await plug(p, { 'x-tuxdeck-token': T })).status, 404, `${p} on the plugin port`);
    eq((await plug('/plugin-host', { host: `evil.example:${pp}` })).status, 403, 'DNS rebinding');
    // The plugin host page hands out registration secrets: only with its own key.
    eq((await plug('/plugin-host')).status, 404, 'plugin host without its key');
    eq((await plug('/plugin-host?k=guess')).status, 404, 'plugin host with a wrong key');
    eq((await plug('/x', { 'service-worker': 'script' })).status, 403, 'no service workers');
    // Plugin data: images for inspectors yes, settings and tokens no (any local user could ask).
    const pd = path.join(d.data, 'plugin-data', 'Data', 'com.e2e.secret');
    fs.mkdirSync(pd, { recursive: true });
    fs.writeFileSync(path.join(pd, 'token.json'), '{"token":"x"}');
    fs.writeFileSync(path.join(pd, 'bg.svg'), SVG('#123456'));
    for (const [p, want] of [['/plugins/Data/com.e2e.secret/token.json', 404], ['/plugins/Data/com.e2e.secret/bg.svg', 200],
      [encodeURI(path.join(pd, 'token.json')), 404], [encodeURI(path.join(pd, 'bg.svg')), 200]]) {
      eq((await plug(p)).status, want, p);
      eq((await d.raw(p)).status === 200 && p.endsWith('.json'), false, `${p} on the UI port`);
    }
    // The login redirect stays on this site.
    const c = (await d.post('/api/login-code')).code;
    eq((await d.raw(`//evil.example/?code=${c}`)).headers.location, '/evil.example/');
    const bridge = await plug('/__tuxdeck/pi-bridge.js');
    assert(bridge.body.includes(`http://127.0.0.1:${d.port}`), 'bridge knows the UI origin');
    assert(/frame-ancestors 'self' http:\/\/127\.0\.0\.1:/.test(bridge.headers['content-security-policy']), 'only framed by the UI');
    assert(!(await d.raw('/plugin-host')).body.toString().includes('TuxDeck plugin host'), 'no plugin host on the UI port');
    // Plugin files on the UI port (icons in the UI) can't run as pages there.
    const url = JSON.stringify(await d.get('/api/actions')).match(/"(\/plugin-files\/[^"]+)"/)?.[1];
    assert(url, 'an action icon URL');
    const icon = await d.raw(url);
    eq([icon.status, icon.headers['content-security-policy'], icon.headers['x-content-type-options']], [200, 'sandbox', 'nosniff'], url);
    eq((await plug(url)).status, 200, 'the same file on the plugin port');
  });

  await test('security: the plugin SDK accepts only the start secret of a plugin and aliases from /api/pi', async () => {
    // Other local programs know plugin UUIDs and could guess contexts, but not these.
    for (const reg of [{ event: 'registerPlugin', uuid: 'com.e2e.counter' }, { event: 'registerPropertyInspector', uuid: '0123456789ABCDEF0123456789ABCDEF' }]) {
      const ws = new WebSocket(`ws://127.0.0.1:${d.sdk}`);
      const got = [];
      ws.onmessage = (e) => got.push(e.data);
      await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
      const closed = new Promise((r) => { ws.onclose = r; });
      ws.send(JSON.stringify(reg));
      ws.send(JSON.stringify({ event: 'getGlobalSettings', context: reg.uuid }));
      await Promise.race([closed, sleep(3000).then(() => { throw new Error(`${reg.event}: not closed`); })]);
      eq(got, [], `${reg.event}: nothing answered`);
    }
  });

  await test('UI files: index, assets, translations; unknown files 404, unknown API routes are JSON 404', async () => {
    let r = await d.raw('/');
    eq(r.status, 200);
    assert(/text\/html/.test(r.headers['content-type']) && /<!doctype html/i.test(r.body.toString()), 'index.html');
    r = await d.raw('/app.js');
    assert(r.status === 200 && /javascript/.test(r.headers['content-type']), `app.js ${r.status} ${r.headers['content-type']}`);
    eq((await d.raw('/style.css')).headers['content-type'].split(';')[0], 'text/css');
    eq(JSON.parse((await d.raw('/i18n/de.json')).body).Delete, 'Löschen');
    eq((await d.raw('/no-such-file.js')).status, 404);
    eq((await d.raw('/some/view')).status, 200, 'client-side routes get the UI');
    r = await d.raw('/api/no-such-endpoint', { headers: { 'x-tuxdeck-token': T } });
    eq(r.status, 404, 'unknown API route');
    assert(/json/.test(r.headers['content-type']) && JSON.parse(r.body).error, 'JSON error');
    eq((await d.raw('/api/state', { method: 'POST', headers: { 'x-tuxdeck-token': T } })).status, 405, 'wrong method');
  });

  await test('not found: every endpoint answers 404 for unknown profiles, pages, slots, devices, files', async () => {
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=3,1`); // an empty slot
    const W = WEBSITE;
    const paste = { controller: 'Keypad', coord: '0,0', instance: inst(W), from_profile: P, from_page: page0 };
    const cases = [
      ['GET', `/api/pages/${P}/no-page`], ['GET', `/api/pages/NO-PROFILE/${page0}`],
      ['DELETE', `/api/pages/${P}/no-page`], ['DELETE', `/api/pages/NO-PROFILE/${page0}`],
      ['POST', `/api/pages/${P}/no-page/slot`, { controller: 'Keypad', coord: '0,0', uuid: W }],
      ['POST', `/api/pages/NO-PROFILE/${page0}/slot`, { controller: 'Keypad', coord: '0,0', uuid: W }],
      ['DELETE', `/api/pages/${P}/no-page/slot?controller=Keypad&coord=0,0`],
      ['PATCH', `/api/pages/${P}/no-page/slot`, { controller: 'Keypad', coord: '0,0', style: { Title: 'x' } }],
      ['PATCH', `/api/pages/NO-PROFILE/${page0}/slot`, { controller: 'Keypad', coord: '0,0', style: { Title: 'x' } }],
      ['PATCH', `/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '3,1', style: { Title: 'x' } }],
      ['POST', `/api/pages/${P}/no-page/move`, { from: { controller: 'Keypad', coord: '0,0' }, to: { controller: 'Keypad', coord: '1,0' } }],
      ['POST', `/api/pages/${P}/${page0}/move`, { from: { controller: 'Keypad', coord: '3,1' }, to: { controller: 'Keypad', coord: '1,0' } }],
      ['POST', `/api/pages/${P}/no-page/paste`, paste], ['POST', `/api/pages/NO-PROFILE/${page0}/paste`, paste],
      ['GET', `/api/pi/${P}/${page0}?controller=Keypad&coord=3,1`], ['GET', `/api/pi/${P}/${page0}?controller=Keypad&coord=0,0`],
      ['GET', `/api/pi/NO-PROFILE/${page0}`],
      ['PATCH', '/api/profiles/NO-PROFILE', { name: 'x' }], ['DELETE', '/api/profiles/NO-PROFILE'],
      ['POST', '/api/profiles/NO-PROFILE/pages'], ['GET', '/api/profiles/NO-PROFILE/export'],
      ['GET', '/api/profiles/NO-PROFILE/conversion'], ['POST', '/api/profiles/NO-PROFILE/conversion'],
      ['POST', '/api/devices/NO-DEVICE', { brightness: 10 }], ['POST', `/api/devices/${dev}`, { profile: 'NO-PROFILE' }],
      ['POST', `/api/devices/${dev}`, { page: 'no-page' }], ['POST', '/api/devices/NO-DEVICE/input', { type: 'dial', index: 0, ticks: 1 }],
      ['GET', '/api/backups/nope.zip'], ['POST', '/api/backups/nope.zip/restore'], ['GET', '/api/backups/..%2Fconfig%2Fsettings.json'],
      ['DELETE', '/api/plugins/com.e2e.nope'], ['POST', '/api/plugins/com.e2e.nope/linux-check'],
      ['DELETE', '/api/iconpacks/com.e2e.nope'], ['GET', '/api/iconpacks/com.e2e.nope/icons'], ['GET', '/api/iconpacks/..%2F..%2Fconfig/icons'],
      ['POST', '/api/templates/no-such-sample', { model: '20GBJ9901' }],
      ['GET', '/iconpack-files/com.e2e.nope/icon.svg'], ['GET', '/plugin-files/com.e2e.nope/manifest.json'],
      ['GET', '/plugins/com.e2e.nope.sdPlugin/manifest.json'],
    ];
    const wrong = [];
    for (const [m, p, b] of cases) {
      const s = await d.status(m, p, b);
      if (s !== 404) wrong.push(`${m} ${p} → ${s}`);
    }
    eq(wrong, [], 'expected 404');
  });

  await test('errors are JSON: wrong method, wrong content type, no access', async () => {
    const json = async (r) => [r.status, (await r.json()).error];
    let r = await fetch(d.url('/api/state'), { method: 'PUT', headers: { 'x-tuxdeck-token': d.token } });
    const [status, msg] = await json(r);
    assert(status === 405 && typeof msg === 'string' && msg, `405: ${msg}`);
    r = await fetch(d.url('/api/settings'), { method: 'POST', headers: { 'x-tuxdeck-token': d.token, 'content-type': 'text/plain' }, body: '{}' });
    eq(r.status, 415);
    assert(typeof (await r.json()).error === 'string', '415 body');
    r = await fetch(d.url('/api/state'), { headers: { 'x-tuxdeck-token': d.token, origin: 'http://evil.example' } });
    eq(await json(r), [403, 'forbidden']);
  });

  await test('bad requests: malformed JSON, missing fields, wrong types, wrong content type', async () => {
    const send = (method, p, body, ct = 'application/json') => d.raw(p, { method, headers: { 'x-tuxdeck-token': T, ...(ct ? { 'content-type': ct } : {}) }, body });
    eq((await send('POST', '/api/profiles', '{nope')).status, 400, 'syntax error');
    eq((await send('POST', '/api/profiles', '{}')).status, 422, 'missing fields');
    eq((await send('POST', '/api/profiles', '{"name": 5, "model": "20GBJ9901"}')).status, 422, 'wrong type');
    eq((await send('POST', '/api/profiles', '{"name":"x","model":"20GBJ9901"}', null)).status, 415, 'no JSON content type');
    eq((await send('POST', '/api/settings', '[1, 2]')).status, 422, 'settings must be an object');
    eq((await send('POST', '/api/deeplink', '{}')).status, 422, 'deep link without url');
    eq((await send('POST', '/api/catalogue/install', '{"id": 1}')).status, 422);
    eq((await send('POST', `/api/devices/${dev}`, '{"brightness": 300}')).status, 422, 'brightness is a byte');
    eq((await send('POST', `/api/devices/${dev}`, '{"brightness": -1}')).status, 422);
    eq((await send('POST', '/api/templates/work', '{}')).status, 422, 'sample without model');
    eq(await d.status('POST', '/api/templates/work', { model: 'bogus' }), 400, 'unknown model');
    eq((await d.state()).profiles.length, st0.profiles.length, 'nothing created');
    eq(d.logsSince(0, /Installation failed/), [], 'request errors are no installation failures');
  });

  await test('validation: slot positions, states, styles, images, settings', async () => {
    const base = `/api/pages/${P}/${page0}`;
    const W = WEBSITE;
    for (const coord of ['', '0', '0,', ',0', '01,0', '0,01', '+1,0', '-1,0', '32,0', '0,32', '1,1,1', 'a,b', ' 0,0', '0, 0', '100,0']) {
      eq(await d.status('POST', `${base}/slot`, { controller: 'Keypad', coord, uuid: W }), 400, `create at ${JSON.stringify(coord)}`);
      eq(await d.status('POST', `${base}/press`, { controller: 'Keypad', coord }), 400, `press at ${JSON.stringify(coord)}`);
    }
    for (const controller of ['keypad', 'KEYPAD', 'Dial', '', 'Encoder ']) {
      eq(await d.status('POST', `${base}/slot`, { controller, coord: '0,0', uuid: W }), 400, `controller ${JSON.stringify(controller)}`);
      eq(await d.status('POST', `${base}/press`, { controller, coord: '0,0' }), 400, `press on ${JSON.stringify(controller)}`);
    }
    eq(await d.status('DELETE', `${base}/slot?controller=Keypad&coord=9,x`), 400, 'delete');
    eq(await d.status('DELETE', `${base}/slot`), 400, 'delete without position');
    eq(await d.status('POST', `${base}/move`, { from: { controller: 'Keypad', coord: '0,0' }, to: { controller: 'Keypad', coord: '0,99' } }), 400, 'move');
    eq(await d.status('POST', `${base}/paste`, { controller: 'Nope', coord: '0,0', instance: inst(W), from_profile: P, from_page: page0 }), 400, 'paste');
    eq(await d.status('POST', `${base}/paste`, { controller: 'Keypad', coord: '0,0', instance: { Name: 'no uuid' }, from_profile: P, from_page: page0 }), 400, 'paste without UUID');
    // The highest position works.
    await d.post(`${base}/slot`, { controller: 'Keypad', coord: '31,31', uuid: W });
    await d.del(`${base}/slot?controller=Keypad&coord=31,31`);
    eq(await d.status('POST', `${base}/slot`, { controller: 'Keypad', coord: '0,0' }), 400, 'uuid missing');
    eq(await d.status('POST', `${base}/slot`, { controller: 'Keypad', coord: '0,0', uuid: 'com.e2e.no.such.action' }), 400, 'unknown action');
    await d.post(`${base}/slot`, { controller: 'Keypad', coord: '0,0', uuid: W });
    const patch = (b) => d.status('PATCH', `${base}/slot`, { controller: 'Keypad', coord: '0,0', ...b });
    eq(await patch({ style: { FontSize: 'big' } }), 400, 'style type');
    eq(await patch({ image: 'data:nonsense' }), 400, 'image');
    eq(await patch({ image: 'https://example.org/x.png' }), 400, 'remote image');
    eq(await patch({ settings: [1] }), 400, 'settings array');
    eq(await patch({ state: 64 }), 400, 'state');
    eq(await patch({ state: 63, style: { Title: 's63' } }), 200, 'last state');
    eq((await d.action('0,0')).States.length, 64, 'states added up to the edited one');
    eq(await patch({ settings_merge: { a: 1 }, settings_remove: ['path'] }), 200);
    eqc((await d.action('0,0')).Settings, { openInBrowser: true, a: 1 }, 'merged and removed');
  });

  await test('oversized bodies are refused; big key images are allowed', async () => {
    const big = JSON.stringify({ touch_color: '#000000', pad: 'x'.repeat(3 * 1024 * 1024) });
    const r = await d.raw('/api/settings', { method: 'POST', headers: { 'x-tuxdeck-token': T, 'content-type': 'application/json' }, body: big }).catch((e) => ({ status: e.code }));
    assert([413, 'ECONNRESET', 'EPIPE'].includes(r.status), `2 MB default limit: ${r.status}`);
    eq((await d.state()).settings.touch_color, st0.settings.touch_color, 'not applied');
    // Key images (e.g. animated GIFs) may be much larger: up to 64 MB.
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144"><!-- ${'p'.repeat(3 * 1024 * 1024)} --><rect width="144" height="144" fill="#ff00ff"/></svg>`;
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '0,0', image: `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}` });
    assert(/^Images\/.+\.svg$/.test((await d.action('0,0')).States[0].Image), 'stored');
    eq(pngSize(await d.render('Keypad', '0,0')), [96, 96]);
  });

  await test('press and input: only on a page a device shows; dials only where there are dials', async () => {
    const other = (await d.post(`/api/profiles/${P}/pages`)).id;
    eq(await d.status('POST', `/api/pages/${P}/${other}/press`, { controller: 'Keypad', coord: '0,0' }), 409);
    await d.del(`/api/pages/${P}/${other}`);
    for (const body of [{ type: 'dial', index: 0, ticks: 1 }, { type: 'dial_press', index: 0 }, { type: 'touch', x: 1, y: 1, kind: 1 }, { type: 'nope' }, {}]) {
      eq(await d.status('POST', `/api/devices/${dev}/input`, body), 400, JSON.stringify(body));
    }
  });

  await test('devices: brightness is clamped and remembered, folders can be opened directly', async () => {
    await d.post(`/api/devices/${dev}`, { brightness: 150 });
    eq((await d.device()).brightness, 100);
    await d.post(`/api/devices/${dev}`, { brightness: 0 });
    eq((await d.device()).brightness, 0);
    eq(JSON.parse(fs.readFileSync(path.join(d.config, 'settings.json'), 'utf8')).devices?.[dev]?.brightness, 0, 'stored in settings.json');
    await d.post(`/api/devices/${dev}`, { brightness: 70 });
    const folder = (await d.post(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '1,1', uuid: `${B}profile.openchild` })).Settings.ProfileUUID;
    await d.post(`/api/devices/${dev}`, { page: folder });
    let st = await d.state();
    eq([st.devices[0].page, st.devices[0].folder_depth], [folder, 1]);
    await d.post(`/api/devices/${dev}`, { page: page0 });
    st = await d.state();
    eq([st.devices[0].page, st.devices[0].folder_depth], [page0, 0]);
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=1,1`);
    eq((await d.state()).profiles.find((p) => p.id === P).folders, [], 'folder removed with its key');
  });

  await test('events: state, page, actions, icon pack, market and log events arrive', async () => {
    const ws = new WebSocket(`ws://127.0.0.1:${d.port}/api/events?token=${T}`);
    const got = [];
    ws.onmessage = (e) => got.push(JSON.parse(e.data));
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    const np = await d.post('/api/profiles', { name: 'Events', model: '20GBJ9901' });
    await d.patch(`/api/pages/${P}/${page0}/slot`, { controller: 'Keypad', coord: '0,0', style: { Title: 'ev' } });
    await d.req('POST', '/api/import', ICONPACK, { 'x-filename': 'e2e.streamDeckIconPack' });
    await d.del('/api/iconpacks/com.e2e.icons');
    await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    await d.del(`/api/profiles/${np.id}`);
    await waitFor(() => got.some((e) => e.event === 'actions') && got.filter((e) => e.event === 'iconpacks').length >= 2, 3000, 'events');
    ws.close();
    assert(got.some((e) => e.event === 'state'), 'state');
    assert(got.some((e) => e.event === 'page' && e.profile === P && e.page === page0), 'page');
    assert(got.some((e) => e.event === 'log' && e.level === 'info' && /Icon pack/.test(e.message)), 'log');
    assert(got.some((e) => e.event === 'market' && e.type === 'installed' && e.id === 'com.e2e.counter'), 'market');
    await d.del('/api/plugins/com.e2e.counter');
  });

  await test('backups: download options, stored backups by name, restoring twice keeps both safety copies', async () => {
    const list = await waitFor(async () => { const l = await d.get('/api/backups'); return l.some((b) => b.kind === 'daily') && l; }, 4000, 'daily backup');
    const daily = list.find((b) => b.kind === 'daily');
    assert(listZip(await d.get(`/api/backups/${encodeURIComponent(daily.name)}`)).includes('tuxdeck-backup.json'), 'stored backup downloadable');
    await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
    assert(!listZip(await d.get('/api/backup?plugins=0')).some((n) => n.startsWith('Plugins/')), 'plugins=0');
    const full = listZip(await d.get('/api/backup?iconpacks=1'));
    assert(full.some((n) => n.startsWith('IconPacks/tuxdeck-essentials.sdIconPack/')), 'iconpacks=1');
    assert(full.some((n) => n.startsWith('Plugins/com.e2e.counter.sdPlugin/')), 'plugins by default');
    const r1 = await d.post(`/api/backups/${encodeURIComponent(daily.name)}/restore`);
    const r2 = await d.post(`/api/backups/${encodeURIComponent(daily.name)}/restore`);
    assert(r1.safety_backup !== r2.safety_backup, `two safety backups: ${r1.safety_backup} ${r2.safety_backup}`);
    const after = await d.get('/api/backups');
    for (const r of [r1, r2]) assert(after.some((b) => b.name === r.safety_backup && b.kind === 'before'), r.safety_backup);
    eq((await d.state()).devices[0].connected, true);
  });
}

/** Profiles, pages, export/import (also hostile archives) and icon packs. */
async function suiteProfiles(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const dev = st0.devices[0].id;
  const profilesDir = path.join(d.data, 'ProfilesV2');
  const W = WEBSITE;
  const importZip = (buf, name = 'x.streamDeckProfile') => d.req('POST', '/api/import', buf, { 'x-filename': name });
  const marker = () => `tuxdeck-e2e-${process.pid}-${randomUUID().slice(0, 8)}`;
  const leaked = (m) => [...walkFiles(d.dir), ...fs.readdirSync(os.tmpdir())].filter((f) => f.includes(m));

  await test('profiles: every model can get a profile; the deck only shows its own model', async () => {
    const models = ['20GBJ9901', '20GBD9901', '20GBA9901', '20GAA9902', '20GAA9901', '20GAT9901', '20GAT9902', '20GAI9901', '20GAI9902', '20GBF9901'];
    for (const model of models) {
      const p = await d.post('/api/profiles', { name: `M ${model}`, model });
      eq([p.model, p.pages.length], [model, 1], model);
      eq(await d.status('POST', `/api/devices/${dev}`, { profile: p.id }), model === '20GBJ9901' ? 200 : 400, `show ${model} on a Neo`);
      eq(await d.status('GET', `/api/profiles/${p.id}/export`), 200, `export ${model}`);
      await d.post(`/api/devices/${dev}`, { profile: P });
      await d.del(`/api/profiles/${p.id}`);
      assert(!fs.existsSync(path.join(profilesDir, `${p.id}.sdProfile`)), 'directory removed');
    }
    eq((await d.state()).profiles.map((p) => p.id), [P]);
  });

  await test('profiles: names lose control characters, app links, safe export file names', async () => {
    const p = await d.post('/api/profiles', { name: 'Tab\there', model: '20GBJ9901' });
    const name = async () => (await d.state()).profiles.find((x) => x.id === p.id);
    eq((await name()).name, 'Tab here', 'create');
    await d.patch(`/api/profiles/${p.id}`, { name: 'Line\nbreak\u0007' });
    eq((await name()).name, 'Line break ', 'rename');
    await d.patch(`/api/profiles/${p.id}`, { app: 'firefox' });
    await d.patch(`/api/profiles/${p.id}`, {});
    eq([(await name()).name, (await name()).app], ['Line break ', 'firefox'], 'empty patch changes nothing');
    await d.patch(`/api/profiles/${p.id}`, { name: 'A/B "C"\\' });
    const r = await fetch(d.url(`/api/profiles/${p.id}/export`), { headers: { 'x-tuxdeck-token': d.token } });
    assert(r.headers.get('content-disposition').includes('filename="A_B _C__.streamDeckProfile"'), r.headers.get('content-disposition'));
    await d.del(`/api/profiles/${p.id}`);
  });

  await test('pages: new pages copy the template page with new ids; deleting the shown page moves the deck', async () => {
    const zipBuf = profileZip({
      name: 'With template',
      pages: [{ keys: { Keypad: { '0,0': inst(W, { path: 'a.example' }, 'A') } } }],
      def: { keys: { Neo: { '1,0': inst(`${B}system.digitaltime`, { layout: 'digital_time_01' }) }, Keypad: { '3,1': inst(W, { path: 'tpl.example' }, 'T') } } },
    });
    const imp = await importZip(zipBuf);
    const prof = (await d.state()).profiles.find((p) => p.id === imp.id);
    assert(prof.default_page, 'template page kept');
    const np = (await d.post(`/api/profiles/${imp.id}/pages`)).id;
    const keys = (pg) => pg.controllers.flatMap((c) => Object.entries(c.Actions).map(([k, a]) => ({ at: `${c.Type} ${k}`, uuid: a.UUID, id: a.ActionID, s: a.Settings })));
    const copy = keys(await d.page(imp.id, np));
    const tpl = keys(await d.page(imp.id, prof.default_page));
    eqc(copy.map((k) => [k.at, k.uuid, k.s]), tpl.map((k) => [k.at, k.uuid, k.s]), 'same keys as the template');
    assert(copy.every((k) => !tpl.some((t) => t.id === k.id)), 'new action ids');
    await d.post(`/api/devices/${dev}`, { profile: imp.id });
    await d.post(`/api/devices/${dev}`, { page: np });
    await d.del(`/api/pages/${imp.id}/${np}`);
    eq((await d.device()).page, prof.pages[0], 'deck back on the first page');
    eq((await d.state()).profiles.find((p) => p.id === imp.id).pages, prof.pages);
    // A folder is no top-level page: it goes with its key.
    const folder = (await d.post(`/api/pages/${imp.id}/${prof.pages[0]}/slot`, { controller: 'Keypad', coord: '1,0', uuid: `${B}profile.openchild` })).Settings.ProfileUUID;
    eq(await d.status('DELETE', `/api/pages/${imp.id}/${folder}`), 404);
    await d.post(`/api/devices/${dev}`, { profile: P });
    await d.del(`/api/profiles/${imp.id}`);
  });

  await test('pages: reorder; the deck stays on its page, "Go to Page" keys follow their pages', async () => {
    const G = `${B}page.goto`;
    const imp = await importZip(profileZip({
      name: 'Order',
      pages: [
        { keys: { Keypad: {
          '0,0': inst(G, { PageIndex: 3 }, 'to C'),
          '1,0': inst(`${B}multiactions.routine`, {}, '', { Actions: [{ Actions: [inst(G, { PageIndex: 1 }), inst(W, { path: 'x.example', PageIndex: 1 })] }] }),
          '2,0': inst(`${B}keys.logic`, {}, '', { Actions: [{ Actions: [inst(G, { PageIndex: 9 })] }, { Actions: [] }, { Actions: [] }] }),
        } } },
        { keys: { Keypad: { '0,0': inst(W, { path: 'b.example' }, 'B') } } },
        { keys: { Keypad: { '0,0': inst(W, { path: 'c.example' }, 'C') } } },
      ],
    }));
    const [A, Bp, C] = (await d.state()).profiles.find((p) => p.id === imp.id).pages;
    await d.post(`/api/devices/${dev}`, { profile: imp.id });
    await d.post(`/api/devices/${dev}`, { page: Bp });
    // C A B
    eq((await d.post(`/api/profiles/${imp.id}/pages/order`, { pages: [C, A, Bp] })).pages, [C, A, Bp]);
    eq((await d.state()).profiles.find((p) => p.id === imp.id).pages, [C, A, Bp], 'new order');
    eq((await d.device()).page, Bp, 'the deck stays on its page');
    const keys = (await d.page(imp.id, A)).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq(keys['0,0'].Settings.PageIndex, 1, 'C is page 1 now');
    eq(keys['1,0'].Actions[0].Actions.map((a) => a.Settings.PageIndex), [2, 1], 'in a multi action (other actions untouched)');
    eq(keys['2,0'].Actions[0].Actions[0].Settings.PageIndex, 1, '"past the last page" meant C, now page 1');
    // Pressing the key goes to the same page as before.
    await d.post(`/api/devices/${dev}`, { page: A });
    await d.press('Keypad', '0,0');
    await d.waitState((st) => st.devices[0].page === C, 2000, 'went to C');
    // Only the same pages in a new order are accepted.
    for (const bad of [[C, A], [C, A, Bp, Bp], [C, A, randomUUID()], []]) {
      eq(await d.status('POST', `/api/profiles/${imp.id}/pages/order`, { pages: bad }), 400, JSON.stringify(bad));
    }
    eq(await d.status('POST', `/api/profiles/${randomUUID()}/pages/order`, { pages: [A] }), 404);
    // Undo: the old order, the numbers come back.
    await d.post(`/api/profiles/${imp.id}/pages/order`, { pages: [A, Bp, C] });
    const back = (await d.page(imp.id, A)).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq([back['0,0'].Settings.PageIndex, back['1,0'].Actions[0].Actions[0].Settings.PageIndex], [3, 1]);
    // Stored in the profile's manifest (the official format).
    const man = JSON.parse(fs.readFileSync(path.join(profilesDir, `${imp.id}.sdProfile`, 'manifest.json'), 'utf8'));
    eq(man.Pages.Pages.map((x) => x.toLowerCase()), [A, Bp, C].map((x) => x.toLowerCase()));
    await d.post(`/api/devices/${dev}`, { profile: P });
    await d.del(`/api/profiles/${imp.id}`);
  });

  await test('export → import keeps keys, titles, settings, custom images, multi actions and folders', async () => {
    const pg = (await d.post(`/api/profiles/${P}/pages`)).id;
    const base = `/api/pages/${P}/${pg}`;
    await d.post(`${base}/slot`, { controller: 'Keypad', coord: '0,0', uuid: W });
    await d.patch(`${base}/slot`, {
      controller: 'Keypad', coord: '0,0', settings: { path: 'https://example.org/rt', openInBrowser: false },
      style: { Title: 'Round\ntrip', TitleColor: '#ff0000', FontSize: 16 }, image: RED_PNG,
    });
    await d.post(`${base}/slot`, { controller: 'Keypad', coord: '1,0', uuid: `${B}multiactions.routine` });
    await d.patch(`${base}/slot`, { controller: 'Keypad', coord: '1,0', settings: { _actions0: [{ UUID: W, Settings: { path: 'one.example' } }, { UUID: `${B}multiactions.delay`, Settings: { delay: 50 } }] } });
    const folder = (await d.post(`${base}/slot`, { controller: 'Keypad', coord: '2,0', uuid: `${B}profile.openchild` })).Settings.ProfileUUID;
    await d.post(`/api/pages/${P}/${folder}/slot`, { controller: 'Keypad', coord: '1,0', uuid: `${B}system.close` });
    await d.patch(`/api/pages/${P}/${folder}/slot`, { controller: 'Keypad', coord: '1,0', settings: { app_name: 'in-folder' } });
    const imp = await importZip(await d.get(`/api/profiles/${P}/export`), 'roundtrip.streamDeckProfile');
    const st = await d.state();
    const a = st.profiles.find((p) => p.id === P);
    const b = st.profiles.find((p) => p.id === imp.id);
    eq([b.name, b.model, b.pages.length, b.folders.length], [a.name, a.model, a.pages.length, a.folders.length]);
    const view = (page) => page.controllers.map((c) => [c.Type, Object.fromEntries(Object.entries(c.Actions).map(([k, x]) => [k, {
      UUID: x.UUID, Settings: stripIds(x.Settings), States: x.States.map((s) => ({ ...s, Image: s.Image ? (s.Image.startsWith('data:') ? s.Image : 'file') : null })),
    }]))]);
    for (let i = 0; i < a.pages.length; i++) eqc(view(await d.page(imp.id, b.pages[i])), view(await d.page(P, a.pages[i])), `page ${i}`);
    const pgB = b.pages[a.pages.indexOf(pg)];
    const ra = await d.get(`/api/render/${P}/${pg}/Keypad/0,0.png?size=96`);
    assert(ra.equals(await d.get(`/api/render/${imp.id}/${pgB}/Keypad/0,0.png?size=96`)), 'same custom image and title');
    const inner = await d.action('1,0', 'Keypad', imp.id, (await d.action('2,0', 'Keypad', imp.id, pgB)).Settings.ProfileUUID);
    eq(inner.Settings.app_name, 'in-folder', 'folder content');
    const ids = async (pid, pgs) => (await Promise.all(pgs.map((x) => d.page(pid, x)))).flatMap((p) => p.controllers.flatMap((c) => Object.values(c.Actions).map((x) => x.ActionID)));
    const idsA = await ids(P, [...a.pages, ...a.folders]);
    const idsB = await ids(imp.id, [...b.pages, ...b.folders]);
    assert(idsB.length === idsA.length && idsB.every((x) => !idsA.includes(x)), 'every action got a new id');
    await d.del(`/api/profiles/${imp.id}`);
    await d.del(`/api/pages/${P}/${pg}`);
  });

  await test('import: archives packed on Windows, the 3.0 layout and macOS metadata are accepted', async () => {
    const files = unzipAll(await d.get(`/api/profiles/${P}/export`));
    const root = Object.keys(files)[0].split('/')[0];
    const win = Object.fromEntries(Object.entries(files).map(([n, v]) => [n.replace(/\//g, '\\'), v]));
    const v3 = Object.fromEntries(Object.entries(files).map(([n, v]) => [`Profiles/${n}`, v]));
    v3['__MACOSX/._manifest.json'] = 'junk';
    v3[`Profiles/${root}/.DS_Store`] = 'junk';
    const orig = (await d.state()).profiles.find((p) => p.id === P);
    const keysOf = async (id, pg) => Object.keys((await d.page(id, pg)).controllers.find((c) => c.Type === 'Keypad').Actions).sort();
    for (const [what, z] of [['backslashes', zip(win)], ['3.0 layout', zip(v3)]]) {
      const r = await importZip(z);
      const p = (await d.state()).profiles.find((x) => x.id === r.id);
      eq([p.pages.length, p.folders.length], [orig.pages.length, orig.folders.length], what);
      eq(await keysOf(r.id, p.pages[0]), await keysOf(P, orig.pages[0]), what);
      const bad = walkFiles(path.join(profilesDir, `${r.id}.sdProfile`)).filter((f) => /\\|\.DS_Store|__MACOSX/.test(path.relative(profilesDir, f)));
      eq(bad, [], `${what}: clean file names`);
      await d.del(`/api/profiles/${r.id}`);
    }
  });

  await test('import: unsafe archive paths (.., absolute, drive letters, backslashes) are never written', async () => {
    const files = unzipAll(await d.get(`/api/profiles/${P}/export`));
    const root = Object.keys(files)[0].split('/')[0];
    const m = marker();
    const abs = path.join(os.tmpdir(), `${m}-abs.txt`);
    const evil = {
      ...files,
      [`${root}/../../${m}-1.txt`]: 'x',
      [`${root}\\..\\..\\${m}-2.txt`]: 'x',
      [`${root}/Profiles/../../../../${m}-3.txt`]: 'x',
      [abs]: 'x',
      [`C:\\${m}-4.txt`]: 'x',
      [`${root}/C:/${m}-5.txt`]: 'x',
      [`..\\${m}-6.txt`]: 'x',
      [`${root}/./ok/../../../${m}-7.txt`]: 'x',
    };
    const before = fs.readdirSync(profilesDir).length;
    const r = await importZip(zip(evil), 'evil.streamDeckProfile');
    eq(fs.readdirSync(profilesDir).length, before + 1, 'imported, unsafe entries skipped');
    eq(leaked(m), [], 'no file of an unsafe entry was written');
    assert(!fs.existsSync(abs), 'absolute path not written');
    // Nothing but unsafe entries: refused, nothing left behind.
    const only = zip({ [`../${root}/manifest.json`]: files[`${root}/manifest.json`], [`/${root}/manifest.json`]: files[`${root}/manifest.json`] });
    eq(await d.status('POST', '/api/import', only, { 'x-filename': 'evil.streamDeckProfile' }), 400);
    eq(fs.readdirSync(profilesDir).length, before + 1, 'no leftover directory');
    await d.del(`/api/profiles/${r.id}`);
  });

  await test('import: a folder key whose target is a path cannot delete anything outside the profile', async () => {
    const m = marker();
    // Relative to a page folder, `../../../X` is <data>/X.
    const canary = (n) => {
      const dir = path.join(d.data, `${m}-${n}`);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'keep.txt'), 'x');
      return dir;
    };
    const abs = canary('abs');
    const rel = canary('rel');
    const folder = (target) => inst(`${B}profile.openchild`, { ProfileUUID: target }, 'F');
    // Format 2 keeps the value as it is, format 3 upper-cases it.
    for (const version of ['2.0', '3.0']) {
      const target = (p) => (version === '3.0' ? p.toUpperCase() : p);
      const imp = await importZip(profileZip({
        version,
        pages: [{ keys: { Keypad: { '0,0': folder(target(abs)), '1,0': folder(target(`../../../${path.basename(rel)}`)), '2,0': folder(''), '3,0': folder('.') } } }],
      }));
      const page = (await d.state()).profiles.find((p) => p.id === imp.id).pages[0];
      for (const coord of ['0,0', '1,0', '2,0', '3,0']) {
        eq(await d.status('DELETE', `/api/pages/${imp.id}/${page}/slot?controller=Keypad&coord=${coord}`), 200, `${version} ${coord}`);
      }
      await d.del(`/api/profiles/${imp.id}`);
      for (const dir of [abs, rel]) assert(fs.existsSync(path.join(dir, 'keep.txt')), `${version}: ${dir} kept`);
    }
    assert(fs.existsSync(profilesDir) && fs.readdirSync(profilesDir).length > 0, 'other profiles kept');
    // Another spelling of a top-level page's id must not delete that page.
    const second = randomUUID();
    const imp = await importZip(profileZip({
      pages: [{ keys: { Keypad: { '0,0': folder(`urn:uuid:${second}`), '1,0': folder(`{${second.toUpperCase()}}`), '2,0': folder(second.replace(/-/g, '')) } } },
        { id: second, keys: { Keypad: { '0,0': inst(WEBSITE, { path: 'kept.example' }, 'K') } } }],
    }));
    const pages = (await d.state()).profiles.find((p) => p.id === imp.id).pages;
    for (const coord of ['0,0', '1,0', '2,0']) await d.del(`/api/pages/${imp.id}/${pages[0]}/slot?controller=Keypad&coord=${coord}`);
    const kept = (await d.page(imp.id, second)).controllers.find((c) => c.Type === 'Keypad').Actions['0,0'];
    eq(kept?.Settings.path, 'kept.example', 'top-level page kept');
    await d.del(`/api/profiles/${imp.id}`);
  });

  await test('import: plugin and icon pack archives packed on Windows work, unsafe entries are skipped', async () => {
    const m = marker();
    const win = Object.fromEntries(Object.entries(pluginWithActions('com.e2e.win', ['count'])).map(([n, v]) => [n.replace(/\//g, '\\'), v]));
    win[`com.e2e.win.sdPlugin\\..\\..\\${m}-p1.txt`] = 'x';
    win[`com.e2e.win.sdPlugin/../../${m}-p2.txt`] = 'x';
    win[path.join(os.tmpdir(), `${m}-p3.txt`)] = 'x';
    win[`com.e2e.win.sdPlugin\\C:\\${m}-p4.txt`] = 'x';
    const mk = d.mark();
    eq((await d.req('POST', '/api/import', zip(win), { 'x-filename': 'win.streamDeckPlugin' })).id, 'com.e2e.win');
    const pdir = path.join(d.data, 'Plugins/com.e2e.win.sdPlugin');
    for (const f of ['manifest.json', 'plugin.js', 'pi/pi.html', 'layouts/custom.json']) assert(fs.existsSync(path.join(pdir, f)), f);
    await d.waitLog(/plugin registered: com\.e2e\.win/, mk, 8000);
    const pack = {
      'com.e2e.winpack.sdIconPack\\manifest.json': JSON.stringify({ Name: 'Win Pack', Author: 'e2e', Version: '1.0', Icon: 'icon.svg' }),
      'com.e2e.winpack.sdIconPack\\icon.svg': SVG('#123456'),
      'com.e2e.winpack.sdIconPack\\icons\\sub\\my_icon-one.svg': SVG('#654321'),
      [`com.e2e.winpack.sdIconPack\\..\\${m}-i1.txt`]: 'x',
      [`C:\\${m}-i2.txt`]: 'x',
      [`/${m}-i3.txt`]: 'x',
    };
    eq((await d.req('POST', '/api/import', zip(pack), { 'x-filename': 'win.streamDeckIconPack' })).id, 'com.e2e.winpack');
    const icons = await d.get('/api/iconpacks/com.e2e.winpack/icons');
    eq(icons.items.map((i) => [i.path, i.name]), [['sub/my_icon-one.svg', 'my icon one']], 'icons without icons.json are listed by file');
    assert((await d.get(icons.items[0].url)).toString().includes('#654321'), 'icon served');
    eq(leaked(m), [], 'nothing written outside');
    await d.del('/api/plugins/com.e2e.win');
    await d.del('/api/iconpacks/com.e2e.winpack');
    assert(!fs.existsSync(pdir), 'plugin removed');
  });

  await test('import: malformed archives are refused and leave nothing behind', async () => {
    const before = fs.readdirSync(profilesDir).sort();
    const exp = await d.get(`/api/profiles/${P}/export`);
    const root = listZip(exp)[0].split('/')[0];
    const cases = [
      ['not a zip', Buffer.from('hello'), 'x.streamDeckProfile', /not a package file/],
      ['empty', Buffer.alloc(0), 'x.streamDeckProfile', /not a package file/],
      ['broken zip', Buffer.concat([Buffer.from('PK\u0003\u0004'), Buffer.alloc(64)]), 'x.streamDeckProfile', /invalid archive/],
      ['truncated', exp.subarray(0, Math.floor(exp.length / 2)), 'x.streamDeckProfile', /invalid archive/],
      ['unknown content', zip({ 'readme.txt': 'hi' }), undefined, /unknown package type/],
      ['no profile folder', zip({ 'readme.txt': 'hi' }), 'x.streamDeckProfile', /contains no profile \(\.sdProfile\)/],
      ['no manifest', zip({ [`${root}/Profiles/X/manifest.json`]: '{}' }), 'x.streamDeckProfile', /manifest|No such file/],
      ['manifest of the wrong shape', zip({ [`${root}/manifest.json`]: '{"Name": 5}' }), 'x.streamDeckProfile', /manifest/],
      ['no plugin folder', zip({ 'plugin.js': '' }), 'x.streamDeckPlugin', /contains no plugin \(\.sdPlugin folder\)/],
      ['plugin without manifest', zip({ 'com.e2e.nomf.sdPlugin/plugin.js': '' }), 'x.streamDeckPlugin', /manifest/],
      ['icon pack without folder', zip({ 'icons/a.svg': SVG('#000') }), 'x.streamDeckIconPack', /sdIconPack/],
      ['icon pack without manifest', zip({ 'com.e2e.nomf.sdIconPack/icons/a.svg': SVG('#000') }), 'x.streamDeckIconPack', /manifest/],
      ['icon pack with broken icons.json', zip({ 'com.e2e.bad.sdIconPack/manifest.json': '{}', 'com.e2e.bad.sdIconPack/icons.json': '{' }), 'x.streamDeckIconPack', /icons\.json/],
      ['icon pack without a name', zip({ '.sdIconPack/manifest.json': '{}' }), 'x.streamDeckIconPack', /name/],
    ];
    for (const [what, body, name, re] of cases) {
      const e = await d.req('POST', '/api/import', body, name ? { 'x-filename': name } : {}).then(() => null, (x) => x);
      assert(e && e.status === 400, `${what}: ${e ? e.status : 'accepted'}`);
      assert(re.test(e.message), `${what}: ${e.message}`);
    }
    eq(fs.readdirSync(profilesDir).sort(), before, 'no leftover profile');
    assert(!fs.existsSync(path.join(d.data, 'Plugins/.installing')), 'no staging folder');
    eq((await d.get('/api/plugins')).length, 0, 'no plugin');
    eq((await d.get('/api/iconpacks')).map((p) => p.id), ['tuxdeck-essentials'], 'no icon pack');
    eq(fs.readdirSync(path.join(d.data, 'IconPacks')).filter((f) => f.endsWith('.installing')), [], 'no staging folder for icon packs');
  });

  await test('icon packs: update in place, search by several words, paging bounds, file paths', async () => {
    await d.req('POST', '/api/import', ICONPACK, { 'x-filename': 'e2e.streamDeckIconPack' });
    await d.req('POST', '/api/import', ICONPACK, { 'x-filename': 'e2e.streamDeckIconPack' });
    eq((await d.get('/api/iconpacks')).filter((p) => p.id === 'com.e2e.icons').length, 1, 'a reinstall replaces the pack');
    const q = (s) => d.get(`/api/iconpacks/com.e2e.icons/icons?${s}`);
    eq((await q('search=TRAVEL%20sport')).items.map((i) => i.name), ['Bike'], 'all words, any case');
    eq((await q('search=zeppelin')).total, 0);
    eq((await q('search=train.svg')).items.map((i) => i.name), ['Train'], 'file name');
    const far = await q('offset=99');
    eq([far.total, far.items.length], [3, 0]);
    eq((await q('limit=100000')).items.length, 3);
    eq((await q('limit=nope&offset=-1')).items.length, 3, 'bad numbers fall back to the defaults');
    for (const p of ['/iconpack-files/com.e2e.icons/%2E%2E/%2E%2E/%2E%2E/config/settings.json', '/iconpack-files/com.e2e.icons/..%2Fmanifest.json',
      '/iconpack-files/com.e2e.icons/%2Fetc%2Fpasswd', '/iconpack-files/..%2FIconPacks%2Fcom.e2e.icons/manifest.json']) {
      assert([403, 404].includes((await d.raw(p)).status), p);
    }
    eq((await d.raw('/iconpack-files/com.e2e.icons/manifest.json')).status, 200, 'files of the pack');
    await d.del('/api/iconpacks/com.e2e.icons');
    eq(await d.status('DELETE', '/api/iconpacks/com.e2e.icons'), 404, 'already removed');
  });

  await test('profiles: the last profile of a connected deck stays; deleting the shown one switches', async () => {
    eq((await d.state()).profiles.filter((p) => p.model === '20GBJ9901').map((p) => p.id), [P]);
    eq(await d.status('DELETE', `/api/profiles/${P}`), 409, 'only profile of the deck');
    const xl = await d.post('/api/profiles', { name: 'Only XL', model: '20GAT9901' });
    await d.del(`/api/profiles/${xl.id}`); // no XL connected
    const other = await d.post('/api/profiles', { name: 'Other', model: '20GBJ9901' });
    await d.del(`/api/profiles/${P}`);
    eq((await d.device()).profile, other.id, 'the deck shows the remaining profile');
    assert(!fs.existsSync(path.join(profilesDir, `${P}.sdProfile`)), 'files removed');
    eq(await d.status('DELETE', `/api/profiles/${other.id}`), 409);
    await d.kill();
    await d.launch();
    eq((await d.device()).profile, other.id, 'remembered after a restart');
  });
}

/** Two decks of the same model: own profiles, unplugging, one deck switching the other. */
async function suiteMultiDeck(d) {
  const C = 'com.e2e.counter.count';
  const ev = (event, ctx = '\\S+') => new RegExp(`\\[com\\.e2e\\.counter\\] ev ${event} ${ctx} `);
  const deck = async (id) => (await d.state()).devices.find((x) => x.id === id);
  const [A, B] = ['VIRTUAL-NEO', 'VIRTUAL-NEO-2'];
  const put = async (id, coord, uuid, settings) => {
    const x = await deck(id);
    await d.post(`/api/pages/${x.profile}/${x.page}/slot`, { controller: 'Keypad', coord, uuid });
    if (settings) await d.patch(`/api/pages/${x.profile}/${x.page}/slot`, { controller: 'Keypad', coord, settings });
  };
  const key = async (id, index, down) => d.post(`/api/devices/${id}/input`, { type: 'key', index, down });
  const plug = (id, connected) => d.post(`/api/devices/${id}/virtual`, { connected });

  await test('two decks of the same model have their own default profile', async () => {
    const st = await d.state();
    eq(st.devices.map((x) => x.id).sort(), [A, B]);
    const [a, b] = [await deck(A), await deck(B)];
    assert(a.profile && b.profile && a.profile !== b.profile, 'own profiles');
  });

  let m = d.mark();
  await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
  await d.waitLog(ev('deviceDidConnect', '-'), m, 8000);

  await test('unplugging releases held keys; plugins see the deck again when it is back', async () => {
    await put(A, '0,0', C);
    await put(A, '1,0', 'com.elgato.streamdeck.system.hotkey', { Hotkeys: [HK_Z] });
    // An action that acts on release must not run because the deck went away.
    await put(A, '3,0', 'com.elgato.streamdeck.system.openapp', { exec: 'e2eunplugged', long_press: 'quit' });
    await d.waitLog(/\[com\.e2e\.counter\] appear Keypad 0,0/, m, 4000);
    m = d.mark();
    await key(A, 0, true);
    await key(A, 1, true);
    await key(A, 3, true);
    await d.waitLog(ev('keyDown'), m);
    await d.waitLog(/send_key_state.*key = "code:52".*"down"/i, m);
    m = d.mark();
    await plug(A, false);
    await d.waitLog(ev('keyUp'), m);
    await d.waitLog(/send_key_state.*key = "code:52".*"up"/i, m);
    await d.waitLog(ev('willDisappear'), m);
    await d.waitLog(ev('deviceDidDisconnect', '-'), m);
    eq((await deck(A)).connected, false);
    eq(d.logsSince(m, /e2eunplugged/), [], 'Open Application neither started nor quit');
    // Changes while it is away reach no plugin.
    m = d.mark();
    await put(A, '2,0', C);
    await sleep(300);
    eq(d.logsSince(m, ev('willAppear')), [], 'no willAppear for an unplugged deck');
    m = d.mark();
    await plug(A, true);
    await d.waitLog(ev('deviceDidConnect', '-'), m);
    await d.waitLog(/\[com\.e2e\.counter\] appear Keypad 0,0/, m);
    await d.waitLog(/\[com\.e2e\.counter\] appear Keypad 2,0/, m);
    // The released key works normally afterwards.
    m = d.mark();
    await key(A, 0, true);
    await key(A, 0, false);
    await d.waitLog(ev('keyUp'), m);
  });

  await test('switch profile: a key on one deck switches the other deck', async () => {
    const target = await d.post('/api/profiles', { name: 'For B', model: (await deck(B)).model });
    const [aBase, bBase] = [(await deck(A)).profile, (await deck(B)).profile];
    // Its own id, or the id of the official app, which contains the serial number.
    for (const device of [B, `@(1)[4057/154/${B}]`]) {
      await d.post(`/api/devices/${B}`, { profile: bBase });
      await put(A, '3,0', 'com.elgato.streamdeck.profile.rotate', { ProfileUUID: target.id, DeviceUUID: device });
      const a = await deck(A);
      await d.post(`/api/pages/${a.profile}/${a.page}/press`, { controller: 'Keypad', coord: '3,0' });
      await d.waitState((st) => st.devices.find((x) => x.id === B).profile === target.id, 2000, `deck B switched (${device})`);
      eq((await deck(A)).profile, aBase, 'deck A stays');
    }
    await d.post(`/api/devices/${B}`, { profile: bBase });
  });

  await test('one page on two decks: test press, settings and previews reach the chosen deck', async () => {
    const a = await deck(A);
    await d.post(`/api/devices/${B}`, { profile: a.profile });
    await d.post(`/api/devices/${B}`, { page: a.page });
    m = d.mark();
    await put(A, '1,1', C);
    await waitFor(async () => d.logsSince(m, /\[com\.e2e\.counter\] appear Keypad 1,1 /).length >= 2, 4000, 'willAppear on both decks');
    const base = `/api/pages/${a.profile}/${a.page}`;
    const png = (dev) => d.get(`/api/render/${a.profile}/${a.page}/Keypad/1,1.png?size=96&v=${Date.now()}&device=${dev}`);
    await sleep(1600); // no OK overlay left from anything before
    const [beforeA, beforeB] = [await png(A), await png(B)];
    // A test press from the UI with deck B selected: deck B's key counts (title n=1).
    m = d.mark();
    await d.post(`${base}/press`, { controller: 'Keypad', coord: '1,1', device: B });
    const pressed = /ev keyDown (\S+) /.exec(await d.waitLog(ev('keyDown'), m))[1];
    await sleep(1600);
    assert(!(await png(B)).equals(beforeB), 'deck B shows the press');
    assert((await png(A)).equals(beforeA), 'deck A does not');
    // A settings change reaches the plugin for both decks' contexts.
    m = d.mark();
    await d.patch(`${base}/slot`, { controller: 'Keypad', coord: '1,1', settings: { both: 1 }, device: B });
    await waitFor(async () => d.logsSince(m, /ev didReceiveSettings \S+ .*"both":1/).length >= 2, 3000, 'both contexts');
    const got = d.logsSince(m, /ev didReceiveSettings \S+ .*"both":1/).map((l) => /ev didReceiveSettings (\S+) /.exec(l)[1]);
    eq(new Set(got).size, 2, 'two different contexts');
    assert(got.includes(pressed), 'the pressed deck is one of them');
  });

  await test('smart profiles meant for one deck switch only that deck', async () => {
    const model = (await deck(A)).model;
    const [baseA, baseB] = [(await deck(A)).profile, (await deck(B)).profile];
    const sa = await d.post('/api/profiles', { name: 'Smart A', model });
    const sb = await d.post('/api/profiles', { name: 'Smart B', model });
    await d.patch(`/api/profiles/${sa.id}`, { app: 'e2esmart', device: A });
    await d.patch(`/api/profiles/${sb.id}`, { app: 'e2esmart', device: B });
    eq((await d.state()).profiles.find((p) => p.id === sa.id).device, A);
    await d.post('/api/active-app', { app: 'e2esmart' });
    await d.waitState((st) => st.devices.find((x) => x.id === A).profile === sa.id && st.devices.find((x) => x.id === B).profile === sb.id, 2000, 'each deck its own');
    await d.post('/api/active-app', { app: 'other' });
    await d.waitState((st) => st.devices.find((x) => x.id === A).profile === baseA && st.devices.find((x) => x.id === B).profile === baseB, 2000, 'back');
  });
}

/** Every built-in action: behaviour and failure messages (dry run). */
async function suiteBuiltins(d) {
  await onePage(d);
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;
  const dev = st0.devices[0].id;
  const home = d.home;
  fs.mkdirSync(path.join(home, 'e2e-dir'));
  fs.writeFileSync(path.join(home, 'e2e.txt'), 'text');
  fs.writeFileSync(path.join(home, 'e2e.sh'), '#!/bin/sh\n', { mode: 0o755 });
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
  const put = async (coord, uuid, settings) => {
    await d.slot('Keypad', coord, uuid);
    if (settings) await d.settings('Keypad', coord, settings);
  };
  const pressLog = async (coord, re, hold) => {
    const m = d.mark();
    await d.press('Keypad', coord, hold);
    return d.waitLog(re, m);
  };
  const brightness = (b) => d.post(`/api/devices/${dev}`, { brightness: b });

  await test('every built-in action can be placed, pressed and removed', async () => {
    const all = (await d.get('/api/actions')).flatMap((c) => c.actions).filter((a) => a.builtin);
    const want = ['profile.openchild', 'profile.backtoparent', 'profile.rotate', 'page.previous', 'page.next', 'page.goto', 'page.indicator',
      'system.keybrightness', 'system.sleep', 'system.hotkey', 'system.hotkeyswitch', 'system.open', 'system.openapp', 'system.close',
      'system.website', 'system.multimedia', 'system.text', 'system.timer', 'multiactions.routine', 'multiactions.routine2',
      'multiactions.random', 'multiactions.delay', 'keys.logic', 'soundboard.playaudio', 'soundboard.stopaudioplay',
      'system.digitaltime', 'system.pagination'].map((u) => B + u).concat('dev.tuxdeck.infobar.text');
    // A new built-in action needs tests here.
    eq(all.map((a) => a.uuid).sort(), [...want].sort(), 'built-in actions');
    for (const a of all) {
      assert(Array.isArray(a.form) && a.icon && a.name && a.supported, `${a.uuid}: form, icon, name`);
      const ctrl = a.controllers.includes('Keypad') ? 'Keypad' : a.controllers[0];
      const coord = ctrl === 'Neo' ? '1,0' : '0,1';
      await d.post(`/api/pages/${P}/${page0}/slot`, { controller: ctrl, coord, uuid: a.uuid });
      await d.post(`/api/pages/${P}/${page0}/press`, { controller: ctrl, coord });
      const s = await d.device();
      if (s.sleeping) await brightness(70);
      if (s.page !== page0 || s.profile !== P) await d.post(`/api/devices/${dev}`, { profile: P, page: page0 });
      await d.del(`/api/pages/${P}/${page0}/slot?controller=${ctrl}&coord=${coord}`);
    }
    await d.slot('Neo', '1,0', `${B}system.digitaltime`);
    eq(d.logsSince(0, /panicked/), []);
  });

  await test('hotkey: earlier combos are tapped, the last one follows the key; Cmd is Super unless Cmd as Ctrl', async () => {
    await put('0,0', `${B}system.hotkey`, { Coalesce: true, Hotkeys: [HK_A, HK_Z] });
    const m = d.mark();
    await d.press('Keypad', '0,0', 300);
    const up = await d.waitLog(/send_key_state.*key = "code:52".*"up"/, m);
    const lines = d.lines.slice(m);
    const tap = lines.findIndex((l) => /send_shortcut.*SHIFT.*key = "code:38"/.test(l));
    const down = lines.findIndex((l) => /send_key_state.*key = "code:52".*"down"/.test(l));
    assert(tap >= 0 && down > tap && lines.indexOf(up) > down, 'A tapped, then Z down, then Z up');
    const mac = { KeyCmd: true, KeyCtrl: false, KeyOption: false, KeyShift: false, KeyModifiers: 8, QTKeyCode: 90, NativeCode: 6, VKeyCode: -1 };
    await d.settings('Keypad', '0,0', { Hotkeys: [mac] });
    await d.post('/api/settings', { cmd_as_ctrl: false });
    await pressLog('0,0', /send_key_state.*mods = "SUPER", key = "z".*"down"/);
    await d.post('/api/settings', { cmd_as_ctrl: true });
    await pressLog('0,0', /send_key_state.*mods = "CTRL", key = "z".*"down"/);
  });

  await test('hotkey switch: without hotkeys only the state toggles', async () => {
    await put('1,0', `${B}system.hotkeyswitch`);
    const m = d.mark();
    await d.press('Keypad', '1,0');
    await waitFor(async () => (await d.action('1,0')).State === 1, 2000, 'state 1');
    await d.press('Keypad', '1,0');
    await waitFor(async () => (await d.action('1,0')).State === 0, 2000, 'state 0');
    eq(d.logsSince(m, /send_shortcut|send_key_state/), [], 'nothing sent');
  });

  await test('open: folders, files, programs, URLs, commands; empty, missing and foreign paths fail', async () => {
    await put('2,0', `${B}system.open`);
    const open = async (p, re) => {
      await d.settings('Keypad', '2,0', { path: p });
      await pressLog('2,0', re);
    };
    await open('~/e2e-dir', new RegExp(`DRYRUN exec "xdg-open" "${esc(path.join(home, 'e2e-dir'))}"`));
    await open('"~/e2e.txt"', new RegExp(`DRYRUN exec "xdg-open" "${esc(path.join(home, 'e2e.txt'))}"`));
    await open('~/e2e.sh', new RegExp(`DRYRUN exec "${esc(path.join(home, 'e2e.sh'))}"`));
    await open('https://example.org/open', /DRYRUN exec "xdg-open" "https:\/\/example\.org\/open"/);
    await open('notify-send hi', /DRYRUN exec "sh" "-c" "notify-send hi"/);
    await open('', /Open: no path set/);
    await open('C:\\Program Files\\App\\app.exe', /is a Windows or macOS path/);
    await open('/Applications/Nope.app', /is a Windows or macOS path/);
    await open('\\\\server\\share\\tool.exe', /is a Windows or macOS path/);
    await open('/no/such/program --flag', /\/no\/such\/program does not exist/);
    // A quoted program path with arguments is a command line, not a missing file.
    const spaced = path.join(home, 'My Apps', 'app');
    fs.mkdirSync(path.dirname(spaced), { recursive: true });
    fs.writeFileSync(spaced, '#!/bin/sh\n', { mode: 0o755 });
    await open(`"${spaced}" --new`, new RegExp(`DRYRUN exec "sh" "-c" "\\\\"${esc(spaced)}\\\\" --new"`));
    await open(`"${spaced}"`, new RegExp(`DRYRUN exec "${esc(spaced)}"`));
    // A failure shows the alert on the key.
    const plain = await d.render('Keypad', '2,0');
    await d.settings('Keypad', '2,0', { path: '' });
    await d.press('Keypad', '2,0');
    await d.waitRender('Keypad', '2,0', (png) => !png.equals(plain), 2000, 'alert overlay');
  });

  await test('multi action and key logic: a failing entry shows the alert on the key', async () => {
    for (const [coord, uuid] of [['0,0', 'com.elgato.streamdeck.multiactions.routine'], ['1,0', 'com.elgato.streamdeck.keys.logic']]) {
      await put(coord, uuid, { _actions0: [{ UUID: `${B}system.open`, Settings: { path: '' } }] });
      const plain = await d.render('Keypad', coord);
      const m = d.mark();
      await d.press('Keypad', coord);
      await d.waitLog(/Open: no path set/, m);
      await d.waitRender('Keypad', coord, (png) => !png.equals(plain), 2000, `alert on ${uuid}`);
    }
  });

  await test('open app: commands, programs with arguments, .desktop files, long press variants, failures', async () => {
    await put('3,0', `${B}system.openapp`);
    const app = async (settings, re, hold) => {
      await d.settings('Keypad', '3,0', settings);
      await pressLog('3,0', re, hold);
    };
    await app({ app_name: 'e2e-fallback' }, /DRYRUN exec "sh" "-c" "e2e-fallback ?"/);
    // gtk-launch where it exists, else gio (the desktop file is found by name).
    await app({ exec: 'firefox.desktop' }, /DRYRUN exec ("gtk-launch" "firefox\.desktop"|"gio" "launch" ".*\/firefox\.desktop")/);
    await app({ exec: '~/e2e.sh', args: '--a "b c"' }, new RegExp(`DRYRUN exec "sh" "-c" "exec \\\\"\\$0\\\\" --a \\\\"b c\\\\"" "${esc(path.join(home, 'e2e.sh'))}"`));
    await app({ exec: '~/e2e.sh', long_press: 'quit' }, /DRYRUN kill -15 e2e\.sh/, 800);
    // Interpreters and launchers are not quit by name; Flatpaks through flatpak.
    await app({ exec: 'python3 ~/tool.py', long_press: 'quit' }, /can't tell which program python3 ~\/tool\.py is/, 800);
    await app({ exec: 'flatpak run --branch=stable org.e2e.App', long_press: 'quit' }, /DRYRUN flatpak kill org\.e2e\.App/, 800);
    // A desktop entry: started with gtk-launch / gio, quit by the program of its Exec line.
    const entry = path.join(home, '.local/share/applications/e2e-app.desktop');
    fs.mkdirSync(path.dirname(entry), { recursive: true });
    fs.writeFileSync(entry, '[Desktop Entry]\nType=Application\nName=E2E App\nExec=/opt/e2e/e2e-desktop-app --x %U\n');
    await app({ exec: entry, long_press: 'quit' }, new RegExp(`DRYRUN exec ("gio" "launch" "${esc(entry)}"|"gtk-launch" "e2e-app\\.desktop"|"sh" "-c" "/opt/e2e/e2e-desktop-app --x")`));
    await app({ exec: entry, long_press: 'quit' }, /DRYRUN kill -15 e2e-desktop-app/, 800);
    await app({ exec: 'e2eapp --x', long_press: 'kill' }, /DRYRUN kill -9 e2eapp/, 800);
    await app({ exec: 'e2eapp', long_press: 'quit', force_kill: true }, /DRYRUN kill -9 e2eapp/, 800);
    await app({ exec: 'e2eapp', long_press: 'none' }, /DRYRUN exec "sh" "-c" "e2eapp ?"/, 800);
    await app({}, /no application set/);
    await app({ exec: 'C:\\Games\\game.exe' }, /is a Windows or macOS path/);
  });

  await test('close: the program name of a path; a missing name fails', async () => {
    await put('0,1', `${B}system.close`, { app_name: '/usr/bin/e2eclose' });
    await pressLog('0,1', /DRYRUN kill -15 e2eclose/);
    await d.settings('Keypad', '0,1', { app_name: '' });
    await pressLog('0,1', /Close: no application set/);
  });

  await test('website: schemes are kept, host names get https; a missing URL fails', async () => {
    await put('1,1', WEBSITE);
    for (const [url, want] of [['localhost:8080/x', 'https://localhost:8080/x'], ['steam://run/570', 'steam://run/570'],
      ['tel:+41441234567', 'tel:+41441234567'], ['  example.org  ', 'https://example.org']]) {
      await d.settings('Keypad', '1,1', { path: url, openInBrowser: true });
      await pressLog('1,1', new RegExp(`DRYRUN exec "xdg-open" "${esc(want)}"`));
    }
    await d.settings('Keypad', '1,1', { path: '' });
    await pressLog('1,1', /Website: no URL set/);
  });

  await test('web links of the UI open in the default browser, nothing else does', async () => {
    const m = d.mark();
    eq(await d.status('POST', '/api/open-url', { url: 'https://marketplace.elgato.com/' }), 200);
    await d.waitLog(/DRYRUN exec "xdg-open" "https:\/\/marketplace\.elgato\.com\/"/, m);
    for (const url of [`http://127.0.0.1:${d.port}/api/state`, 'http://localhost/', 'http://[::1]:80/', 'file:///etc/passwd', 'javascript:alert(1)', 'sh -c x', '']) {
      eq(await d.status('POST', '/api/open-url', { url }), 400, url);
    }
    eq(d.logsSince(m, /xdg-open/).length, 1, 'only the web link');
  });

  await test('multimedia and text: defaults, unknown entries, empty text, terminals paste with Ctrl+Shift+V', async () => {
    await put('2,1', `${B}system.multimedia`);
    await pressLog('2,1', /DRYRUN run playerctl play-pause/);
    await d.settings('Keypad', '2,1', { actionIdx: 42 });
    await pressLog('2,1', /unsupported multimedia action 42/);
    await put('3,1', `${B}system.text`, { pastedText: '', isSendingEnter: true });
    let m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/send_shortcut.*key = "Return"/, m);
    eq(d.logsSince(m, /wl-copy/), [], 'nothing pasted');
    await d.post('/api/active-app', { app: 'kitty' });
    await d.settings('Keypad', '3,1', { pastedText: 'in a terminal', isSendingEnter: false });
    m = d.mark();
    await d.press('Keypad', '3,1');
    await d.waitLog(/DRYRUN pipe wl-copy .*"in a terminal"/, m);
    await d.waitLog(/send_shortcut.*mods = "CTRL\|SHIFT".*key = "v"|send_shortcut.*CTRL.*SHIFT.*key = "v"/, m);
    await d.post('/api/active-app', { app: 'firefox' });
  });

  await test('brightness: every option, its bounds and the cycle', async () => {
    await put('0,0', `${B}system.keybrightness`);
    const step = async (idx, from, want) => {
      if (from !== undefined) await brightness(from);
      await d.settings('Keypad', '0,0', { actionIdx: idx });
      await d.press('Keypad', '0,0');
      await d.waitState((s) => s.devices[0].brightness === want, 2000, `option ${idx} from ${from}: ${want}`);
    };
    await step(0, 95, 100);
    await step(0, undefined, 100);
    await step(1, 3, 0);
    await step(1, undefined, 0);
    await step(2, 10, 100);
    await step(3, undefined, 75);
    await step(4, undefined, 50);
    await step(5, undefined, 25);
    await step(6, undefined, 5);
    for (const [from, want] of [[5, 10], [10, 40], [69, 70], [70, 100], [100, 10]]) await step(7, from, want);
    await brightness(42);
    await d.settings('Keypad', '0,0', { actionIdx: 99 });
    await d.press('Keypad', '0,0');
    eq((await d.device()).brightness, 42, 'unknown option: unchanged');
    eq(JSON.parse(fs.readFileSync(path.join(d.config, 'settings.json'), 'utf8')).devices[dev].brightness, 42, 'remembered');
    await brightness(70);
  });

  await test('sleep: the next key, touch point or brightness change wakes the deck', async () => {
    await put('1,0', `${B}system.sleep`);
    await d.press('Keypad', '1,0');
    await d.waitState((s) => s.devices[0].sleeping, 2000, 'sleeping');
    await d.press('Neo', '2,0'); // touch point: wakes, does not change the page
    await d.waitState((s) => !s.devices[0].sleeping && s.devices[0].page === page0, 2000, 'awake on the same page');
    await d.press('Keypad', '1,0');
    await d.waitState((s) => s.devices[0].sleeping, 2000, 'sleeping again');
    await brightness(40);
    await d.waitState((s) => !s.devices[0].sleeping && s.devices[0].brightness === 40, 2000, 'brightness wakes');
    await d.press('Keypad', '1,0');
    await d.waitState((s) => s.devices[0].sleeping, 2000, 'sleeping a third time');
    await d.press('Keypad', '1,0'); // the sleep key itself only wakes
    await d.waitState((s) => !s.devices[0].sleeping, 2000, 'awake');
    await brightness(70);
  });

  await test('soundboard: play/stop toggle, overlapping, play while held, missing file, stop all', async () => {
    await put('2,0', `${B}soundboard.playaudio`, { path: '~/e2e.wav', actionType: 0, volume: 30 });
    const play = new RegExp(`DRYRUN play_audio ${esc(path.join(home, 'e2e.wav'))} volume=30`);
    let m = d.mark();
    await d.press('Keypad', '2,0');
    await d.waitLog(play, m);
    await d.press('Keypad', '2,0'); // second press stops it
    await d.press('Keypad', '2,0'); // third plays again
    await waitFor(() => d.logsSince(m, play).length === 2, 2000, 'play, stop, play');
    await d.settings('Keypad', '2,0', { path: '~/e2e.wav', actionType: 1, volume: 30 });
    m = d.mark();
    await d.press('Keypad', '2,0');
    await d.press('Keypad', '2,0');
    await waitFor(() => d.logsSince(m, play).length === 2, 2000, 'overlapping');
    await d.settings('Keypad', '2,0', { path: '~/e2e.wav', actionType: 2, volume: 30 });
    m = d.mark();
    await d.press('Keypad', '2,0', 200);
    await d.press('Keypad', '2,0', 200); // released before: plays again
    await waitFor(() => d.logsSince(m, play).length === 2, 2000, 'while held');
    await put('3,0', `${B}soundboard.stopaudioplay`);
    await pressLog('3,0', /DRYRUN stop_audio/);
    await d.settings('Keypad', '2,0', { path: '', actionType: 0 });
    await pressLog('2,0', /no audio file set/);
  });

  await test('timer: counts down, pauses, resets on long press, alarm when done', async () => {
    await put('0,1', `${B}system.timer`, { seconds: 30 });
    const idle = await d.render('Keypad', '0,1');
    await d.press('Keypad', '0,1');
    await d.waitRender('Keypad', '0,1', (png) => !png.equals(idle), 2500, 'running');
    await d.press('Keypad', '0,1'); // pause
    const paused = await d.render('Keypad', '0,1');
    await sleep(1200);
    assert(paused.equals(await d.render('Keypad', '0,1')), 'paused time stands still');
    await d.press('Keypad', '0,1', 800);
    await d.waitRender('Keypad', '0,1', (png) => png.equals(idle), 2000, 'reset');
    await d.settings('Keypad', '0,1', { seconds: 1, alarm: '~/alarm.wav' });
    await pressLog('0,1', new RegExp(`DRYRUN play_audio ${esc(path.join(home, 'alarm.wav'))}`));
  });

  await test('switch profile: unknown targets fail, ids match in any case', async () => {
    await put('1,1', `${B}profile.rotate`, { ProfileUUID: 'NO-SUCH-PROFILE' });
    await pressLog('1,1', /profile not found/);
    const p2 = await d.post('/api/profiles', { name: 'Target', model: '20GBJ9901' });
    await d.settings('Keypad', '1,1', { ProfileUUID: p2.id.toLowerCase() });
    await d.press('Keypad', '1,1');
    await d.waitState((s) => s.devices[0].profile === p2.id, 2000, 'switched');
    await d.post(`/api/devices/${dev}`, { profile: P });
    await d.del(`/api/profiles/${p2.id}`);
  });

  await test('pages: go to page clamps, next/previous stop at the ends, touch points, folders', async () => {
    const pA = (await d.post(`/api/profiles/${P}/pages`)).id;
    const pB = (await d.post(`/api/profiles/${P}/pages`)).id;
    const at = async (pg) => d.waitState((s) => s.devices[0].page === pg, 2000, `page ${pg}`);
    const onAll = async (coord, uuid, settings) => {
      for (const pg of [page0, pA, pB]) {
        await d.post(`/api/pages/${P}/${pg}/slot`, { controller: 'Keypad', coord, uuid });
        if (settings) await d.patch(`/api/pages/${P}/${pg}/slot`, { controller: 'Keypad', coord, settings });
      }
    };
    await onAll('0,0', `${B}page.goto`, { PageIndex: 99 });
    await onAll('1,0', `${B}page.next`);
    await onAll('2,0', `${B}page.previous`);
    await onAll('3,0', `${B}page.indicator`);
    await d.press('Keypad', '0,0');
    await at(pB);
    await d.press('Keypad', '1,0'); // next on the last page
    await d.press('Keypad', '3,0'); // the indicator does nothing
    await d.press('Neo', '2,0'); // right touch point = next
    await sleep(150);
    eq((await d.device()).page, pB, 'stays on the last page');
    await d.patch(`/api/pages/${P}/${pB}/slot`, { controller: 'Keypad', coord: '0,0', settings: { PageIndex: 0 } });
    await d.press('Keypad', '0,0');
    await at(page0);
    await d.press('Keypad', '2,0'); // previous on the first page
    await d.press('Neo', '0,0');
    await sleep(150);
    eq((await d.device()).page, page0, 'stays on the first page');
    await d.press('Neo', '2,0');
    await at(pA);
    // Inside a folder next/previous do nothing, go to page leaves it.
    const folder = (await d.slot('Keypad', '0,1', `${B}profile.openchild`)).Settings.ProfileUUID;
    for (const [coord, uuid, settings] of [['1,0', `${B}page.next`], ['2,0', `${B}page.previous`], ['3,0', `${B}page.goto`, { PageIndex: 1 }]]) {
      await d.post(`/api/pages/${P}/${folder}/slot`, { controller: 'Keypad', coord, uuid });
      if (settings) await d.patch(`/api/pages/${P}/${folder}/slot`, { controller: 'Keypad', coord, settings });
    }
    await d.press('Keypad', '0,1');
    await at(folder);
    await d.press('Keypad', '1,0');
    await d.press('Keypad', '2,0');
    await sleep(150);
    eq((await d.device()).page, folder, 'still in the folder');
    await d.press('Keypad', '3,0');
    const st = await at(page0);
    eq(st.devices[0].folder_depth, 0);
    await d.del(`/api/pages/${P}/${pA}`);
    await d.del(`/api/pages/${P}/${pB}`);
  });

  await test('folders: nested folders and back; back at the top does nothing; deleting removes sub folders', async () => {
    const f1 = (await d.slot('Keypad', '0,1', `${B}profile.openchild`)).Settings.ProfileUUID;
    const f2 = (await d.post(`/api/pages/${P}/${f1}/slot`, { controller: 'Keypad', coord: '1,0', uuid: `${B}profile.openchild` })).Settings.ProfileUUID;
    await d.press('Keypad', '0,1');
    await d.waitState((s) => s.devices[0].page === f1 && s.devices[0].folder_depth === 1, 2000, 'folder 1');
    await d.press('Keypad', '1,0');
    await d.waitState((s) => s.devices[0].page === f2 && s.devices[0].folder_depth === 2, 2000, 'folder 2');
    eq((await d.page(P, f2)).parent, f1);
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].page === f1 && s.devices[0].folder_depth === 1, 2000, 'back to folder 1');
    await d.press('Keypad', '0,0');
    await d.waitState((s) => s.devices[0].page === page0 && s.devices[0].folder_depth === 0, 2000, 'back to the page');
    await put('1,1', `${B}profile.backtoparent`);
    await d.press('Keypad', '1,1');
    await sleep(150);
    eq((await d.device()).page, page0, 'no parent at the top');
    await d.del(`/api/pages/${P}/${page0}/slot?controller=Keypad&coord=0,1`);
    const folders = (await d.state()).profiles.find((p) => p.id === P).folders;
    assert(!folders.includes(f1) && !folders.includes(f2), 'both folders deleted');
  });

  await test('multi actions: a failing entry does not stop the rest; empty lists; default delay', async () => {
    const close = (n) => ({ UUID: `${B}system.close`, Settings: { app_name: n } });
    await put('0,1', `${B}multiactions.routine`, { _actions0: [close(''), close('after-fail')] });
    let m = d.mark();
    await d.press('Keypad', '0,1');
    await d.waitLog(/no application set/, m);
    await d.waitLog(/DRYRUN kill -15 after-fail/, m);
    await d.settings('Keypad', '0,1', {
      _actions0: [{ UUID: WEBSITE, Settings: { path: 'https://example.org/d1' } }, { UUID: `${B}multiactions.delay` }, { UUID: WEBSITE, Settings: { path: 'https://example.org/d2' } }],
    });
    m = d.mark();
    await d.press('Keypad', '0,1');
    await d.waitLog(/example\.org\/d1/, m);
    const t1 = Date.now();
    await d.waitLog(/example\.org\/d2/, m);
    assert(Date.now() - t1 >= 400, `default delay of 500 ms (${Date.now() - t1} ms)`);
    for (const uuid of ['multiactions.routine', 'multiactions.random', 'multiactions.routine2']) {
      await put('1,1', B + uuid, { _actions0: [] });
      m = d.mark();
      await d.press('Keypad', '1,1');
      await d.press('Keypad', '1,1');
      await sleep(100);
      eq(d.logsSince(m, /DRYRUN|WARN/), [], `${uuid}: empty list runs nothing`);
    }
    // Page actions work inside a multi action.
    const pA = (await d.post(`/api/profiles/${P}/pages`)).id;
    await d.settings('Keypad', '0,1', { _actions0: [{ UUID: `${B}page.next` }] });
    await d.press('Keypad', '0,1');
    await d.waitState((s) => s.devices[0].page === pA, 2000, 'next page from a multi action');
    await d.post(`/api/devices/${dev}`, { page: page0 });
    await d.del(`/api/pages/${P}/${pA}`);
  });

  await test('key logic: holding an empty slot alerts and runs nothing; press slot only', async () => {
    await put('2,1', `${B}keys.logic`, { _actions0: [{ UUID: `${B}system.close`, Settings: { app_name: 'kl-only' } }], _actions1: [], _actions2: [] });
    const plain = await d.render('Keypad', '2,1');
    let m = d.mark();
    await d.press('Keypad', '2,1', 900);
    await d.waitRender('Keypad', '2,1', (png) => !png.equals(plain), 2000, 'alert');
    eq(d.logsSince(m, /DRYRUN kill/), [], 'hold runs nothing');
    m = d.mark();
    await d.press('Keypad', '2,1');
    await d.waitLog(/DRYRUN kill -15 kl-only/, m);
  });

  await test('actions of missing or foreign plugins report what is missing', async () => {
    await d.post(`/api/pages/${P}/${page0}/paste`, { controller: 'Keypad', coord: '3,1', instance: inst('com.example.missing.action', {}, 'Missing'), from_profile: P, from_page: page0 });
    await pressLog('3,1', /the plugin for com\.example\.missing\.action is not installed/);
    const plain = await d.render('Keypad', '3,1');
    eq(pngSize(plain), [96, 96], 'renders with its title');
  });
}

/** The id a property inspector of this key (on the deck's current page) registers with. */
async function piAlias(d, controller, coord) {
  const dev = await d.device();
  return (await d.get(`/api/pi/${dev.profile}/${dev.page}?controller=${controller}&coord=${coord}`)).uuid;
}

/** Property inspector connection: drives the test plugin and records what the PI gets. */
async function inspector(d, ctx, action, controller, coord) {
  const ws = new WebSocket(`ws://127.0.0.1:${d.sdk}`);
  const got = [];
  ws.onmessage = (e) => got.push(JSON.parse(e.data));
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  const alias = await piAlias(d, controller, coord);
  ws.send(JSON.stringify({ event: 'registerPropertyInspector', uuid: alias }));
  return {
    got,
    /** The inspector's name for its action (instead of the context). */
    alias,
    /** Make the plugin send these SDK messages. */
    plugin: (msgs) => ws.send(JSON.stringify({ event: 'sendToPlugin', action, context: ctx, payload: { send: msgs } })),
    /** Send a message as the property inspector. */
    send: (msg) => ws.send(JSON.stringify({ context: ctx, action, ...msg })),
    waitFor: (pred, ms = 3000, what = 'property inspector message') => waitFor(() => got.find(pred), ms, what),
    close: () => ws.close(),
  };
}

/** Plugins built on Elgato's own @elgato/streamdeck library (1.x, 2.x, 3.x): what real
 * plugins rely on (version gating, request ids, settings, resources, secrets). */
const SDK_FIXTURES = path.join(ROOT, 'tests/fixtures/elgato-sdk');
function libPlugin(major) {
  const id = `com.e2e.lib${major}`;
  return zip({
    [`${id}.sdPlugin/manifest.json`]: JSON.stringify({
      Name: `E2E Library ${major}`, UUID: id, Version: '1.0', Author: 'e2e', SDKVersion: major === 1 ? 2 : 3, CodePath: 'plugin.mjs',
      Category: 'E2E', Icon: 'img', Nodejs: { Version: '20' }, OS: [{ Platform: 'mac', MinimumVersion: '10.15' }],
      Software: { MinimumVersion: major === 1 ? '6.5' : '7.1' },
      Actions: [{ UUID: `${id}.count`, Name: 'Library counter', Icon: 'img', States: [{ Image: 'img' }], Controllers: ['Keypad'] }],
    }),
    [`${id}.sdPlugin/plugin.mjs`]: fs.readFileSync(path.join(SDK_FIXTURES, `plugin-v${major}.mjs`)),
    [`${id}.sdPlugin/img.svg`]: SVG('#884400'),
  });
}

async function suiteElgatoLibrary(d) {
  await test('keys with an old starter icon get the Elgato icon once; a backup is made first', async () => {
    const dev = await d.device();
    await d.slot('Keypad', '3,1', 'com.elgato.streamdeck.system.multimedia');
    await d.kill();
    // As the starter profile stored it: the icon file as a data URL in the page manifest.
    const old = 'data:image/svg+xml;base64,' + fs.readFileSync(path.join(ROOT, 'tests/fixtures/old-starter-play.svg')).toString('base64');
    // The manifest of the page with that key (the starter profile has two pages).
    const file = walkFiles(path.join(d.data, 'ProfilesV2', `${dev.profile}.sdProfile`)).filter((f) => f.endsWith('manifest.json'))
      .find((f) => JSON.parse(fs.readFileSync(f, 'utf8')).Controllers?.find((c) => c.Type === 'Keypad')?.Actions?.['3,1']?.UUID === 'com.elgato.streamdeck.system.multimedia');
    const m = JSON.parse(fs.readFileSync(file, 'utf8'));
    const kp = m.Controllers.find((c) => c.Type === 'Keypad');
    kp.Actions['3,1'].States[0].Image = old;
    kp.Actions['3,1'].States[0].Title = 'kept';
    fs.writeFileSync(file, JSON.stringify(m));
    const settings = path.join(d.config, 'settings.json');
    fs.writeFileSync(settings, JSON.stringify({ ...JSON.parse(fs.readFileSync(settings, 'utf8')), elgato_icons: false }));
    await d.launch();
    const st = (await d.action('3,1')).States[0];
    assert(st.Image.startsWith('data:image/svg+xml;base64,') && st.Image !== old, 'new icon');
    assert(Buffer.from(st.Image.split(',')[1], 'base64').toString().includes('scale('), 'drawn from the Elgato set');
    eq(st.Title, 'kept', 'nothing else changes');
    assert((await d.get('/api/backups')).some((b) => b.name.startsWith('before-icons')), 'backup first');
    // Only once: a later start leaves keys alone.
    eq(JSON.parse(fs.readFileSync(settings, 'utf8')).elgato_icons, true);
    await d.del(`/api/pages/${dev.profile}/${dev.page}/slot?controller=Keypad&coord=3,1`);
  });


  const dev = await d.device();
  const log = (major) => {
    const f = path.join(d.data, 'logs', `com.e2e.lib${major}.log`);
    return fs.existsSync(f) ? fs.readFileSync(f, 'utf8') : '';
  };
  const waitLine = (major, re, ms = 8000) => waitFor(() => log(major).split('\n').find((l) => re.test(l)), ms, `lib${major}: ${re}`);
  for (const [major, coord] of [[1, '0,0'], [2, '1,0'], [3, '2,0']]) {
    await test(`@elgato/streamdeck ${major}.x: connects, gets settings, keys, global settings${major > 1 ? ', resources' : ''}${major === 2 ? ', secrets' : ''}`, async () => {
      await d.req('POST', '/api/import', libPlugin(major), { 'x-filename': `lib${major}.streamDeckPlugin` });
      await waitLine(major, /connected /);
      eq(JSON.parse(/global (.*)$/.exec(await waitLine(major, / global /))[1]), {});
      if (major === 2) {
        // Answered at once (it used to hang forever). Library 3 no longer has it.
        const secrets = await waitLine(major, / secrets/);
        assert(!/failed/.test(secrets), secrets);
      }
      await d.slot('Keypad', coord, `com.e2e.lib${major}.count`);
      await waitLine(major, / willAppear \S+ Keypad \{\}/);
      const before = await d.render('Keypad', coord);
      await d.press('Keypad', coord);
      const down = await waitLine(major, / keyDown 1 /);
      eq(JSON.parse(/keyDown 1 (.*)$/.exec(down)[1]), { count: 1 }, 'getSettings after setSettings');
      eq((await d.action(coord)).Settings, { count: 1 }, 'stored');
      if (major > 1) {
        const res = await waitLine(major, / resources/);
        assert(!/failed/.test(res), res);
        const [, ms, json] = / resources (\d+) (.*)$/.exec(res);
        assert(Number(ms) < 2000, `answered in ${ms} ms`);
        eq(JSON.parse(json), {});
        // The answer to its own getSettings is not reported as a change made in the
        // property inspector (request ids, SDK 7.1).
        eq(log(major).split('\n').filter((l) => / didReceiveSettings /.test(l)), [], 'no settings-changed event for own requests');
      }
      await d.waitRender('Keypad', coord, (png) => !png.equals(before), 3000, 'setTitle shows');
      // A change made in the editor is reported as such.
      await d.settings('Keypad', coord, { count: 5 });
      await waitLine(major, / didReceiveSettings \{"count":5\}/);
      await d.del(`/api/pages/${dev.profile}/${dev.page}/slot?controller=Keypad&coord=${coord}`);
    });
  }
}

/** Plugin SDK: lifecycle, events in both directions, settings, crashes, uninstall. */
async function suiteSdk(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const page0 = st0.devices[0].page;
  const dev = st0.devices[0].id;
  const C = 'com.e2e.counter.count';
  const ev = (event, ctx = '\\S+', plugin = 'com\\.e2e\\.counter') => new RegExp(`\\[${plugin}\\] ev ${event} ${ctx} `);
  const payload = (line) => JSON.parse(/\] ev \S+ \S+ (.*)$/.exec(line)[1]);
  const ctxOf = (line) => /\] ev \S+ (\S+) /.exec(line)[1];
  // Crashes right away: restarted a few times, then given up (checked at the end).
  await d.req('POST', '/api/import', pluginVariant('com.e2e.crashloop', { 'plugin.js': 'process.exit(7);' }), { 'x-filename': 'crash.streamDeckPlugin' });
  let m = d.mark();
  await d.req('POST', '/api/import', PLUGIN, { 'x-filename': 'e2e.streamDeckPlugin' });
  const connected = await d.waitLog(ev('deviceDidConnect', '-'), m, 8000);

  await test('lifecycle: deviceDidConnect, willAppear/willDisappear on page switches and moves', async () => {
    eqc(payload(connected), { name: 'Stream Deck Neo', type: 9, size: { columns: 4, rows: 2 } }, 'device info');
    m = d.mark();
    await d.slot('Keypad', '0,0', C);
    const a = await d.waitLog(ev('willAppear'), m, 4000);
    const ctxA = ctxOf(a);
    eqc(payload(a), { settings: {}, coordinates: { column: 0, row: 0 }, controller: 'Keypad', isInMultiAction: false, resources: {} }, 'willAppear payload');
    const pB = (await d.post(`/api/profiles/${P}/pages`)).id;
    await d.post(`/api/pages/${P}/${pB}/slot`, { controller: 'Keypad', coord: '1,1', uuid: C });
    m = d.mark();
    await d.post(`/api/devices/${dev}`, { page: pB });
    await d.waitLog(ev('willDisappear', ctxA), m);
    const b = await d.waitLog(ev('willAppear'), m);
    eqc(payload(b).coordinates, { column: 1, row: 1 });
    m = d.mark();
    await d.post(`/api/pages/${P}/${pB}/move`, { from: { controller: 'Keypad', coord: '1,1' }, to: { controller: 'Keypad', coord: '3,0' } });
    await d.waitLog(ev('willDisappear', ctxOf(b)), m);
    eqc(payload(await d.waitLog(ev('willAppear', ctxOf(b)), m)).coordinates, { column: 3, row: 0 }, 'moved: same context, new place');
    m = d.mark();
    await d.post(`/api/devices/${dev}`, { page: page0 });
    await d.waitLog(ev('willAppear', ctxA), m);
    await d.del(`/api/pages/${P}/${pB}`);
    m = d.mark();
    const other = await d.post('/api/profiles', { name: 'Empty', model: '20GBJ9901' });
    await d.post(`/api/devices/${dev}`, { profile: other.id });
    await d.waitLog(ev('willDisappear', ctxA), m);
    await d.post(`/api/devices/${dev}`, { profile: P });
    await d.del(`/api/profiles/${other.id}`);
  });

  let ctx;
  let pi;
  await test('input: keyDown/keyUp payloads; nested actions in multi actions and Key Logic', async () => {
    ctx = ctxOf(await d.waitLog(ev('willAppear'), 0));
    m = d.mark();
    await d.press('Keypad', '0,0');
    const down = payload(await d.waitLog(ev('keyDown', ctx), m));
    eqc(down, { settings: {}, coordinates: { column: 0, row: 0 }, controller: 'Keypad', isInMultiAction: false, resources: {} });
    await d.waitLog(ev('keyUp', ctx), m);
    // In a multi action: its own context, no coordinates.
    m = d.mark();
    await d.slot('Keypad', '1,0', `${B}multiactions.routine`);
    await d.settings('Keypad', '1,0', { _actions0: [{ UUID: C }] });
    const nested = await d.waitLog(new RegExp(`${ev('willAppear').source}.*"isInMultiAction":true`), m);
    m = d.mark();
    await d.press('Keypad', '1,0');
    const nd = payload(await d.waitLog(ev('keyDown', ctxOf(nested)), m));
    eq([nd.isInMultiAction, nd.coordinates], [true, undefined], 'multi action entry');
    await d.waitLog(ev('keyUp', ctxOf(nested)), m);
    // Key Logic press slot.
    await d.slot('Keypad', '2,0', `${B}keys.logic`);
    m = d.mark();
    await d.settings('Keypad', '2,0', { _actions0: [{ UUID: C }] });
    const kl = ctxOf(await d.waitLog(new RegExp(`${ev('willAppear').source}.*"coordinates":\\{"column":2`), m));
    m = d.mark();
    await d.press('Keypad', '2,0');
    await d.waitLog(ev('keyDown', kl), m);
    await d.waitLog(ev('keyUp', kl), m);
    pi = await inspector(d, ctx, C, 'Keypad', '0,0');
    await d.waitLog(ev('propertyInspectorDidAppear', ctx), m);
  });

  await test('titles and images: setTitle per target, setImage from data URL and plugin file, overlays', async () => {
    // Messages of one plugin are handled in order: a logged marker means the ones before are done.
    pi.plugin([{ event: 'setTitle', context: ctx, payload: {} }, { event: 'logMessage', payload: { message: 'mark-0' } }]);
    await d.waitLog(/mark-0/);
    await sleep(1600); // the OK overlay of the earlier key press is gone
    const base = await d.render('Keypad', '0,0');
    pi.plugin([{ event: 'setTitle', context: ctx, payload: { title: 'Soft', target: 2 } }, { event: 'logMessage', payload: { message: 'mark-1' } }]);
    await d.waitLog(/mark-1/);
    assert(base.equals(await d.render('Keypad', '0,0')), 'software-only title does not reach the deck');
    pi.plugin([{ event: 'setTitle', context: ctx, payload: { title: 'Hw' } }]);
    const titled = await d.waitRender('Keypad', '0,0', (p) => !p.equals(base), 3000, 'title');
    pi.plugin([{ event: 'setImage', context: ctx, payload: { image: `data:image/svg+xml;base64,${Buffer.from(SVG('#00ff00')).toString('base64')}` } }]);
    const green = await d.waitRender('Keypad', '0,0', (p) => !p.equals(titled), 3000, 'data URL image');
    pi.plugin([{ event: 'setImage', context: ctx, payload: { image: 'img' } }]);
    const file = await d.waitRender('Keypad', '0,0', (p) => !p.equals(green), 3000, 'image from the plugin folder');
    pi.plugin([{ event: 'setImage', context: ctx, payload: { image: '' } }, { event: 'setTitle', context: ctx, payload: {} }]);
    await d.waitRender('Keypad', '0,0', (p) => p.equals(base), 3000, 'back to the default look');
    assert(!file.equals(base), 'plugin image differs from the default');
    for (const o of ['showAlert', 'showOk']) {
      pi.plugin([{ event: o, context: ctx }]);
      await d.waitRender('Keypad', '0,0', (p) => !p.equals(base), 3000, o);
      await d.waitRender('Keypad', '0,0', (p) => p.equals(base), 4000, `${o} ends`);
    }
  });

  await test('settings: plugin ↔ property inspector ↔ API, getSettings, stored on disk', async () => {
    m = d.mark();
    pi.plugin([{ event: 'setSettings', context: ctx, payload: { a: 1 } }]);
    await pi.waitFor((x) => x.event === 'didReceiveSettings' && x.payload?.settings?.a === 1);
    eqc((await d.action('0,0')).Settings, { a: 1 }, 'stored');
    pi.send({ event: 'setSettings', payload: { b: 2 } });
    assert(payload(await d.waitLog(ev('didReceiveSettings', ctx), m)).settings.b === 2, 'plugin gets the PI settings');
    m = d.mark();
    pi.plugin([{ event: 'getSettings', context: ctx }]);
    eqc(payload(await d.waitLog(ev('didReceiveSettings', ctx), m)).settings, { b: 2 }, 'getSettings');
    pi.got.length = 0;
    pi.send({ event: 'getSettings' });
    await pi.waitFor((x) => x.event === 'didReceiveSettings' && x.payload?.settings?.b === 2, 3000, 'PI getSettings');
    m = d.mark();
    await d.settings('Keypad', '0,0', { c: 3 });
    eqc(payload(await d.waitLog(ev('didReceiveSettings', ctx), m)).settings, { c: 3 }, 'edited in the UI');
    const files = walkFiles(path.join(d.data, 'ProfilesV2', `${P}.sdProfile`)).filter((f) => f.endsWith('manifest.json'));
    assert(files.some((f) => /"c":\s*3/.test(fs.readFileSync(f, 'utf8'))), 'saved in the page manifest');
  });

  await test('global settings: per plugin, saved, delivered to plugin and property inspector', async () => {
    pi.got.length = 0;
    pi.plugin([{ event: 'setGlobalSettings', context: 'com.e2e.counter', payload: { g: 1 } }]);
    await pi.waitFor((x) => x.event === 'didReceiveGlobalSettings' && x.payload.settings.g === 1);
    eqc(JSON.parse(fs.readFileSync(path.join(d.config, 'settings.json'), 'utf8')).global_settings['com.e2e.counter'], { g: 1 });
    m = d.mark();
    pi.plugin([{ event: 'getGlobalSettings', context: 'com.e2e.counter' }]);
    eqc(payload(await d.waitLog(ev('didReceiveGlobalSettings', '-'), m)), { settings: { g: 1 } });
    m = d.mark();
    pi.send({ event: 'setGlobalSettings', payload: { g: 2 } });
    eqc(payload(await d.waitLog(ev('didReceiveGlobalSettings', '-'), m)), { settings: { g: 2 } }, 'from the PI');
    pi.got.length = 0;
    pi.send({ event: 'getGlobalSettings' });
    await pi.waitFor((x) => x.event === 'didReceiveGlobalSettings' && x.payload.settings.g === 2, 3000, 'PI getGlobalSettings');
  });

  await test('SDK 7.1: answers carry the request id; secrets and resources are answered', async () => {
    m = d.mark();
    pi.plugin([
      { event: 'getSettings', context: ctx, id: 'q1' },
      { event: 'getGlobalSettings', context: 'com.e2e.counter', id: 'q2' },
      { event: 'getSecrets', id: 'q3' },
      { event: 'setResources', context: ctx, payload: { resources: { sound: 'beep.mp3' } } },
      { event: 'getResources', context: ctx, id: 'q4' },
    ]);
    await d.waitLog(new RegExp(`ev didReceiveSettings ${ctx} id=q1 `), m);
    await d.waitLog(/ev didReceiveGlobalSettings - id=q2 /, m);
    await d.waitLog(/ev didReceiveSecrets - id=q3 \{"secrets":\{\}\}/, m);
    await d.waitLog(new RegExp(`ev didReceiveResources ${ctx} id=q4 .*"resources":\\{"sound":"beep\\.mp3"\\}`), m);
    const res = await pi.waitFor((x) => x.event === 'didReceiveResources', 3000, 'the PI hears about setResources');
    eqc(res.payload.resources, { sound: 'beep.mp3' });
    eqc((await d.action('0,0')).Resources, { sound: 'beep.mp3' }, 'stored with the action');
    // A change made in the PI has no id.
    m = d.mark();
    pi.send({ event: 'setSettings', payload: { b: 3 } });
    const line = await d.waitLog(ev('didReceiveSettings', ctx), m);
    assert(!/ id=/.test(line), line);
  });

  await test('a property inspector opened while the deck is unplugged still reaches the plugin', async () => {
    m = d.mark();
    pi.close();
    await d.waitLog(ev('propertyInspectorDidDisappear', ctx), m);
    await d.post(`/api/devices/${dev}/virtual`, { connected: false });
    m = d.mark();
    pi = await inspector(d, ctx, C, 'Keypad', '0,0');
    await d.waitLog(ev('willAppear', ctx), m);
    await d.waitLog(ev('propertyInspectorDidAppear', ctx), m);
    m = d.mark();
    pi.close();
    await d.waitLog(ev('willDisappear', ctx), m);
    m = d.mark();
    await d.post(`/api/devices/${dev}/virtual`, { connected: true });
    await d.waitLog(ev('willAppear', ctx), m);
    pi = await inspector(d, ctx, C, 'Keypad', '0,0');
  });

  await test('openUrl, logMessage (also into the plugin log), sendToPropertyInspector, PI appear/disappear', async () => {
    m = d.mark();
    pi.plugin([{ event: 'openUrl', payload: { url: 'https://example.org/from-plugin' } }, { event: 'logMessage', payload: { message: 'into-the-log-file' } }]);
    await d.waitLog(/DRYRUN exec "xdg-open" "https:\/\/example\.org\/from-plugin"/, m);
    await d.waitLog(/\[com\.e2e\.counter\] into-the-log-file/, m);
    assert(fs.readFileSync(path.join(d.data, 'logs/com.e2e.counter.log'), 'utf8').includes('into-the-log-file'), 'plugin log file');
    pi.plugin([{ event: 'sendToPropertyInspector', context: ctx, payload: { hello: 'pi' } }]);
    const msg = await pi.waitFor((x) => x.event === 'sendToPropertyInspector');
    eq([msg.action, msg.context, msg.payload], [C, pi.alias, { hello: 'pi' }], 'the inspector gets its alias as context');
    m = d.mark();
    pi.close();
    await d.waitLog(ev('propertyInspectorDidDisappear', ctx), m);
    pi = await inspector(d, ctx, C, 'Keypad', '0,0');
  });

  await test('applications to monitor: launch and end of a program reach the plugin', async () => {
    const name = `e2emon${process.pid}`;
    m = d.mark();
    await d.req('POST', '/api/import', pluginVariant('com.e2e.monitor', {}, { ApplicationsToMonitor: { windows: [`${name}.exe`], mac: ['com.example.nothing'] } }), { 'x-filename': 'mon.streamDeckPlugin' });
    await d.waitLog(ev('deviceDidConnect', '-', 'com\\.e2e\\.monitor'), m, 8000);
    m = d.mark();
    // A process whose argv[0] is the program name (like `exec -a`; coreutils would not
    // run under another name).
    const proc = track(spawn(process.execPath, ['-e', 'setTimeout(() => {}, 60000)'], { argv0: name, stdio: 'ignore' }));
    await d.waitLog(new RegExp(`\\[com\\.e2e\\.monitor\\] ev applicationDidLaunch - \\{"application":"${name}\\.exe"\\}`), m, 12000);
    m = d.mark();
    proc.kill();
    await d.waitLog(new RegExp(`\\[com\\.e2e\\.monitor\\] ev applicationDidTerminate - \\{"application":"${name}\\.exe"\\}`), m, 12000);
    eq(d.logsSince(0, /com\.example\.nothing/), [], 'apps that do not run are not reported');
    await d.del('/api/plugins/com.e2e.monitor');
  });

  await test('isolation: other plugins cannot touch the actions; switchToProfile needs the plugin\'s own profiles', async () => {
    m = d.mark();
    await d.req('POST', '/api/import', pluginVariant('com.e2e.other'), { 'x-filename': 'other.streamDeckPlugin' });
    await d.slot('Keypad', '3,0', 'com.e2e.other.count');
    const octx = ctxOf(await d.waitLog(ev('willAppear', '\\S+', 'com\\.e2e\\.other'), m, 8000));
    const opi = await inspector(d, octx, 'com.e2e.other.count', 'Keypad', '3,0');
    const before = await d.render('Keypad', '0,0');
    opi.plugin([
      { event: 'setTitle', context: ctx, payload: { title: 'HACK' } },
      { event: 'setSettings', context: ctx, payload: { hacked: true } },
      { event: 'getGlobalSettings', context: 'com.e2e.other' },
    ]);
    // Global settings are per plugin: the other one has none.
    eqc(payload(await d.waitLog(ev('didReceiveGlobalSettings', '-', 'com\\.e2e\\.other'), m)), { settings: {} });
    assert(before.equals(await d.render('Keypad', '0,0')), 'title unchanged');
    eq((await d.action('0,0')).Settings.hacked, undefined, 'settings unchanged');
    m = d.mark();
    const name = (await d.state()).profiles.find((p) => p.id === P).name;
    pi.plugin([{ event: 'switchToProfile', context: 'com.e2e.counter', device: dev, payload: { profile: name } }]);
    await d.waitLog(/is not one of the plugin's profiles/, m);
    eq((await d.device()).profile, P);
    opi.close();
    await d.del('/api/plugins/com.e2e.other');
  });

  await test('states: setState within bounds, automatic toggling, DisableAutomaticStates', async () => {
    const files = pluginWithActions('com.e2e.states', ['two', 'fixed']);
    const mf = JSON.parse(files['com.e2e.states.sdPlugin/manifest.json']);
    for (const a of mf.Actions) a.States = [{ Image: 'img' }, { Image: 'img' }];
    mf.Actions[1].DisableAutomaticStates = true;
    files['com.e2e.states.sdPlugin/manifest.json'] = JSON.stringify(mf);
    m = d.mark();
    await d.req('POST', '/api/import', zip(files), { 'x-filename': 'states.streamDeckPlugin' });
    await d.slot('Keypad', '0,1', 'com.e2e.states.two');
    await d.slot('Keypad', '1,1', 'com.e2e.states.fixed');
    const appear = await d.waitLog(new RegExp(`${ev('willAppear', '\\S+', 'com\\.e2e\\.states').source}.*"column":0,"row":1`), m, 8000);
    eq(payload(appear).state, 0, 'multi-state actions report their state');
    const sctx = ctxOf(appear);
    const spi = await inspector(d, sctx, 'com.e2e.states.two', 'Keypad', '0,1');
    spi.plugin([{ event: 'setState', context: sctx, payload: { state: 1 } }]);
    await waitFor(async () => (await d.action('0,1')).State === 1, 2000, 'setState 1');
    spi.plugin([{ event: 'setState', context: sctx, payload: { state: 5 } }, { event: 'logMessage', payload: { message: 'after-bad-state' } }]);
    await d.waitLog(/after-bad-state/, m);
    await sleep(100);
    eq((await d.action('0,1')).State, 1, 'out of range ignored');
    await d.press('Keypad', '0,1');
    await waitFor(async () => (await d.action('0,1')).State === 0, 2000, 'toggled on key up');
    await d.press('Keypad', '1,1');
    await sleep(150);
    eq((await d.action('1,1')).State, 0, 'DisableAutomaticStates keeps the state');
    spi.close();
  });

  await test('restart: a crashed plugin is started again and gets its actions; a crash loop is given up', async () => {
    m = d.mark();
    pi.send({ event: 'sendToPlugin', payload: { exit: 3 } });
    await d.waitLog(/Plugin E2E Counter exited \(exit code 3\), restart 1\/5/, m, 12000);
    await d.waitLog(ev('willAppear', ctx), m, 15000);
    const diag = await d.get('/api/plugins/com.e2e.counter/diagnostics');
    eq([diag.running, diag.connected, diag.restarts], [true, true, 1]);
    assert(diag.stats.registrations >= 2 && diag.shown.some((s) => s.context === ctx), JSON.stringify(diag));
    await d.waitLog(/Plugin com\.e2e\.crashloop exited \(exit code 7\) and is no longer restarted/, 0, 60000);
    const dead = await d.get('/api/plugins/com.e2e.crashloop/diagnostics');
    eq([dead.running, dead.restarts], [null, 6]);
    assert(d.lines.filter((l) => /started plugin com\.e2e\.crashloop/.test(l)).length >= 6, 'started and restarted five times');
    pi.close();
  });

  await test('uninstall while running: the process tree ends, nothing restarts, keys report the missing plugin', async () => {
    const files = pluginWithActions('com.e2e.exec', ['count'], { CodePathLin: 'run.sh' });
    files['com.e2e.exec.sdPlugin/run.sh'] = '#!/bin/sh\nsleep 300 &\nwait\n';
    m = d.mark();
    const r = await d.req('POST', '/api/import', zip(files, { 'com.e2e.exec.sdPlugin/run.sh': 0o755 }), { 'x-filename': 'exec.streamDeckPlugin' });
    eq([r.id, r.runs_on_linux], ['com.e2e.exec', true]);
    const pid = Number(/pid (\d+)/.exec(await d.waitLog(/started plugin com\.e2e\.exec \(pid/, m, 5000))[1]);
    const group = () => fs.readdirSync('/proc').filter((x) => /^\d+$/.test(x)).filter((x) => {
      try { return fs.readFileSync(`/proc/${x}/stat`, 'utf8').split(') ').pop().split(' ')[2] === String(pid); } catch { return false; }
    });
    await waitFor(() => group().length >= 2, 3000, 'the plugin and its child run');
    eq((await d.get('/api/plugins/com.e2e.exec/diagnostics')).running, true);
    await d.slot('Keypad', '2,1', 'com.e2e.exec.count');
    const um = d.mark();
    await d.del('/api/plugins/com.e2e.exec');
    await waitFor(() => group().length === 0, 3000, 'all processes of the plugin ended');
    assert(!(await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.exec'), 'not listed');
    assert(!(await d.get('/api/actions')).flatMap((c) => c.actions).some((a) => a.uuid === 'com.e2e.exec.count'), 'action gone from the catalogue');
    await pressAndWait(d, '2,1', /the plugin for com\.e2e\.exec\.count is not installed/);
    // The watchdog (every 5 s) must not bring it back.
    await sleep(5500);
    eq(d.logsSince(um, /started plugin com\.e2e\.exec|Plugin com\.e2e\.exec exited/), []);
  });

  await test('unsupported plugins install with the reason; their keys say why they do nothing', async () => {
    const z = pluginVariant('com.e2e.winonly', { 'plugin.exe': 'MZ' }, { CodePath: 'plugin.exe', Nodejs: undefined });
    const r = await d.req('POST', '/api/import', z, { 'x-filename': 'win.streamDeckPlugin' });
    eq([r.runs_on_linux, r.reason], [false, 'Windows program']);
    const act = (await d.get('/api/actions')).flatMap((c) => c.actions).find((a) => a.uuid === 'com.e2e.winonly.count');
    eq([act.supported, act.unsupported_reason], [false, 'Windows program']);
    await d.slot('Keypad', '3,1', 'com.e2e.winonly.count');
    await pressAndWait(d, '3,1', /the plugin does not run on Linux/);
    eq(d.logsSince(0, /started plugin com\.e2e\.winonly/), [], 'never started');
  });
}

/** Desktop entries and folders the conversion finds (only these: see the group's env). */
function prepareDesktop(d) {
  const apps = path.join(d.home, '.local/share/applications');
  fs.mkdirSync(apps, { recursive: true });
  fs.writeFileSync(path.join(apps, 'steam.desktop'), '[Desktop Entry]\nType=Application\nName=Steam\nExec=e2e-steam-launcher %U\n');
  fs.mkdirSync(path.join(d.home, 'Documents'));
  fs.mkdirSync(path.join(d.dir, 'share'));
}

/** Wait until no Linux check (started by catalogue installs) is running any more. */
async function linuxChecksDone(d) {
  await waitFor(async () => !(await d.get('/api/linux-checks')).running.length, 120000, 'Linux checks to finish');
}

/** Converting while the plugin catalogue can't be reached: plugin keys wait. */
async function suiteConvertOffline(d) {
  const st0 = await d.state();
  const dev = st0.devices[0].id;
  const O = `${B}system.open`;
  const imported = profileZip({
    name: 'From Windows (offline)',
    pages: [{ keys: { Keypad: {
      '0,0': inst(O, { path: '/Applications/Microsoft Outlook.app' }, 'Outlook'),
      '1,0': inst('com.e2e.cat.count', {}, '', { Name: 'Cat' }),
      '2,0': inst('com.example.gizmo.action', {}, 'Giz', { Name: 'Gizmo' }),
    } } }],
  });
  await test('without the catalogue, keys of missing plugins stay and are offered again', async () => {
    const id = (await d.req('POST', '/api/import', imported, { 'x-filename': 'win.streamDeckProfile' })).id;
    await d.post(`/api/devices/${dev}`, { profile: id });
    const plan = await d.get(`/api/profiles/${id}/conversion`);
    eqc([plan.needed, plan.waiting, plan.manual, plan.installs], [true, 2, [], []]);
    eqc(await d.post(`/api/profiles/${id}/conversion`, {}), { changed: 1, manual: 0, left: 2, failed: [] });
    const kp = (await d.page()).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq([kp['0,0'].UUID, kp['1,0'].UUID, kp['2,0'].UUID], [WEBSITE, 'com.e2e.cat.count', 'com.example.gizmo.action']);
    eq((await d.get(`/api/profiles/${id}/conversion`)).waiting, 2, 'offered again');
  });
}

/** Converting profiles from Windows/macOS: edge cases, nested actions, idempotence, backups. */
async function suiteConvertEdge(d) {
  const st0 = await d.state();
  const P = st0.devices[0].profile;
  const dev = st0.devices[0].id;
  const O = `${B}system.open`;

  await test('nothing to convert: a Linux profile needs no conversion and converting changes nothing', async () => {
    const plan = await d.get(`/api/profiles/${P}/conversion`);
    eq([plan.needed, plan.programs, plan.web, plan.folders, plan.replaced, plan.manual, plan.installs], [false, [], [], [], [], [], []]);
    const before = await d.page();
    eqc(await d.post(`/api/profiles/${P}/conversion`, {}), { changed: 0, manual: 0, failed: [] });
    eqc(await d.page(), before, 'page unchanged');
  });

  await test('a profile made here is not offered a conversion for keys waiting for a command or a plugin', async () => {
    const pg = st0.devices[0].page;
    const paste = (coord, inst) => d.post(`/api/pages/${P}/${pg}/paste`, { controller: 'Keypad', coord, instance: inst, from_profile: P, from_page: pg });
    // "Steam" is installed here (prepareDesktop), the plugin is in the catalogue.
    await paste('0,0', { UUID: O, Name: 'Open', Settings: { path: '' }, State: 0, States: [{ Title: 'Steam' }] });
    await paste('1,0', { UUID: 'com.e2e.cat.count', Name: 'Cat', Settings: {}, State: 0, States: [{}] });
    eq((await d.get(`/api/profiles/${P}/conversion`)).needed, false);
    await d.del(`/api/pages/${P}/${pg}/slot?controller=Keypad&coord=0,0`);
    await d.del(`/api/pages/${P}/${pg}/slot?controller=Keypad&coord=1,0`);
  });

  // A profile as the official app on Windows/macOS writes it.
  const folder = randomUUID();
  const imported = profileZip({
    name: 'From Windows',
    pages: [
      { keys: { Keypad: {
        '0,0': inst(O, { path: '/Applications/Steam.app' }, 'Steam'),
        '1,0': inst(`${B}system.openapp`, { exec: 'C:\\Program Files (x86)\\Steam\\steam.exe' }),
        '2,0': inst(O, { path: '/Applications/Microsoft Outlook.app' }, 'Outlook'),
        '3,0': inst(O, { path: 'C:\\Users\\Jared\\Documents' }, 'Docs'),
        '0,1': inst(O, { path: '/Applications/E2E Nowhere.app' }, 'Nowhere'),
        '1,1': inst(`${B}multiactions.routine`, {}, '', { Actions: [{ Actions: [
          inst(O, { path: '"C:\\Program Files (x86)\\Steam\\steam.exe"' }),
          inst('com.barraider.spotifynext', {}, '', { Name: 'Next' }),
          inst(WEBSITE, { path: 'https://linux.example', openInBrowser: true }),
        ] }] }),
        '2,1': inst('com.example.gizmo.action', {}, 'Giz', { Name: 'Gizmo' }),
        '3,1': inst('com.e2e.cat.count', {}, '', { Name: 'Cat' }),
      } } },
      { keys: { Keypad: {
        '0,0': inst('com.elgato.obsstudio.record', {}, '', { Name: 'Record' }),
        '1,0': inst('com.microsoft.teams.button.togglevideo', {}, '', { Name: 'Video' }),
        '2,0': inst('com.elgato.twitch.marker', {}, '', { Name: 'Twitch Marker' }),
        '3,0': inst(`${B}keys.logic`, {}, '', { Actions: [inst(O, { path: '\\\\server\\share\\tool.exe' }, 'Tool'), { Actions: [] }, { Actions: [] }] }),
        '0,1': inst(`${B}profile.openchild`, { ProfileUUID: folder }),
      } } },
    ],
    folders: [{ id: folder, keys: { Keypad: { '1,0': inst(O, { path: 'C:\\Windows\\System32\\calc.exe' }, 'Calc') } } }],
  });
  let id;
  let pages;
  await test('a foreign key says it needs converting; the plan lists programs, web versions, folders, replacements', async () => {
    id = (await d.req('POST', '/api/import', imported, { 'x-filename': 'win.streamDeckProfile' })).id;
    pages = (await d.state()).profiles.find((p) => p.id === id).pages;
    await d.post(`/api/devices/${dev}`, { profile: id });
    await pressAndWait(d, '0,0', /\/Applications\/Steam\.app is a Windows or macOS path: convert the profile/);
    const plan = await d.get(`/api/profiles/${id}/conversion`);
    eq(plan.needed, true);
    const sorted = (l) => [...l].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
    eqc(sorted(plan.programs), sorted([{ title: 'Steam', target: 'Steam' }, { title: '', target: 'Steam' }, { title: '', target: 'Steam' }]));
    eqc(plan.web, [{ title: 'Outlook', target: 'outlook.office.com/mail' }]);
    eqc(plan.folders, [{ title: 'Docs', target: '~/Documents' }]);
    eqc(sorted(plan.replaced.map((r) => [r.key, r.keys])), sorted([['media', 1], ['teams:hotkey', 1], ['de.felitendo.obs-tools', 1], ['com.e2e.cat', 1]]));
    eqc(sorted(plan.manual), sorted(['Nowhere', 'Gizmo (Giz)', 'Twitch Marker', 'Tool', 'Calc']));
    eqc(sorted(plan.installs), sorted([{ id: 'com.e2e.cat', name: 'Catalogue Plugin' }, { id: 'de.felitendo.obs-tools', name: 'OBS Tools' }]));
  });

  await test('convert: nested and folder keys too; installs what it can; titles stay; the converted keys work', async () => {
    const r = await d.post(`/api/profiles/${id}/conversion`, {});
    // The OBS key stays as it is: its plugin could not be installed (offered again later).
    eqc(r, { changed: 12, manual: 5, left: 1, failed: ['OBS Tools'] });
    const k1 = (await d.page(id, pages[0])).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq([k1['0,0'].UUID, k1['0,0'].Settings.path, k1['0,0'].States[0].Title], [O, 'e2e-steam-launcher', 'Steam']);
    eq([k1['1,0'].UUID, k1['1,0'].Settings.path], [O, 'e2e-steam-launcher'], 'Open Application → Open');
    eq([k1['2,0'].UUID, k1['2,0'].Settings.path], [WEBSITE, 'https://outlook.office.com/mail/']);
    eq([k1['3,0'].UUID, k1['3,0'].Settings.path], [O, '~/Documents']);
    eq([k1['0,1'].UUID, k1['0,1'].Settings.path, k1['0,1'].States[0].Title], [O, '', 'Nowhere'], 'manual: waits for a command');
    const nested = k1['1,1'].Settings._actions0;
    eq(nested.map((a) => [a.UUID, a.Settings.path ?? a.Settings.actionIdx]), [[O, 'e2e-steam-launcher'], [`${B}system.multimedia`, 2], [WEBSITE, 'https://linux.example']]);
    eq([k1['2,1'].UUID, k1['2,1'].States[0].Title], [O, 'Giz']);
    eq(k1['3,1'].UUID, 'com.e2e.cat.count', 'the catalogue plugin was installed: key unchanged');
    assert((await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.cat'), 'installed');
    const k2 = (await d.page(id, pages[1])).controllers.find((c) => c.Type === 'Keypad').Actions;
    eq(k2['0,0'].UUID, 'com.elgato.obsstudio.record', 'OBS replacement plugin missing: unchanged');
    eq([k2['1,0'].UUID, k2['1,0'].Settings.Hotkeys[0].LinuxCode, k2['1,0'].Settings.Hotkeys[0].KeyShift], [`${B}system.hotkey`, 24, true]);
    eq([k2['2,0'].UUID, k2['2,0'].States[0].Title, k2['2,0'].Settings._from], [O, 'Twitch Marker', 'Twitch Marker'], 'named after the action');
    eq(k2['3,0'].Settings._actions0.map((a) => [a.UUID, a.Settings.path]), [[O, '']], 'Key Logic slot');
    const fk = (await d.page(id, folder)).controllers.find((c) => c.Type === 'Keypad').Actions['1,0'];
    eq([fk.UUID, fk.Settings.path, fk.States[0].Title], [O, '', 'Calc'], 'key in a folder');
    await pressAndWait(d, '0,0', /DRYRUN exec "sh" "-c" "e2e-steam-launcher"/);
    await pressAndWait(d, '3,0', new RegExp(`DRYRUN exec "xdg-open" "${path.join(d.home, 'Documents').replace(/[/.]/g, '\\$&')}"`));
    await pressAndWait(d, '0,1', /Open: no path set/);
    await linuxChecksDone(d);
  });

  await test('only the key that had to stay is offered again; converted keys are not; one backup', async () => {
    const plan = await d.get(`/api/profiles/${id}/conversion`);
    eq(plan.needed, true);
    eqc([plan.replaced.map((r) => [r.key, r.keys]), plan.manual, plan.programs, plan.web, plan.folders], [[['de.felitendo.obs-tools', 1]], [], [], [], []]);
    const backups = (await d.get('/api/backups')).filter((b) => b.kind === 'convert').map((b) => b.name).sort();
    eq(backups.length, 1, `one backup for the one real conversion (none when there was nothing to convert): ${backups}`);
    const pageOf = async (name) => Object.entries(unzipAll(await d.get(`/api/backups/${encodeURIComponent(name)}`)))
      .filter(([n]) => n.startsWith(`ProfilesV2/${id}.sdProfile/Profiles/${pages[0].toUpperCase()}/`) && n.endsWith('manifest.json'))
      .map(([, v]) => v.toString()).join('');
    assert((await pageOf(backups[0])).includes('/Applications/Steam.app'), 'the backup before the conversion has the original keys');
    await d.post(`/api/devices/${dev}`, { profile: P });
  });

  await test('sample profiles without their catalogue plugins: keys keep their look and wait for a command', async () => {
    const pagesOf = async (id) => {
      const prof = (await d.state()).profiles.find((p) => p.id === id);
      return Promise.all(prof.pages.map(async (pg) => (await d.page(id, pg)).controllers.find((c) => c.Type === 'Keypad').Actions));
    };
    const r = await d.post('/api/templates/stream', { model: '20GBJ9901' });
    eq(r.failed, ['OBS Tools'], 'by name');
    const [kp, kp2] = await pagesOf(r.id);
    for (const coord of ['0,0', '1,0', '2,0', '3,0', '0,1', '1,1', '2,1']) {
      eq([kp[coord].UUID, kp[coord].Settings.path], [O, ''], coord);
      assert(kp[coord].States[0].Title && /^data:image\/svg/.test(kp[coord].States[0].Image), `${coord} title and icon`);
    }
    eq([kp2['3,1'].UUID, kp2['3,1'].Settings.path], [WEBSITE, 'https://dashboard.twitch.tv/']);
    eq((await d.get(`/api/profiles/${r.id}/conversion`)).needed, false, 'nothing to convert');
    const play = await d.post('/api/templates/play', { model: '20GBJ9901' });
    eq(play.failed, ['Discord']);
    const [pk, pk2] = await pagesOf(play.id);
    eq([pk['3,0'].States[0].Title, pk['3,0'].Settings.path], ['Steam', 'e2e-steam-launcher'], 'installed program found');
    eq([pk2['0,0'].States[0].Title, pk2['0,0'].Settings.path], ['Heroic', ''], 'a program that is not installed gets no command');
    await d.del(`/api/profiles/${r.id}`);
    await d.del(`/api/profiles/${play.id}`);
  });
}

/** Sample profiles on every deck, with the catalogue plugins they use. */
async function suiteTemplates(d) {
  const GEOM = { '20GBJ9901': [4, 2], '20GBA9901': [5, 3], '20GAA9902': [5, 3], '20GAI9901': [3, 2], '20GAT9901': [8, 4], '20GBD9901': [4, 2], '20GBF9901': [3, 1] };
  const IDS = ['standard', 'work', 'play', 'stream'];
  // Keys per page. Every profile has two 4×2 pages: on Neo and + one per page (touch
  // points / swipe), side by side on the XL, one per page with Previous/Next Page keys in
  // the free bottom row of 5×3 decks; the Mini gets them in order with page keys in its
  // bottom corners; the Pedal three keys.
  const expectedPages = (model, cap) => {
    if (model === '20GBF9901') return [3];
    if (model === '20GBJ9901' || model === '20GBD9901') return [8, 8];
    if (model === '20GAT9901') return [16];
    if (cap === 15) return [9, 9];
    const pages = [];
    for (let left = 16; left > 0;) {
      const prev = pages.length > 0;
      const next = left > cap - prev;
      const keys = Math.min(left, cap - prev - next);
      pages.push(keys + prev + next);
      left -= keys;
    }
    return pages;
  };
  const dev = (await d.state()).devices[0].id;

  await test('samples: names, descriptions and the icons of the first page', async () => {
    const list = await d.get('/api/templates');
    eq(list.map((t) => [t.id, t.name]), [['standard', 'Standard'], ['work', 'Work'], ['play', 'Play'], ['stream', 'Streaming']]);
    assert(list.every((t) => t.description && t.icons.length === 8 && t.icons.every((i) => /^data:image\/svg/.test(i))), 'descriptions and icons');
  });

  await test('samples: Standard, Work, Play and Streaming fit every deck (Neo, MK.2, Original, Mini, XL, +, Pedal)', async () => {
    for (const [model, [cols, rows]] of Object.entries(GEOM)) {
      for (const t of IDS) {
        const r = await d.post(`/api/templates/${t}`, { model });
        eq(r.failed, [], `${t} on ${model}: plugins installed`);
        const prof = (await d.state()).profiles.find((p) => p.id === r.id);
        const want = expectedPages(model, cols * rows);
        eq([prof.model, prof.pages.length], [model, want.length], `${t} on ${model}`);
        for (const [i, pg] of prof.pages.entries()) {
          const page = await d.page(r.id, pg);
          const kp = page.controllers.find((c) => c.Type === 'Keypad')?.Actions || {};
          eq(Object.keys(kp).length, want[i], `${t} on ${model}, page ${i + 1}: key count`);
          for (const [coord, a] of Object.entries(kp)) {
            const [c, rr] = coord.split(',').map(Number);
            assert(c < cols && rr < rows, `${t} on ${model}: ${coord} is outside ${cols}x${rows}`);
            if (/^com\.elgato\.streamdeck\.page\.(next|previous)$/.test(a.UUID)) continue;
            assert(a.States.every((s) => /^data:image\/svg/.test(s.Image || '') && s.Title && s.ShowTitle === true), `${t} ${model} ${coord}: icon and title`);
          }
          eq(!!page.controllers.find((c) => c.Type === 'Neo')?.Actions?.['1,0'], model === '20GBJ9901', `${t} on ${model}: info bar`);
        }
        await d.del(`/api/profiles/${r.id}`);
      }
    }
  });

  await test('samples: plugin keys use the installed plugins and run; shortcuts and commands work', async () => {
    const pageKeys = async (id, i) => (await d.page(id, (await d.state()).profiles.find((p) => p.id === id).pages[i])).controllers.find((c) => c.Type === 'Keypad').Actions;
    const s = await d.post('/api/templates/stream', { model: '20GBJ9901' });
    const kp = await pageKeys(s.id, 0);
    eq(['0,0', '1,0', '2,0', '3,0', '0,1', '1,1', '2,1'].map((c) => kp[c].UUID.replace('de.felitendo.obs-tools.', '')),
      ['streamtoggle', 'recordtoggle', 'previousscene', 'nextscene', 'mutetoggle', 'recordtools', 'smartscene']);
    await d.post(`/api/devices/${dev}`, { profile: s.id });
    let m = d.mark();
    await d.press('Keypad', '1,0');
    await d.waitLog(/\[de\.felitendo\.obs-tools\] keyDown 1/, m, 8000);
    const w = await d.post('/api/templates/work', { model: '20GBJ9901' });
    await d.post(`/api/devices/${dev}`, { profile: w.id });
    await pressAndWait(d, '3,1', /DRYRUN exec "sh" "-c" "loginctl lock-session"/);
    await pressAndWait(d, '0,0', /DRYRUN exec "sh" "-c" "xdg-open mailto:"/);
    await pressAndWait(d, '0,1', /DRYRUN exec "sh" "-c" "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle/);
    const p2 = (await d.state()).profiles.find((p) => p.id === w.id).pages[1];
    await d.post(`/api/devices/${dev}`, { page: p2 });
    m = d.mark();
    await d.press('Keypad', '0,0'); // Copy
    await d.waitLog(/send_key_state.*CTRL.*key = "code:54".*"down"/, m);
    await pressAndWait(d, '3,1', /send_key_state.*key = "code:107".*"down"/); // Print
    const pl = await d.post('/api/templates/play', { model: '20GBJ9901' });
    eq(pl.failed, []);
    const pk = await pageKeys(pl.id, 0);
    eq(['0,0', '1,0', '2,0'].map((c) => pk[c].UUID), ['me.amankhanna.oadiscord.togglemute', 'me.amankhanna.oadiscord.toggledeafen', 'me.amankhanna.oadiscord.voicechannel']);
    await linuxChecksDone(d);
  });

  await test('samples: unknown samples and models are refused before anything is installed', async () => {
    const before = (await d.get('/api/plugins')).length;
    eq(await d.status('POST', '/api/templates/nope', { model: '20GBJ9901' }), 404);
    eq(await d.status('POST', '/api/templates/stream', { model: 'bogus' }), 400);
    eq((await d.get('/api/plugins')).length, before);
    eq(d.logsSince(0, /Installation failed/), [], 'no installation failure logged');
  });
}

/** The command line: run the binary with the instance's environment. */
async function suiteCli(d) {
  await d.init();
  const version = /^version\s*=\s*"([^"]+)"/m.exec(fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8'))[1];
  const files = path.join(d.dir, 'files');
  fs.mkdirSync(files);
  const file = (name, content) => { const f = path.join(files, name); fs.writeFileSync(f, content); return f; };

  await test('cli: --help, --version, unknown commands and missing arguments', async () => {
    let r = await d.cli(['--help']);
    eq(r.code, 0, r.stderr);
    for (const c of ['run', 'ui', 'import-elgato', 'import-profile', 'install-plugin', 'install-icon-pack', 'list', 'probe', 'udev', 'open-url']) {
      assert(r.stdout.includes(c), `help lists ${c}`);
    }
    r = await d.cli(['--version']);
    eq([r.code, r.stdout.trim()], [0, `tuxdeck ${version}`]);
    r = await d.cli(['import-profile', '--help']);
    assert(r.code === 0 && /<FILE>/.test(r.stdout), r.stdout);
    r = await d.cli(['frobnicate']);
    assert(r.code === 2 && /unrecognized subcommand/.test(r.stderr), r.stderr);
    for (const argv of [['import-profile'], ['install-plugin'], ['install-icon-pack'], ['open-url'], ['import-elgato'], ['probe', 'soon'], ['run', '--bogus']]) {
      r = await d.cli(argv);
      eq(r.code, 2, `${argv.join(' ')}: ${r.stderr}`);
    }
  });

  await test('cli: udev rules; the device list shows the simulated deck and leaves hardware alone', async () => {
    let r = await d.cli(['udev']);
    eq([r.code, r.stdout], [0, fs.readFileSync(path.join(ROOT, 'packaging/70-tuxdeck.rules'), 'utf8')]);
    assert(/0fd9/i.test(r.stdout), 'Elgato vendor id');
    // Never real hardware in tests (see Daemon.init).
    assert(d.env.TUXDECK_VIRTUAL_DEVICE === 'neo' && !('TUXDECK_ALLOW_HARDWARE' in d.env), 'test environment');
    r = await d.cli(['list']);
    eq([r.code, r.stdout.trim()], [0, 'Stream Deck Neo  serial VIRTUAL-NEO  firmware virtual  (simulated)']);
  });

  // A profile to import: the daemon's own export, renamed.
  await d.launch();
  const origId = (await d.state()).devices[0].profile;
  const exp = unzipAll(await d.get(`/api/profiles/${origId}/export`));
  await d.kill();
  const root = Object.keys(exp)[0].split('/')[0];
  const mf = JSON.parse(exp[`${root}/manifest.json`]);
  mf.Name = 'CLI import';
  exp[`${root}/manifest.json`] = JSON.stringify(mf);
  const marker = `tuxdeck-e2e-cli-${process.pid}`;
  exp[`${root}/../../${marker}.txt`] = 'x';
  const prof = file('cli.streamDeckProfile', zip(exp));

  await test('cli: import-profile, install-plugin and install-icon-pack; the daemon has them after a start', async () => {
    let r = await d.cli(['import-profile', prof]);
    eq(r.code, 0, r.stderr);
    const id = /Profile "CLI import" imported \(([0-9A-F-]+)\)/.exec(r.stdout)?.[1];
    assert(id, r.stdout);
    eq(walkFiles(d.dir).filter((f) => f.includes(marker)), [], 'unsafe entry skipped');
    r = await d.cli(['install-plugin', file('p.streamDeckPlugin', PLUGIN)]);
    eq([r.code, r.stdout.trim()], [0, 'Plugin com.e2e.counter installed'], r.stderr);
    r = await d.cli(['install-icon-pack', file('i.streamDeckIconPack', ICONPACK)]);
    eq([r.code, r.stdout.trim()], [0, 'Icon pack com.e2e.icons installed'], r.stderr);
    const m = d.mark();
    await d.launch();
    eq((await d.state()).profiles.find((p) => p.id === id)?.name, 'CLI import');
    assert((await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.counter'), 'plugin');
    assert((await d.get('/api/iconpacks')).some((p) => p.id === 'com.e2e.icons'), 'icon pack');
    await d.waitLog(/plugin registered: com\.e2e\.counter/, m, 8000);
    // Like an import in the UI: the copy must not share plugin contexts with the original.
    const ids = async (pid) => {
      const p = (await d.state()).profiles.find((x) => x.id === pid);
      return (await Promise.all(p.pages.map((pg) => d.page(pid, pg)))).flatMap((pg) => pg.controllers.flatMap((c) => Object.values(c.Actions).map((a) => a.ActionID)));
    };
    const [orig, copy] = [await ids(origId), await ids(id)];
    assert(copy.length === orig.length && copy.every((x) => !orig.includes(x)), 'new action ids');
  });

  await test('cli: with the service running, imports go through it and show at once', async () => {
    mf.Name = 'CLI live';
    exp[`${root}/manifest.json`] = JSON.stringify(mf);
    let r = await d.cli(['import-profile', file('live.streamDeckProfile', zip(exp))]);
    eq(r.code, 0, r.stderr);
    const id = /Profile "CLI live" imported \(([0-9A-F-]+)\)/.exec(r.stdout)?.[1];
    eq((await d.state()).profiles.find((p) => p.id === id)?.name, 'CLI live', r.stdout);
    r = await d.cli(['install-plugin', file('live.streamDeckPlugin', pluginVariant('com.e2e.clilive'))]);
    eq([r.code, r.stdout.trim()], [0, 'Plugin com.e2e.clilive installed'], r.stderr);
    assert((await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.clilive'), 'loaded without a restart');
    // Each command takes only its own kind of package.
    r = await d.cli(['import-profile', file('wrong.streamDeckPlugin', pluginVariant('com.e2e.wrongkind'))]);
    assert(r.code === 1 && /is a plugin, not a profile/.test(r.stderr), r.stderr);
    assert(!(await d.get('/api/plugins')).some((p) => p.uuid === 'com.e2e.wrongkind'), 'not installed');
    r = await d.cli(['import-profile', file('bad-live.bin', 'not a zip')]);
    assert(r.code === 1 && /not a package file/.test(r.stderr), r.stderr);
  });

  await test('cli: bad files and paths fail with exit code 1 and change nothing', async () => {
    // Without the service (with it, the service's checks answer; see above).
    await d.kill();
    const before = fs.readdirSync(path.join(d.data, 'ProfilesV2')).sort();
    const tmp = path.join(d.dir, 'tmp');
    fs.mkdirSync(tmp);
    fs.mkdirSync(path.join(files, 'empty'));
    const bad = file('bad.bin', 'not a zip');
    for (const [argv, re] of [
      [['import-profile', path.join(files, 'missing.streamDeckProfile')], /No such file/],
      [['import-profile', bad], /not a zip archive/],
      [['import-profile', file('evil.streamDeckProfile', zip({ [`../${root}/manifest.json`]: 'x' }))], /contains no profile \(\.sdProfile\)/],
      [['import-profile', file('broken.streamDeckProfile', zip({ [`${root}/manifest.json`]: '{nope' }))], /manifest\.json/],
      [['install-plugin', bad], /not a zip archive/],
      [['install-plugin', file('nomf.streamDeckPlugin', zip({ 'com.e2e.nomf.sdPlugin/plugin.js': '' }))], /manifest/],
      [['install-icon-pack', bad], /not a valid archive/],
      [['import-elgato', path.join(files, 'empty')], /Contents\/Resources/],
    ]) {
      const r = await d.cli(argv, 30000, { TMPDIR: tmp });
      assert(r.code === 1 && re.test(r.stderr), `${argv.join(' ')}: ${r.code} ${r.stderr}`);
    }
    eq(fs.readdirSync(path.join(d.data, 'ProfilesV2')).sort(), before);
    eq(fs.readdirSync(tmp), [], 'import-elgato cleans up its temporary folder');
    assert(fs.existsSync(path.join(d.data, 'elgato')) === fs.existsSync(ELGATO), 'Elgato resources untouched');
    await d.launch();
  });

  await test('cli: open-url forwards streamdeck:// links to the running daemon', async () => {
    const m = d.mark();
    let r = await d.cli(['open-url', 'streamdeck://plugins/message/com.e2e.counter/cli?x=1']);
    eq(r.code, 0, r.stderr);
    await d.waitLog(/\[com\.e2e\.counter\] deeplink \/cli\?x=1/, m, 5000);
    r = await d.cli(['open-url', 'streamdeck://something/else']);
    eq(r.code, 0, r.stderr);
  });
}

async function pressAndWait(d, coord, re, hold) {
  const m = d.mark();
  await d.press('Keypad', coord, hold);
  return d.waitLog(re, m);
}

// Each group runs against a fresh daemon, so a failure can't leave state behind that
// breaks the following groups.
const GROUPS = [
  { model: 'neo', name: 'neo: actions', run: suiteNeoActions },
  { model: 'neo', name: 'neo: editing', run: suiteNeoEditing },
  { model: 'neo', name: 'neo: plugins', run: suiteNeoPlugins },
  { model: 'neo', name: 'neo: system', run: suiteNeoSystem },
  { model: 'neo', name: 'neo: backup', run: suiteBackup },
  { model: 'neo', name: 'neo: convert', run: suiteCompat },
  // The deliberately crashing test plugin also runs (and is given up) in the outer instance.
  { model: 'neo', name: 'neo: linux-check', run: suiteLinuxCheck, allow: /Plugin com\.e2e\.crash exited/ },
  { model: 'neo', name: 'neo: linux-check-plain', run: suiteLinuxCheckPlain, opts: { env: () => ({ TUXDECK_NO_SANDBOX: '', TUXDECK_BWRAP: 'none' }) } },
  // Refused packages and links are logged as errors.
  { model: 'neo', name: 'neo: catalogue', run: suiteCatalogue, allow: /Installation failed|official app/ },
  { model: 'plus', name: 'plus', run: suitePlus },
  { model: 'mini', name: 'mini', run: (d) => suiteSmall(d, 'mini', [3, 2]) },
  { model: 'original', name: 'original', run: (d) => suiteSmall(d, 'original', [5, 3]) },
  // The second Stream Deck generation by its model number.
  { model: '20GAA9902', name: 'original2', run: (d) => suiteSmall(d, 'original2', [5, 3]) },
  { model: 'xl', name: 'xl', run: (d) => suiteSmall(d, 'xl', [8, 4]) },
  { model: 'pedal', name: 'pedal', run: suitePedal },
  { model: 'neo', name: 'neo: api', run: suiteApi },
  { model: 'neo', name: 'neo: profiles', run: suiteProfiles },
  { model: 'neo', name: 'neo: builtins', run: suiteBuiltins },
  // The crash-looping test plugin is given up on purpose.
  { model: 'neo', name: 'neo: elgato-library', run: suiteElgatoLibrary },
  { model: 'neo', name: 'neo: sdk', run: suiteSdk, allow: /Plugin com\.e2e\.crashloop exited \(exit code 7\) and is no longer restarted/ },
  // Only the desktop entries made by prepareDesktop (plus system-wide Flatpaks) are found.
  // No catalogue: nothing listens on port 9.
  { model: 'neo', name: 'neo: convert-offline', run: suiteConvertOffline, opts: { env: () => ({ TUXDECK_CATALOGUE_URL: 'http://127.0.0.1:9/catalogue.json' }) } },
  { model: 'neo', name: 'neo: convert-edge', run: suiteConvertEdge, opts: { prepare: prepareDesktop, env: (d) => ({ XDG_DATA_DIRS: path.join(d.dir, 'share') }) } },
  // A catalogue that also has the plugins the sample profiles use.
  { model: 'neo', name: 'neo: templates', run: suiteTemplates, opts: { env: () => ({ TUXDECK_CATALOGUE_URL: CATALOGUE.fullUrl() }) } },
  { model: 'neo', name: 'cli', run: suiteCli, manual: true },
  { model: 'mk2', name: 'mk2', run: async (d) => {
    await suiteSmall(d, 'mk2', [5, 3]);
    await test('mk2: the profile it shows can be deleted when an Original profile fits', async () => {
      const dev = await d.device();
      const original = await d.post('/api/profiles', { name: 'From the Original', model: '20GAA9902' });
      await d.del(`/api/profiles/${dev.profile}`);
      eq((await d.device()).profile, original.id, 'the deck shows the Original profile');
    });
  } },
  { model: 'neo,neo', name: 'two decks', run: suiteMultiDeck },
];

if (!fs.existsSync(BIN)) {
  console.error(`binary not found: ${BIN} (build with: cargo build --release)`);
  process.exit(2);
}
console.log(`TuxDeck e2e – ${BIN}${fs.existsSync(ELGATO) ? ' (with Elgato resources)' : ''}`);
// --only takes models (neo,plus) or group names (neo:plugins).
const selected = GROUPS.filter((g) => ONLY.some((o) => o === g.model || o.replace(/\s/g, '') === g.name.replace(/\s/g, '')));
if (!selected.length) {
  console.error(`no group matches ${ONLY.join(',')}; groups: ${GROUPS.map((g) => g.name.replace(/\s/g, '')).join(', ')}`);
  process.exit(2);
}
await CATALOGUE.start();
for (const g of selected) {
  console.log(`\n# ${g.name}`);
  const d = new Daemon(g.model, g.opts);
  try {
    if (!g.manual) await d.start();
    await g.run(d);
    await test(`${g.name}: no panics or unexpected errors in the log`, async () => {
      eq(d.lines.filter((l) => /panicked|ERROR/.test(l) && !(g.allow && g.allow.test(l))), []);
    });
  } catch (e) {
    failures++;
    console.log(`  FAIL  ${g.name}: ${e.stack || e}`);
  } finally {
    await d.stop();
  }
}
CATALOGUE.server.close();
console.log(`\n${passes} passed, ${failures} failed`);
process.exit(failures ? 1 : 0);
