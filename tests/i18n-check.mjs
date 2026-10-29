#!/usr/bin/env node
// Checks that every translatable string has an entry in each ui/i18n/<lang>.json and
// that the dictionaries contain no stale keys and keep the {placeholders}.
//
//   node tests/i18n-check.mjs            exit code 1 on problems
//
// Sources of strings: t('…') / tn(n, '…', '…') in ui/*.js, data-i18n / data-i18n-attr in
// ui/index.html, tr!("…") / i18n::tr("…") in src/**/*.rs and the catalogue texts in
// src/builtin.rs (names, tooltips, categories, form labels, option labels).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => fs.readFileSync(path.join(ROOT, p), 'utf8');
const keys = new Map(); // key -> first location
const add = (k, where) => { if (k && !keys.has(k)) keys.set(k, where); };
const unq = (s) => s.slice(1, -1).replace(/\\(['"\\])/g, '$1').replace(/\\n/g, '\n');

// JavaScript: t('…'), t("…"), tn(n, '…', '…'); strings passed through t(LABEL) constants
// (tabs, sorts and texts of the plugin sources, LINUX_NOTE in market.js; key labels in
// keycodes.js) are listed explicitly.
const JS_STR = String.raw`'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"`;
for (const f of fs.readdirSync(path.join(ROOT, 'ui')).filter((f) => f.endsWith('.js'))) {
  const src = read(`ui/${f}`).replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
  for (const m of src.matchAll(new RegExp(String.raw`\bt\(\s*(${JS_STR})`, 'g'))) add(unq(m[1]), `ui/${f}`);
  for (const m of src.matchAll(new RegExp(String.raw`\btn\([^,]+,\s*(${JS_STR}),\s*(${JS_STR})`, 'g'))) {
    add(unq(m[1]), `ui/${f}`);
    add(unq(m[2]), `ui/${f}`);
  }
}
const market = read('ui/market.js');
add(/const LINUX_NOTE = '([^']+)'/.exec(market)?.[1], 'ui/market.js (LINUX_NOTE)');
for (const f of ['market.js', ...fs.readdirSync(path.join(ROOT, 'ui')).filter((f) => /^source-.*\.js$/.test(f))]) {
  const src = read(`ui/${f}`);
  for (const m of src.matchAll(/\blabel: '([^']+)'/g)) add(m[1], `ui/${f} (tab)`);
  for (const m of src.matchAll(/\[\s*'[^']*',\s*'([^']+)'\s*\]/g)) add(m[1], `ui/${f} (sort)`);
  for (const m of src.matchAll(/\b(?:searchPlaceholder|errorText): '([^']+)'/g)) add(m[1], `ui/${f}`);
}
for (const m of read('ui/keycodes.js').matchAll(/\[\s*'[A-Za-z0-9]+',\s*\d+,\s*[^,]+,\s*'([^']+)'\]/g)) {
  if (/[a-z]{2}/.test(m[1])) add(m[1], 'ui/keycodes.js');
}

// HTML
const html = read('ui/index.html');
for (const m of html.matchAll(/<\w+([^>]*)>([^<]*)(?=<)/g)) {
  if (/\sdata-i18n(?![-\w])/.test(m[1])) add(m[2].trim(), 'ui/index.html');
}
for (const m of html.matchAll(/<\w+([^>]*)\sdata-i18n-attr="([^"]+)"([^>]*)>/g)) {
  const attrs = m[1] + m[3];
  for (const a of m[2].split(',')) {
    const v = new RegExp(`${a}="([^"]*)"`).exec(attrs)?.[1];
    if (v) add(v, 'ui/index.html');
  }
}

// Rust
const RS_STR = String.raw`"(?:[^"\\]|\\.)*"`;
for (const f of fs.readdirSync(path.join(ROOT, 'src'), { recursive: true }).filter((f) => f.endsWith('.rs'))) {
  const src = read(`src/${f}`).replace(/#\[cfg\(test\)\][\s\S]*$/, '');
  for (const m of src.matchAll(new RegExp(String.raw`\btr!\(\s*(${RS_STR})`, 'g'))) add(unq(m[1]), `src/${f}`);
  for (const m of src.matchAll(new RegExp(String.raw`i18n::tr\(\s*(${RS_STR})`, 'g'))) add(unq(m[1]), `src/${f}`);
  // not_found("Page") translates the noun at runtime
  for (const m of src.matchAll(new RegExp(String.raw`not_found\(\s*(${RS_STR})`, 'g'))) add(unq(m[1]), `src/${f}`);
}
// Sample profiles: names, descriptions and key titles (translated at runtime).
const templates = read('src/engine/templates.rs');
for (const m of templates.matchAll(/\b(?:name|description): "([^"]+)"/g)) add(m[1], 'src/engine/templates.rs');
for (const m of templates.matchAll(/\bk\("([^"]+)"/g)) add(m[1], 'src/engine/templates.rs (key)');
const builtin = read('src/builtin.rs');
// Formatting-independent: rustfmt may put the def! arguments on separate lines.
for (const m of builtin.matchAll(/def!\(\s*\w+,\s*"[^"]+",\s*"([^"]+)",\s*"([^"]+)"/g)) {
  add(m[1], 'src/builtin.rs (name)');
  add(m[2], 'src/builtin.rs (tooltip)');
}
for (const m of builtin.matchAll(/pub const CAT_\w+: &str = "([^"]+)";/g)) add(m[1], 'src/builtin.rs (category)');
for (const m of builtin.matchAll(/"(?:label|help|placeholder)": "([^"]+)"/g)) add(m[1], 'src/builtin.rs (form)');
for (const m of builtin.matchAll(/json!\([^)]*\),\s*"([^"]+)"\.(?:into|to_string)\(\)/g)) add(m[1], 'src/builtin.rs (option)');
const lists = /MEDIA_ACTIONS: &\[&str\] = &\[([^\]]+)\]/.exec(builtin)?.[1] || '';
for (const m of lists.matchAll(/"([^"]+)"/g)) add(m[1], 'src/builtin.rs (media)');
for (const m of builtin.matchAll(/\(\s*"([^"]+)",\s*-?\d+\s*\)/g)) add(m[1], 'src/builtin.rs (brightness)');
for (const m of read('src/layout.rs').matchAll(/\[("[A-Z][a-z]+",? ?)+\]/g)) {
  for (const n of m[0].matchAll(/"([^"]+)"/g)) add(n[1], 'src/layout.rs (date)');
}

// Strings that are the same in every language need no entry.
const SAME = /^(Num\b.*|Esc|Tab|Enter|Pause|Alt( Gr)?|Super|firefox|https:\/\/)$/;
for (const k of [...keys.keys()]) if (SAME.test(k)) keys.delete(k);

// Compare with the dictionaries
const placeholders = (s) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(',');
let problems = 0;
for (const f of fs.readdirSync(path.join(ROOT, 'ui/i18n')).filter((f) => f.endsWith('.json'))) {
  const dict = JSON.parse(read(`ui/i18n/${f}`));
  const missing = [...keys].filter(([k]) => !(k in dict));
  const stale = Object.keys(dict).filter((k) => !keys.has(k));
  const broken = Object.entries(dict).filter(([k, v]) => keys.has(k) && placeholders(k) !== placeholders(v));
  for (const [k, where] of missing) console.log(`${f}: missing  ${JSON.stringify(k)}  (${where})`);
  for (const k of stale) console.log(`${f}: unused   ${JSON.stringify(k)}`);
  for (const [k, v] of broken) console.log(`${f}: placeholders differ  ${JSON.stringify(k)} → ${JSON.stringify(v)}`);
  problems += missing.length + stale.length + broken.length;
  console.log(`${f}: ${Object.keys(dict).length} entries, ${keys.size} strings in the code`);
}
process.exit(problems ? 1 : 0);
