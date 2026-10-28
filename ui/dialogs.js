// Toasts, modal dialogs and context menu.
import { h } from './store.js';
import { t } from './i18n.js';

/** `action`: optional `{ label, run }` button, e.g. "Undo". */
export function toast(message, level = 'info', ms = 5000, action = null) {
  const box = document.getElementById('toasts');
  const el = h('div.toast', { class: 'toast-' + level, role: 'status' },
    h('span.toast-msg', null, message),
    action ? h('button.btn.small.toast-action', { onclick: () => { el.remove(); action.run(); } }, action.label) : null,
    h('button.toast-close', { title: t('Close'), onclick: () => el.remove() }, '×'));
  box.append(el);
  while (box.children.length > 5) box.firstElementChild.remove();
  setTimeout(() => {
    el.classList.add('toast-out');
    setTimeout(() => el.remove(), 300);
  }, ms);
}

/** Generic modal built on <dialog>. Resolves with the value of the pressed button. */
export function modal(title, body, buttons) {
  return new Promise((resolve) => {
    const returnFocus = document.activeElement;
    const dlg = h('dialog.modal', null,
      h('form', { method: 'dialog' },
        h('h2', null, title),
        body,
        h('div.modal-buttons', null, buttons.map((b) =>
          h('button', { value: b.value, class: b.primary ? 'btn primary' : b.danger ? 'btn danger' : 'btn' }, b.label)))));
    document.body.append(dlg);
    dlg.addEventListener('close', () => {
      const v = dlg.returnValue;
      dlg.remove();
      restoreFocus(returnFocus);
      resolve(v);
    });
    // Enter in a text field would submit with the first button (Cancel) -> use the primary one.
    const primary = buttons.find((b) => b.primary || b.danger);
    dlg.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && e.target.tagName === 'INPUT' && primary) {
        e.preventDefault();
        dlg.close(primary.value);
      }
    });
    dlg.showModal();
    // Destructive dialogs start on Cancel, so Enter doesn't delete by accident.
    const focusable = dlg.querySelector('input, textarea, select') || dlg.querySelector('button.primary') || dlg.querySelector('button');
    focusable?.focus();
    if (focusable?.select) focusable.select();
  });
}

/** Focus `el` again (e.g. the element that opened a dialog), if it is still in the page. */
export function restoreFocus(el) {
  if (el && el !== document.body && el.isConnected && !el.disabled) el.focus({ preventScroll: true });
}

export async function askText(title, label, value = '', okLabel = t('OK')) {
  const input = h('input.text', { type: 'text', value, spellcheck: 'false' });
  const body = h('label.modal-field', null, h('span', null, label), input);
  const r = await modal(title, body, [
    { label: t('Cancel'), value: 'cancel' },
    { label: okLabel, value: 'ok', primary: true },
  ]);
  if (r !== 'ok') return null;
  const text = input.value.trim();
  return text || null;
}

export async function confirmDialog(title, message, okLabel = t('Delete'), danger = true) {
  const r = await modal(title, h('p.modal-text', null, message), [
    { label: t('Cancel'), value: 'cancel' },
    { label: okLabel, value: 'ok', danger, primary: !danger },
  ]);
  return r === 'ok';
}

let openMenu = null;
let menuReturnFocus = null;
export function closeContextMenu() {
  if (!openMenu) return;
  openMenu.remove();
  openMenu = null;
  menuReturnFocus?.focus?.();
  menuReturnFocus = null;
}

/** items: [{label, action, disabled, danger, title} | '-'] */
export function contextMenu(x, y, items) {
  closeContextMenu();
  menuReturnFocus = document.activeElement;
  const menu = h('div.ctxmenu', { role: 'menu' },
    items.map((it) => it === '-' ? h('div.ctx-sep') :
      h('button.ctx-item', {
        role: 'menuitem', disabled: !!it.disabled, class: it.danger ? 'danger' : null, title: it.title || null,
        onclick: () => { closeContextMenu(); it.action(); },
      }, h('span', null, it.label), it.hint ? h('kbd', null, it.hint) : null)));
  document.body.append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, innerWidth - r.width - 4) + 'px';
  menu.style.top = Math.min(y, innerHeight - r.height - 4) + 'px';
  openMenu = menu;
  menu.querySelector('button:not([disabled])')?.focus();
}

addEventListener('mousedown', (e) => { if (openMenu && !openMenu.contains(e.target)) closeContextMenu(); }, true);
// Menu keys are handled here (capture) so they don't reach the page shortcuts.
addEventListener('keydown', (e) => {
  if (!openMenu) return;
  const items = [...openMenu.querySelectorAll('button:not([disabled])')];
  const i = items.indexOf(document.activeElement);
  if (e.key === 'Escape') {
    closeContextMenu();
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const n = items.length;
    if (n) items[(i + (e.key === 'ArrowDown' ? 1 : n - 1) + (i < 0 && e.key === 'ArrowUp' ? 1 : 0)) % n].focus();
  } else if (e.key === 'Home' || e.key === 'End') {
    items[e.key === 'Home' ? 0 : items.length - 1]?.focus();
  } else if (e.key === 'Tab') {
    closeContextMenu();
    return;
  } else {
    return;
  }
  e.preventDefault();
  e.stopPropagation();
}, true);
addEventListener('blur', closeContextMenu);
addEventListener('resize', closeContextMenu);
