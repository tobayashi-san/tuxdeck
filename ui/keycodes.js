// KeyboardEvent.code -> Linux evdev code / Qt::Key value / English display label (translated with t()).
// Standard US (ANSI/ISO) keyboard incl. F1-F24, navigation, numpad, punctuation, media keys.
import { t } from './i18n.js';

const K = 0x01000000; // Qt special key base
export const QT_UNUSED = 33554431; // 0x01FFFFFF = Qt::Key_unknown

/** @type {Array<[string, number, number, string]>} code, evdev, qt, label */
const TABLE = [];

// Letters
const LETTER_EV = {
  Q: 16, W: 17, E: 18, R: 19, T: 20, Y: 21, U: 22, I: 23, O: 24, P: 25,
  A: 30, S: 31, D: 32, F: 33, G: 34, H: 35, J: 36, K: 37, L: 38,
  Z: 44, X: 45, C: 46, V: 47, B: 48, N: 49, M: 50,
};
for (const ch of 'ABCDEFGHIJKLMNOPQRSTUVWXYZ') TABLE.push(['Key' + ch, LETTER_EV[ch], ch.charCodeAt(0), ch]);

// Digits (top row)
for (let d = 0; d <= 9; d++) TABLE.push(['Digit' + d, d === 0 ? 11 : d + 1, 0x30 + d, String(d)]);

// Punctuation (US layout, unshifted ASCII)
TABLE.push(
  ['Minus', 12, 0x2d, '-'],
  ['Equal', 13, 0x3d, '='],
  ['BracketLeft', 26, 0x5b, '['],
  ['BracketRight', 27, 0x5d, ']'],
  ['Backslash', 43, 0x5c, '\\'],
  ['Semicolon', 39, 0x3b, ';'],
  ['Quote', 40, 0x27, "'"],
  ['Backquote', 41, 0x60, '`'],
  ['Comma', 51, 0x2c, ','],
  ['Period', 52, 0x2e, '.'],
  ['Slash', 53, 0x2f, '/'],
  ['IntlBackslash', 86, 0x3c, '<'],
  ['IntlRo', 89, 0x5f, 'Ro'],
  ['IntlYen', 124, 0xa5, '¥'],
  ['Space', 57, 0x20, 'Space'],
);

// Editing / navigation
TABLE.push(
  ['Escape', 1, K + 0x00, 'Esc'],
  ['Tab', 15, K + 0x01, 'Tab'],
  ['Backspace', 14, K + 0x03, 'Backspace'],
  ['Enter', 28, K + 0x04, 'Enter'],
  ['Insert', 110, K + 0x06, 'Insert'],
  ['Delete', 111, K + 0x07, 'Del'],
  ['Pause', 119, K + 0x08, 'Pause'],
  ['PrintScreen', 99, K + 0x09, 'Print'],
  ['Home', 102, K + 0x10, 'Home'],
  ['End', 107, K + 0x11, 'End'],
  ['ArrowLeft', 105, K + 0x12, '←'],
  ['ArrowUp', 103, K + 0x13, '↑'],
  ['ArrowRight', 106, K + 0x14, '→'],
  ['ArrowDown', 108, K + 0x15, '↓'],
  ['PageUp', 104, K + 0x16, 'Page Up'],
  ['PageDown', 109, K + 0x17, 'Page Down'],
  ['ContextMenu', 127, K + 0x55, 'Menu'],
  ['Help', 138, K + 0x58, 'Help'],
);

// Modifiers and locks
TABLE.push(
  ['ShiftLeft', 42, K + 0x20, 'Shift'],
  ['ShiftRight', 54, K + 0x20, 'Right Shift'],
  ['ControlLeft', 29, K + 0x21, 'Ctrl'],
  ['ControlRight', 97, K + 0x21, 'Right Ctrl'],
  ['AltLeft', 56, K + 0x23, 'Alt'],
  ['AltRight', 100, 0x01001103, 'Alt Gr'],
  ['MetaLeft', 125, K + 0x53, 'Super'],
  ['MetaRight', 126, K + 0x54, 'Right Super'],
  ['OSLeft', 125, K + 0x53, 'Super'],
  ['OSRight', 126, K + 0x54, 'Right Super'],
  ['CapsLock', 58, K + 0x24, 'Caps Lock'],
  ['NumLock', 69, K + 0x25, 'Num'],
  ['ScrollLock', 70, K + 0x26, 'Scroll Lock'],
);

