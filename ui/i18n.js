// UI translations. Source strings are English; `i18n/<lang>.json` maps them to the
// translation (missing entries fall back to English). The daemon uses the same files
// for its messages and tells the UI the effective language in `state.settings.ui_language`.
//
//   t('Delete')                       → "Löschen"
//   t('{n} icons', { n: 3 })          → "3 Icons"
//   <span data-i18n>Settings</span>   translated by translateDom()
//   data-i18n-attr="title,placeholder" translates those attributes

let dict = {};
export let lang = 'en';

/** Load the dictionary for `code` (e.g. "de"); English needs none. */
export async function initI18n(code) {
  lang = code || 'en';
  dict = {};
  document.documentElement.lang = lang.replace('_', '-');
  if (lang === 'en') return;
  try {
    const r = await fetch(`i18n/${encodeURIComponent(lang)}.json`, { cache: 'no-cache' });
    if (r.ok) dict = await r.json();
  } catch {
    /* untranslated: English */
  }
}

/** Translate `s`, replacing `{name}` placeholders from `params`. */
export function t(s, params) {
  let out = dict[s] || s;
  if (params) out = out.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m));
  return out;
}

/** Plural helper: `tn(n, '{n} page', '{n} pages')`. */
export function tn(n, one, many) {
  return t(n === 1 ? one : many, { n });
}

/** Translate static markup (index.html) once after loading the dictionary. */
export function translateDom(root = document) {
  for (const el of root.querySelectorAll('[data-i18n]')) {
    const key = el.dataset.i18nKey || (el.dataset.i18nKey = el.textContent.trim());
    el.textContent = t(key);
  }
  for (const el of root.querySelectorAll('[data-i18n-attr]')) {
    for (const a of el.dataset.i18nAttr.split(',')) {
      const k = `i18nOrig_${a.replace(/-/g, '_')}`;
      if (!(k in el.dataset)) el.dataset[k] = el.getAttribute(a) || '';
      if (el.dataset[k]) el.setAttribute(a, t(el.dataset[k]));
    }
  }
}

/** Locale for dates and numbers. */
export function locale() {
  return lang.replace('_', '-');
}
