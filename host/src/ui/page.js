// What every page of the host's windows shares (the panel, setup, the on-screen bar). ui::designed
// puts it into each page at its page marker (ui::PAGE_MARK), after `LANGUAGES`, the host's
// language codes. The language and its texts come from the host (window.I18N, set before any
// page script runs). Pages build their content from text, never from markup: `h` makes elements,
// and strings always become text.
const $ = (id) => document.getElementById(id);
const send = (m) => window.ipc.postMessage(JSON.stringify(m));
const lang = () => (window.I18N && window.I18N.lang) || 'en';
const t = (key, args) => {
  const s = (window.I18N && window.I18N.dict[key]) ?? key;
  return args ? s.replace(/\{(\w+)\}/g, (m, n) => (n in args ? String(args[n]) : m)) : s;
};

// h('p', { class: 'note', id: 'x' }, 'text', child, …): attributes that are false or null are
// left out; children that are strings become text.
const h = (tag, attrs, ...children) => {
  const e = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs || {})) {
    if (value === false || value == null) continue;
    if (name === 'class') e.className = value;
    else e.setAttribute(name, value === true ? '' : String(value));
  }
  e.append(...children.flat().filter((c) => c != null && c !== false));
  return e;
};

// The page's texts in the current language: elements with data-i18n, attributes named in
// data-i18n-attr ("attr=key,attr=key"), and the document's language.
const applyTexts = () => {
  document.documentElement.lang = lang();
  for (const e of document.querySelectorAll('[data-i18n]')) e.textContent = t(e.dataset.i18n);
  for (const e of document.querySelectorAll('[data-i18n-attr]')) {
    for (const pair of e.dataset.i18nAttr.split(',')) { const [attr, key] = pair.split('='); e.setAttribute(attr.trim(), t(key.trim())); }
  }
};

// Follows the host's language: the host calls window.setLang(l) with another language's texts,
// which go in, and then `redraw` runs. In `menu` (a holder element), the language menu (lang.js)
// offers the host's languages, each in its own name; choosing one tells the host.
const followLanguage = (redraw, menu) => {
  const show = menu ? langMenu(menu, LANGUAGES, (l) => send({ cmd: 'lang', lang: l })) : null;
  const apply = () => {
    applyTexts();
    if (show) show(lang(), t('common.language'), Object.fromEntries(LANGUAGES.map((code) => [code, t(`lang.${code}`)])));
  };
  window.setLang = (l) => {
    window.I18N = l;
    apply();
    redraw();
  };
  apply();
};