// Function keys F1-F24
const F_EV = [59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88];
for (let n = 1; n <= 24; n++) TABLE.push(['F' + n, n <= 12 ? F_EV[n - 1] : 183 + (n - 13), K + 0x30 + n - 1, 'F' + n]);

// Numpad (Qt uses the same key values as the main keys, plus KeypadModifier)
const NUM_EV = [82, 79, 80, 81, 75, 76, 77, 71, 72, 73];
for (let d = 0; d <= 9; d++) TABLE.push(['Numpad' + d, NUM_EV[d], 0x30 + d, 'Num ' + d]);
TABLE.push(
  ['NumpadDecimal', 83, 0x2e, 'Num ,'],
  ['NumpadAdd', 78, 0x2b, 'Num +'],
  ['NumpadSubtract', 74, 0x2d, 'Num -'],
  ['NumpadMultiply', 55, 0x2a, 'Num *'],
  ['NumpadDivide', 98, 0x2f, 'Num /'],
  ['NumpadEqual', 117, 0x3d, 'Num ='],
  ['NumpadComma', 121, 0x2c, 'Num ,'],
  ['NumpadEnter', 96, K + 0x05, 'Num Enter'],
);

// Media / browser / system keys (incl. legacy Firefox aliases)
TABLE.push(
  ['AudioVolumeMute', 113, K + 0x71, 'Mute'],
  ['AudioVolumeDown', 114, K + 0x70, 'Volume down'],
  ['AudioVolumeUp', 115, K + 0x72, 'Volume up'],
  ['VolumeMute', 113, K + 0x71, 'Mute'],
  ['VolumeDown', 114, K + 0x70, 'Volume down'],
  ['VolumeUp', 115, K + 0x72, 'Volume up'],
  ['MediaPlayPause', 164, K + 0x86, 'Play/Pause'],
  ['MediaStop', 166, K + 0x81, 'Stop'],
  ['MediaTrackNext', 163, K + 0x83, 'Next track'],
  ['MediaTrackPrevious', 165, K + 0x82, 'Previous track'],
  ['MediaSelect', 226, K + 0xa1, 'Media'],
  ['LaunchMediaPlayer', 226, K + 0xa1, 'Media'],
  ['BrowserBack', 158, K + 0x61, 'Browser back'],
  ['BrowserForward', 159, K + 0x62, 'Browser forward'],
  ['BrowserRefresh', 173, K + 0x64, 'Reload'],
  ['BrowserStop', 128, K + 0x63, 'Browser stop'],
  ['BrowserSearch', 217, K + 0x92, 'Search'],
  ['BrowserHome', 172, K + 0x90, 'Home page'],
  ['BrowserFavorites', 156, K + 0x91, 'Favorites'],
  ['LaunchMail', 155, K + 0xa0, 'Email'],
  ['LaunchApp1', 157, K + 0xa2, 'My Computer'],
  ['LaunchApp2', 140, K + 0xcb, 'Calculator'],
  ['Eject', 161, K + 0xb9, 'Eject'],
  ['Power', 116, K + 0xb7, 'Power'],
  ['Sleep', 142, 0x01020004, 'Sleep'],
  ['WakeUp', 143, K + 0xb8, 'Wake up'],
  ['Copy', 133, K + 0xcf, 'Copy'],
  ['Cut', 137, K + 0xd0, 'Cut'],
  ['Paste', 135, K + 0xe2, 'Paste'],
);

/** code -> {ev, qt, label} */
export const KEYMAP = new Map(TABLE.map(([code, ev, qt, label]) => [code, { ev, qt, label }]));
const BY_EV = new Map();
const BY_QT = new Map();
for (const [, ev, qt, label] of TABLE) {
  if (!BY_EV.has(ev)) BY_EV.set(ev, label);
  if (!BY_QT.has(qt)) BY_QT.set(qt, label);
}

export const MODIFIER_CODES = new Set([
  'ShiftLeft', 'ShiftRight', 'ControlLeft', 'ControlRight', 'AltLeft', 'AltRight',
  'MetaLeft', 'MetaRight', 'OSLeft', 'OSRight',
]);

