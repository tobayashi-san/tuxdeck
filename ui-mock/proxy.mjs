#!/usr/bin/env node
// Serve ui/ from this checkout and proxy everything else (HTTP + WebSocket) to a real daemon.
// Usage: node ui-mock/proxy.mjs [listenPort] [upstream]   (default 47899 → http://127.0.0.1:47830)
import http from 'node:http';
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const UI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', 'ui');
const PORT = Number(process.argv[2] || 47899);
const UP = new URL(process.argv[3] || 'http://127.0.0.1:47830');
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png' };

function serveUi(req, res) {
  let rel = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (rel === '/') rel = '/index.html';
  const file = path.resolve(UI_DIR, '.' + rel);
  if (!file.startsWith(UI_DIR + path.sep) || !fs.existsSync(file)) return false;
  res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
  fs.createReadStream(file).pipe(res);
  return true;
}

// The daemon checks Host and Origin (CSRF protection). The proxy passes on only requests
// from its own page and presents them as same-origin; other web pages are refused here.
const OWN = [`127.0.0.1:${PORT}`, `localhost:${PORT}`];
function own(headers) {
  if (headers.host && !OWN.includes(headers.host)) return false;
  return !headers.origin || OWN.some((h) => headers.origin === `http://${h}`);
}

function rewrite(headers) {
  const out = { ...headers, host: UP.host };
  if (out.origin) out.origin = UP.origin;
  if (out.referer) out.referer = out.referer.replace(/^https?:\/\/[^/]+/, UP.origin);
  return out;
}

const server = http.createServer((req, res) => {
  if (!own(req.headers)) {
    res.writeHead(403).end('forbidden');
    return;
  }
  const p = req.url.split('?')[0];
  if (!/^\/(api|plugin-files|plugins|iconpack-files)\//.test(p) && serveUi(req, res)) return;
  const up = http.request({
    host: UP.hostname, port: UP.port, method: req.method, path: req.url,
    headers: rewrite(req.headers),
  }, (r) => {
    res.writeHead(r.statusCode, r.headers);
    r.pipe(res);
  });
  up.on('error', (e) => {
    if (!res.headersSent) res.writeHead(502, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'upstream: ' + e.message }));
  });
  req.pipe(up);
});

server.on('upgrade', (req, socket, head) => {
  if (!own(req.headers)) {
    socket.end('HTTP/1.1 403 Forbidden\r\n\r\n');
    return;
  }
  const up = net.connect(Number(UP.port), UP.hostname, () => {
    const lines = [`${req.method} ${req.url} HTTP/1.1`];
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const k = req.rawHeaders[i];
      const lk = k.toLowerCase();
      lines.push(`${k}: ${lk === 'host' ? UP.host : lk === 'origin' ? UP.origin : req.rawHeaders[i + 1]}`);
    }
    up.write(lines.join('\r\n') + '\r\n\r\n');
    if (head?.length) up.write(head);
    socket.pipe(up).pipe(socket);
  });
  const end = () => { socket.destroy(); up.destroy(); };
  up.on('error', end);
  socket.on('error', end);
  up.on('close', end);
  socket.on('close', end);
});

server.listen(PORT, '127.0.0.1', () => console.log(`ui proxy on http://127.0.0.1:${PORT} → ${UP.origin}`));
