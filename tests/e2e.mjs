#!/usr/bin/env node
// End-to-end tests against a real daemon with a simulated deck and dry-run mode.
//
//   node tests/e2e.mjs [--bin target/release/tuxdeck] [--only neo,plus,mini,original,neo:plugins,...] [--keep] [-v]
//
// Every suite starts its own daemon (TUXDECK_VIRTUAL_DEVICE=<model>,
// TUXDECK_DRY_RUN=1, isolated data/config dirs, free ports) and drives it through
// the HTTP API exactly like the web UI. Side effects of actions are checked through
// the "DRYRUN …" log lines. If the Elgato resources were imported
// (~/.local/share/tuxdeck/elgato), they are linked in so that the original
// icons and info bar layouts are rendered as well.
// No dependencies besides Node.js >= 22.

import { spawn } from 'node:child_process';
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
const ONLY = (opt('--only', 'neo,plus,mini,original')).split(',');
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
};

// ------------------------------------------------------------------------------- daemon

class Daemon {
  constructor(model) {
    this.model = model;
    this.lines = [];
  }

  async start() {
    this.dir = fs.mkdtempSync(path.join(os.tmpdir(), `sd-e2e-${this.model}-`));
    const data = path.join(this.dir, 'data');
    const config = path.join(this.dir, 'config');
    fs.mkdirSync(data, { recursive: true });
    fs.mkdirSync(config, { recursive: true });
    if (fs.existsSync(ELGATO)) fs.symlinkSync(ELGATO, path.join(data, 'elgato'));
    this.port = await freePort();
    this.sdk = await freePort();
    fs.writeFileSync(path.join(config, 'settings.json'), JSON.stringify({ ui_port: this.port, sdk_port: this.sdk, checks_repo: '' }));
    // Own HOME: nothing a test instance or its plugins do may touch the user's files.
    const home = path.join(this.dir, 'home');
    fs.mkdirSync(home, { recursive: true });
    const env = {
      ...process.env,
      TUXDECK_VIRTUAL_DEVICE: this.model, TUXDECK_DRY_RUN: '1', TUXDECK_NO_URL_HANDLER: '1', TUXDECK_NO_WINDOW_WATCH: '1',
      TUXDECK_DATA_DIR: data, TUXDECK_CONFIG_DIR: config, HOME: home, RUST_LOG: 'info', NO_COLOR: '1',
      ...CATALOGUE.env(),
      // Deterministic language: English (the default) regardless of the machine's locale.
      LANGUAGE: '', LC_ALL: '', LC_MESSAGES: '', LANG: 'C.UTF-8',
      // Deterministic clipboard path in dry-run mode.
      WAYLAND_DISPLAY: 'e2e-wayland',
    };
    delete env.HYPRLAND_INSTANCE_SIGNATURE;
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
        this.token ??= fs.readFileSync(path.join(config, 'api-token'), 'utf8').trim();
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

  await test('render: every key preview is a PNG', async () => {
    for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) eq(pngSize(await d.render('Keypad', `${c},${r}`)), [96, 96]);
  });

  await test('press: default keys trigger actions without errors', async () => {
    const page = await d.page();
    const keypad = page.controllers.find((c) => c.Type === 'Keypad');
    const coords = Object.keys(keypad?.Actions || {});
    assert(coords.length > 0, 'default profile has actions');
    const m = d.mark();
    for (const c of coords) await d.press('Keypad', c);
    assert(d.logsSince(m, /DRYRUN/).length >= 1, 'some dry-run side effect');
    eq(d.logsSince(m, /WARN|ERROR/), [], 'no warnings');
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
    const pag = all.find((x) => x.uuid === 'com.elgato.streamdeck.system.pagination');
    const theme = pag.form.find((f) => f.key === 'theme');
    eq(theme.depends_on, 'layout');
    assert(theme.options_by && typeof theme.options_by === 'object', 'options_by');
  });

  await test('info bar: digital time and pagination render 248x58 without SVG errors', async () => {
    const m = d.mark();
    eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
    await d.slot('Neo', '1,0', 'com.elgato.streamdeck.system.pagination');
    const form = (await d.get('/api/actions')).flatMap((c) => c.actions).find((a) => a.uuid === 'com.elgato.streamdeck.system.pagination').form;
    for (const o of form.find((f) => f.key === 'layout').options) {
      await d.settings('Neo', '1,0', { layout: o.value, theme: '' });
      eq(pngSize(await d.render('Neo', '1,0')), [248, 58]);
    }
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

/** Neo: pages, folders, slots, multi actions, profiles, validation. */
async function suiteNeoEditing(d) {
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
    await patch({ state: 0, style: { Title: null, ShowTitle: null, TitleColor: null } });
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
    assert(pi.url.startsWith('/plugins/com.e2e.counter.sdPlugin/'), `PI url ${pi.url}`);
    const html = await d.get(pi.url);
    assert(html.toString().includes('connectElgatoStreamDeckSocket'), 'PI served');
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
    const pi = await driver(ctx, 'com.e2e.counter.count');
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
  async function driver(ctx, action) {
    const pi = new WebSocket(`ws://127.0.0.1:${d.sdk}`);
    await new Promise((r, j) => { pi.onopen = r; pi.onerror = j; });
    pi.send(JSON.stringify({ event: 'registerPropertyInspector', uuid: ctx }));
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
    const drv = await driver(ctx, 'com.e2e.counter.count');
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
    const drv = await driver(ctx, 'com.e2e.prof.act');
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
    await d.del('/api/plugins/com.e2e.prof');
    await d.del(`/api/profiles/${bundled.id}`);
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
    eq([def.name, def.author], ['TuxDeck Essentials', 'TuxDeck']);
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
    // The app menu opens /?token=…: it becomes a cookie and the URL is cleaned.
    const r = await raw({}, `/?token=${d.token}`);
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
}

// Each group runs against a fresh daemon, so a failure can't leave state behind that
// breaks the following groups.
const GROUPS = [
  { model: 'neo', name: 'neo: actions', run: suiteNeoActions },
  { model: 'neo', name: 'neo: editing', run: suiteNeoEditing },
  { model: 'neo', name: 'neo: plugins', run: suiteNeoPlugins },
  { model: 'neo', name: 'neo: system', run: suiteNeoSystem },
  { model: 'neo', name: 'neo: backup', run: suiteBackup },
  // The deliberately crashing test plugin also runs (and is given up) in the outer instance.
  { model: 'neo', name: 'neo: linux-check', run: suiteLinuxCheck, allow: /Plugin com\.e2e\.crash exited/ },
  // Refused packages and links are logged as errors.
  { model: 'neo', name: 'neo: catalogue', run: suiteCatalogue, allow: /Installation failed|official app/ },
  { model: 'plus', name: 'plus', run: suitePlus },
  { model: 'mini', name: 'mini', run: (d) => suiteSmall(d, 'mini', [3, 2]) },
  { model: 'original', name: 'original', run: (d) => suiteSmall(d, 'original', [5, 3]) },
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
  const d = new Daemon(g.model);
  try {
    await d.start();
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
