// Language and texts (docs/i18n.md). The texts live in i18n/<lang>.json. Only the chosen
// language is fetched, plus English, which fills in any key a catalog lacks. The choice is kept
// in this browser only.
import { store } from './env.js';

export const LANGS = ['en', 'zh-Hans', 'zh-Hant'];
// For dates and times; Traditional Chinese follows Taiwan.
const LOCALES = { en: 'en', 'zh-Hans': 'zh-CN', 'zh-Hant': 'zh-TW' };
let lang = 'en';
let catalog = {};
let fallback = {};
const catalogs = {};
let plurals = null;

export const currentLang = () => lang;

// A saved choice, else the browser's languages in order: zh-TW/HK/MO and zh-Hant are
// Traditional, any other Chinese is Simplified, and the first other language means English.
export function pickLanguage() {
  const saved = store.get('deskway.lang');
  if (LANGS.includes(saved)) return saved;
  const wanted = navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language || ''];
  for (const l of wanted) {
    const x = String(l || '').toLowerCase().replace(/_/g, '-');
    if (!x) continue;
    if (/^zh-(tw|hk|mo)(-|$)|^zh-hant(-|$)/.test(x)) return 'zh-Hant';
    if (x === 'zh' || x.startsWith('zh-')) return 'zh-Hans';
    return 'en';
  }
  return 'en';
}

async function loadCatalog(l) {
  if (!catalogs[l]) {
    const res = await fetch(`i18n/${l}.json`);
    if (!res.ok) throw new Error(String(res.status));
    catalogs[l] = await res.json();
  }
  return catalogs[l];
}

// Loads `l` (and English) and makes it the page's language; `save` remembers it here.
export async function setLanguage(l, { save = false } = {}) {
  if (!LANGS.includes(l)) l = 'en';
  const [en, cur] = await Promise.all([loadCatalog('en').catch(() => null), loadCatalog(l).catch(() => null)]);
  lang = cur ? l : 'en';
  fallback = en || {};
  catalog = cur || fallback;
  plurals = null;
  document.documentElement.lang = lang;
  if (save) store.set('deskway.lang', lang);
}

// The text for `key`, with {name} placeholders filled from `params`. A numeric `n` picks the
// plural form (`key.one`, `key.other`) when the catalog has them. Missing: English, then the key.
export function t(key, params) {
  let s;
  if (params && typeof params.n === 'number') {
    plurals ||= new Intl.PluralRules(LOCALES[lang]);
    const form = plurals.select(params.n);
    s = catalog[`${key}.${form}`] ?? catalog[`${key}.other`] ?? fallback[`${key}.${form}`] ?? fallback[`${key}.other`];
  }
  s ??= catalog[key] ?? fallback[key] ?? key;
  return params ? s.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m)) : s;
}

export const timeText = (date) => date.toLocaleTimeString(LOCALES[lang], { hour: '2-digit', minute: '2-digit' });

// A day, from unix seconds.
export const dateText = (unix) => new Date(unix * 1000).toLocaleDateString(LOCALES[lang], { year: 'numeric', month: 'short', day: 'numeric' });

export function agoText(unix, { seen = true } = {}) {
  if (!unix) return '';
  const mins = Math.max(0, Math.floor((Date.now() / 1000 - unix) / 60));
  if (mins < 1) return t(seen ? 'time.justNow' : 'time.now');
  const [n, unit] = mins < 60 ? [mins, 'minute']
    : mins < 1440 ? [Math.floor(mins / 60), 'hour']
      : [Math.floor(mins / 1440), 'day'];
  if (seen) return t(`time.${unit}s`, { n });
  return new Intl.RelativeTimeFormat(LOCALES[lang]).format(-n, unit);
}

// A size in KB or MB, never 0 KB.
export const fmtSize = (n) => (n >= 1 << 20 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);

// One file by its name; several by the first name and how many.
export const filesText = (names) => (names.length > 1 ? t('xfer.files', { n: names.length, name: names[0] }) : names[0]);
