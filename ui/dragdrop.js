// Drag & drop of actions and slots, file drops (images, imports).

import { S, instanceAccepts, instanceAt, slotAccepts, slotAppearance } from './store.js';
import { dropImage } from './editor.js';
import { t } from './i18n.js';
import { toast } from './dialogs.js';
import { deleteSlot, moveSlot, placeAction, select, slotFromEvent } from './canvas.js';
import { $, el } from './dom.js';
import { dragEnded } from './data.js';
import { IMPORT_RE, importFile } from './topbar.js';

function canDrop(controller, coord, e) {
  const d = S.drag;
  if (!d) return false;
  if (d.kind === 'action') return slotAccepts(controller, coord, d.action);
  if (d.kind === 'slot') {
    if (d.controller === controller && d.coord === coord) return false;
    if (!instanceAccepts(controller, coord, d.inst, d.controller)) return false;
    const copy = e && (e.ctrlKey || e.altKey);
    const target = instanceAt(controller, coord);
    if (!copy && target && !instanceAccepts(d.controller, d.coord, target, controller)) return false;
    return true;
  }
  return false;
}

export function markCandidates() {
  for (const s of el.device.querySelectorAll('.slot')) {
    const ok = canDrop(s.dataset.controller, s.dataset.coord, null);
    s.classList.toggle('drop-candidate', ok);
    s.classList.toggle('drop-denied', !ok && !(S.drag?.kind === 'slot' && S.drag.controller === s.dataset.controller && S.drag.coord === s.dataset.coord));
  }
}

function endDrag() {
  if (!S.drag) return;
  S.drag = null;
  document.body.classList.remove('dragging', 'dragging-slot');
  for (const s of el.device.querySelectorAll('.slot')) s.classList.remove('drop-ok', 'drop-candidate', 'drop-denied', 'drag-source');
  el.trash.classList.remove('over');
  el.sidebar.classList.remove('drop-delete');
  dragEnded();
}

el.device.addEventListener('dragstart', (e) => {
  const s = slotFromEvent(e);
  const inst = s && instanceAt(s.controller, s.coord);
  if (!inst) {
    e.preventDefault();
    return;
  }
  S.drag = { kind: 'slot', controller: s.controller, coord: s.coord, inst };
  e.dataTransfer.effectAllowed = 'copyMove';
  e.dataTransfer.setData('text/plain', `slot:${s.controller}/${s.coord}`);
  s.el.classList.add('drag-source');
  // Also on the key itself: dragend reaches it even if it was removed from the page.
  s.el.addEventListener('dragend', endDrag, { once: true });
  document.body.classList.add('dragging', 'dragging-slot');
  select(s);
  markCandidates();
});

el.device.addEventListener('dragover', (e) => {
  const s = slotFromEvent(e);
  if (!s || !S.drag) return;
  if (!canDrop(s.controller, s.coord, e)) {
    e.dataTransfer.dropEffect = 'none';
    return;
  }
  e.preventDefault();
  const copy = S.drag.kind === 'action' || e.ctrlKey || e.altKey;
  e.dataTransfer.dropEffect = copy ? 'copy' : 'move';
  for (const x of el.device.querySelectorAll('.slot.drop-ok')) if (x !== s.el) x.classList.remove('drop-ok');
  s.el.classList.add('drop-ok');
});

el.device.addEventListener('dragleave', (e) => {
  const s = slotFromEvent(e);
  if (s && !s.el.contains(e.relatedTarget)) s.el.classList.remove('drop-ok');
});

el.device.addEventListener('drop', (e) => {
  const s = slotFromEvent(e);
  const d = S.drag;
  if (!s || !d) return;
  e.preventDefault();
  const allowed = canDrop(s.controller, s.coord, e);
  const copy = e.ctrlKey || e.altKey || e.dataTransfer.dropEffect === 'copy';
  endDrag();
  if (!allowed) return;
  if (d.kind === 'action') placeAction(d.action, s);
  else moveSlot(d, s, copy);
});

