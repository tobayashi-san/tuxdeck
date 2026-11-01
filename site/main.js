// The deck in the hero: its keys take you around the page (also with 1–8), the touch
// points page through the info bar, Party drops keys, Sound makes the keys click.
const ICONS = 'icons.svg#';
const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;

const KEYS = [
  { icon: 'download', label: 'Install', c: '#60a5fa', go: '#install' },
  { icon: 'grid', label: 'Features', c: '#60a5fa', go: '#features' },
  { icon: 'stream-deck-mk2', label: 'Devices', c: '#2dd4bf', go: '#devices' },
  { icon: 'code', label: 'GitHub', c: '#94a3b8', href: 'https://github.com/tobayashi-san/tuxdeck' },
  { icon: 'question-mark', label: 'FAQ', c: '#a78bfa', go: '#faq' },
  { icon: 'heart', label: 'Star it', c: '#fb7185', href: 'https://github.com/tobayashi-san/tuxdeck/stargazers' },
  { icon: 'volume-muted', label: 'Sound', c: '#fbbf24', toggle: 'sound' },
  { icon: 'starbursts', label: 'Party', c: '#f87171', party: true },
];

const MESSAGES = [
  'hello, penguin 👋',
  () => new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) + ' · time to press keys',
  'no VM was harmed',
  'Wayland ✓  X11 ✓',
  'KDE · GNOME · Hyprland',
  'your plugins came too',
  'sudo make me a deck',
  'MIT licensed · free',
];

let sound = false;
let audio = null;
let msg = 0;
const keysEl = document.getElementById('keys');
let barEl = document.getElementById('bar-text');

function click() {
  if (!sound) return;
  audio ||= new AudioContext();
  const o = audio.createOscillator();
  const g = audio.createGain();
  o.type = 'square';
  o.frequency.value = 900 + Math.random() * 300;
  g.gain.setValueAtTime(0.05, audio.currentTime);
  g.gain.exponentialRampToValueAtTime(0.0001, audio.currentTime + 0.06);
  o.connect(g).connect(audio.destination);
  o.start();
  o.stop(audio.currentTime + 0.07);
}

/** Show message `i` (or `text`) in the info bar, sliding in. */
function showMessage(i, text) {
  msg = (i + MESSAGES.length) % MESSAGES.length;
  const m = text ?? MESSAGES[msg];
  const span = document.createElement('span');
  span.textContent = typeof m === 'function' ? m() : m;
  barEl.replaceWith(span);
  barEl = span;
}

function party() {
  if (reduced) return;
  const box = document.getElementById('confetti');
  const colours = KEYS.map((k) => k.c);
  for (let i = 0; i < 46; i++) {
    const k = document.createElement('i');
    k.style.left = `${Math.random() * 100}vw`;
    k.style.setProperty('--c', colours[i % colours.length]);
    k.style.setProperty('--r', `${(Math.random() * 720 - 360) | 0}deg`);
    k.style.animationDuration = `${1.6 + Math.random() * 1.8}s`;
    k.style.animationDelay = `${Math.random() * 0.4}s`;
    k.style.transform = `rotate(${(Math.random() * 90) | 0}deg)`;
    box.append(k);
    k.addEventListener('animationend', () => k.remove());
  }
}

function press(i) {
  const k = KEYS[i];
  const el = keysEl.children[i];
  click();
  el.classList.add('down', 'flash');
  setTimeout(() => el.classList.remove('down'), 120);
  setTimeout(() => el.classList.remove('flash'), 420);
  if (k.go) {
    document.querySelector(k.go)?.scrollIntoView({ behavior: reduced ? 'auto' : 'smooth' });
    showMessage(msg, `→ ${k.label}`);
  } else if (k.href) {
    window.open(k.href, '_blank', 'noopener');
  } else if (k.toggle === 'sound') {
    sound = !sound;
    el.classList.toggle('on', sound);
    el.querySelector('use').setAttribute('href', ICONS + (sound ? 'volume' : 'volume-muted'));
    el.querySelector('.lbl').textContent = sound ? 'Sound on' : 'Sound';
    el.setAttribute('aria-pressed', String(sound));
    if (sound) click();
    showMessage(msg, sound ? 'click click click' : 'shhh');
  } else if (k.party) {
    el.classList.add('on');
    setTimeout(() => el.classList.remove('on'), 900);
    showMessage(msg, '🎉 keys everywhere');
    party();
  }
}

KEYS.forEach((k, i) => {
  const b = document.createElement('button');
  b.className = 'key';
  b.type = 'button';
  b.style.setProperty('--c', k.c);
  b.title = `${k.label} (${i + 1})`;
  if (k.toggle) b.setAttribute('aria-pressed', 'false');
  b.innerHTML = `<span class="led"></span><svg aria-hidden="true"><use href="${ICONS}${k.icon}"/></svg><span class="lbl"></span>`;
  b.querySelector('.lbl').textContent = k.label;
  b.addEventListener('click', () => press(i));
  keysEl.append(b);
});

document.addEventListener('keydown', (e) => {
  if (e.ctrlKey || e.metaKey || e.altKey || /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
  const n = Number(e.key);
  if (n >= 1 && n <= 8) press(n - 1);
});

document.getElementById('touch-l').addEventListener('click', () => { click(); showMessage(msg - 1); });
document.getElementById('touch-r').addEventListener('click', () => { click(); showMessage(msg + 1); });
if (!reduced) setInterval(() => showMessage(msg + 1), 4500);

// Install tabs (arrow keys move between them).
const tabs = [...document.querySelectorAll('.tabs [role=tab]')];
function select(tab) {
  for (const t of tabs) {
    const on = t === tab;
    t.setAttribute('aria-selected', String(on));
    t.tabIndex = on ? 0 : -1;
    document.getElementById(t.getAttribute('aria-controls')).hidden = !on;
  }
}
tabs.forEach((t, i) => {
  t.addEventListener('click', () => select(t));
  t.addEventListener('keydown', (e) => {
    const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (!d) return;
    const next = tabs[(i + d + tabs.length) % tabs.length];
    select(next);
    next.focus();
  });
});
// Pick the tab that fits the visitor's system when the browser says.
const ua = navigator.userAgent.toLowerCase();
if (/fedora|suse|red hat/.test(ua)) select(tabs[1]);

// Copy buttons: the commands without prompts and comments.
for (const b of document.querySelectorAll('.copy')) {
  b.addEventListener('click', async () => {
    const lines = b.parentElement.querySelector('code').innerText.split('\n').filter((l) => l.startsWith('$')).map((l) => l.replace(/^\$\s*/, ''));
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      b.classList.add('done');
      b.querySelector('span').textContent = 'Copied';
      b.querySelector('use').setAttribute('href', ICONS + 'checkmark');
      setTimeout(() => {
        b.classList.remove('done');
        b.querySelector('span').textContent = 'Copy';
        b.querySelector('use').setAttribute('href', ICONS + 'copy');
      }, 1600);
    } catch {
      b.querySelector('span').textContent = 'Select & copy';
    }
  });
}