export function unusedHotkey() {
  return {
    KeyCmd: false, KeyCtrl: false, KeyOption: false, KeyShift: false, KeyModifiers: 0,
    QTKeyCode: QT_UNUSED, NativeCode: -1, VKeyCode: -1, LinuxCode: -1,
  };
}

export function isUnusedHotkey(hk) {
  return !hk || ((hk.QTKeyCode == null || hk.QTKeyCode === QT_UNUSED) && !(hk.LinuxCode >= 0));
}

/** The keyboard layout's characters by KeyboardEvent.code (Chromium), for labels and the
 * key code of the official format; null where the browser can't tell. */
let layoutMap = null;
let layoutLoading = Promise.resolve();
export function loadLayoutMap() {
  layoutLoading = (async () => {
    try {
      layoutMap = await navigator.keyboard?.getLayoutMap?.() ?? null;
    } catch {
      layoutMap = null;
    }
  })();
  return layoutLoading;
}
/** Resolves once the layout started loading last is known. */
export const layoutLoaded = () => layoutLoading;

/** The upper-case letter, or the character itself where that is not one character (ß). */
const upperChar = (ch) => ([...ch.toUpperCase()].length === 1 ? ch.toUpperCase() : ch);

/** Build an Elgato hotkey object from a KeyboardEvent.code and modifier flags. The
 * physical key goes to LinuxCode; the character the layout puts there (QWERTZ: Z on the
 * US Y key) to QTKeyCode, which tools that send characters use. */
export function makeHotkey(code, mods) {
  const k = KEYMAP.get(code);
  const shift = !!mods.shift, ctrl = !!mods.ctrl, alt = !!mods.alt, meta = !!mods.meta;
  const ch = layoutMap?.get(code);
  // Only for keys that type a character (letters, digits, punctuation).
  const layoutQt = k && k.qt > 0x20 && k.qt < 0x1000000 && typeof ch === 'string' && [...ch].length === 1
    ? upperChar(ch).codePointAt(0) : null;
  return {
    KeyCmd: meta, KeyCtrl: ctrl, KeyOption: alt, KeyShift: shift,
    KeyModifiers: (shift ? 1 : 0) | (ctrl ? 2 : 0) | (alt ? 4 : 0) | (meta ? 8 : 0),
    QTKeyCode: layoutQt ?? (k ? k.qt : QT_UNUSED), NativeCode: -1, VKeyCode: -1, LinuxCode: k ? k.ev : -1,
  };
}

/** Human readable label, e.g. "Ctrl + Shift + Z" (translated). Returns "" for unused. */
export function hotkeyLabel(hk, macStyle = false) {
  if (isUnusedHotkey(hk)) return '';
  const m = hk.KeyModifiers ?? 0;
  const parts = [];
  if (hk.KeyCtrl || m & 2) parts.push(t('Ctrl'));
  if (hk.KeyOption || m & 4) parts.push(macStyle ? '⌥' : 'Alt');
  if (hk.KeyShift || m & 1) parts.push(t('Shift'));
  if (hk.KeyCmd || m & 8) parts.push(macStyle ? '⌘' : 'Super');
  let key = null;
  // A character key is named by its character (recorded from the layout); other keys by
  // their position.
  const qt = hk.QTKeyCode;
  if (qt != null && qt > 0x20 && qt < 0x1000000 && !(qt >= 0x7f && qt < 0xa0)) key = String.fromCodePoint(qt);
  if (!key && hk.LinuxCode >= 0) key = BY_EV.get(hk.LinuxCode);
  if (key && key.length > 1) key = t(key);
  if (!key && hk.QTKeyCode != null && hk.QTKeyCode !== QT_UNUSED) {
    key = BY_QT.get(hk.QTKeyCode);
    if (!key && hk.QTKeyCode >= 0x20 && hk.QTKeyCode < 0x7f) key = String.fromCharCode(hk.QTKeyCode);
    if (key) key = t(key);
    if (!key) key = t('Key') + ' 0x' + hk.QTKeyCode.toString(16);
  }
  // Modifier-only hotkey: avoid "Ctrl + Ctrl"
  if (key && !parts.includes(key)) parts.push(key);
  return parts.join(' + ');
}