document.addEventListener('dragend', endDrag);
// Prevent the browser from navigating to dropped files/links anywhere else.
document.addEventListener('dragover', (e) => {
  if (isFileDrag(e)) fileDragOver(e);
  else if (!S.drag) e.preventDefault();
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  if (isFileDrag(e)) dropFiles(e);
});

// ---- files from the file manager ------------------------------------------------------------
// Images go onto a key (or the editor preview); profiles, plugins and icon packs are imported
// wherever they are dropped.

const IMAGE_RE = /\.(png|jpe?g|gif|svg|webp)$/i;
const isFileDrag = (e) => !S.drag && [...(e.dataTransfer?.types || [])].includes('Files');
// While dragging only the MIME types are known (the names only on drop).
const draggingImages = (e) => [...e.dataTransfer.items].some((i) => i.kind === 'file' && i.type.startsWith('image/'));

function imageDropTarget(e) {
  const s = slotFromEvent(e);
  if (s) {
    const inst = instanceAt(s.controller, s.coord);
    return inst && slotAppearance(s.controller, s.coord, inst).image ? s : null;
  }
  const p = e.target.closest?.('.ed-preview');
  return p && !p.classList.contains('static') && el.editor.contains(p) ? { el: p, preview: true } : null;
}

let fileDragTimer = 0;
function fileDragOver(e) {
  e.preventDefault();
  const images = draggingImages(e);
  const target = images ? imageDropTarget(e) : null;
  e.dataTransfer.dropEffect = images && !target ? 'none' : 'copy';
  if (!document.body.classList.contains('file-dragging')) {
    document.body.classList.add('file-dragging');
    for (const n of el.device.querySelectorAll('.slot')) {
      const inst = instanceAt(n.dataset.controller, n.dataset.coord);
      n.classList.toggle('image-candidate', !!inst && slotAppearance(n.dataset.controller, n.dataset.coord, inst).image);
    }
  }
  document.body.classList.toggle('file-import', !images);
  for (const x of document.querySelectorAll('.file-drop-ok')) if (x !== target?.el) x.classList.remove('file-drop-ok');
  target?.el.classList.add('file-drop-ok');
  // dragover repeats while the pointer is over the page; when it stops, the drag left the window.
  clearTimeout(fileDragTimer);
  fileDragTimer = setTimeout(endFileDrag, 250);
}

function endFileDrag() {
  clearTimeout(fileDragTimer);
  document.body.classList.remove('file-dragging', 'file-import');
  for (const x of document.querySelectorAll('.file-drop-ok, .image-candidate')) x.classList.remove('file-drop-ok', 'image-candidate');
}

async function dropFiles(e) {
  const target = imageDropTarget(e);
  endFileDrag();
  const files = [...e.dataTransfer.files];
  const imports = files.filter((f) => IMPORT_RE.test(f.name));
  const image = files.find((f) => !IMPORT_RE.test(f.name) && (f.type.startsWith('image/') || IMAGE_RE.test(f.name)));
  for (const f of imports) await importFile(f);
  if (image && target) {
    if (!target.preview) select(target);
    await dropImage(image);
  } else if (image) {
    toast(t('Drop images onto a key with an action or onto the image in the editor.'), 'info', 4000);
  } else if (!imports.length && files.length) {
    toast(t('Only .streamDeckProfile, .streamDeckPlugin and .streamDeckIconPack files can be imported.'), 'error');
  }
}

for (const zone of [el.trash, el.sidebar]) {
  zone.addEventListener('dragover', (e) => {
    if (S.drag?.kind !== 'slot') return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    (zone === el.trash ? el.trash : el.sidebar).classList.add(zone === el.trash ? 'over' : 'drop-delete');
  });
  zone.addEventListener('dragleave', (e) => {
    if (!zone.contains(e.relatedTarget)) zone.classList.remove('over', 'drop-delete');
  });
  zone.addEventListener('drop', (e) => {
    if (S.drag?.kind !== 'slot') return;
    e.preventDefault();
    e.stopPropagation();
    const d = S.drag;
    endDrag();
    deleteSlot(d);
  });
}
