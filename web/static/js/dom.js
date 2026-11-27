// Building the page: elements, icons, and the two roots everything is drawn into.

export const $app = document.getElementById('app');
export const $dialogs = document.getElementById('dialog-root');

// An element with attributes (`on…` become listeners) and children (nulls are skipped).
export function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null && v !== false) el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c !== null && c !== undefined && c !== false) el.append(c);
  }
  return el;
}

export const svg = (markup) => {
  const t = document.createElement('template');
  t.innerHTML = markup;
  return t.content.firstChild;
};

export const copyableCode = (text) => h('code', { class: 'copyable-code' }, text);

export const ICONS = {
  // The way back, drawn in the title's own type: sized by the title (style.css .title-back).
  titleBack: '<svg viewBox="0 0 12 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 1.5 2 10l8 8.5"/></svg>',
  more: '<svg width="18" height="18" viewBox="0 0 18 18" fill="currentColor" aria-hidden="true"><circle cx="3.5" cy="9" r="1.4"/><circle cx="9" cy="9" r="1.4"/><circle cx="14.5" cy="9" r="1.4"/></svg>',
  check: '<svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M2.5 7.2l3 3 6-6.4"/></svg>',
  globe: '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><circle cx="8" cy="8" r="6.2"/><path d="M1.8 8h12.4M8 1.8c1.7 1.8 2.6 3.9 2.6 6.2S9.7 12.4 8 14.2C6.3 12.4 5.4 10.3 5.4 8S6.3 3.6 8 1.8z"/></svg>',
  qr: '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><rect x="2" y="2" width="4.5" height="4.5"/><rect x="9.5" y="2" width="4.5" height="4.5"/><rect x="2" y="9.5" width="4.5" height="4.5"/><path d="M9.5 9.5h2v2M14 9.5v1.5M9.5 14h1.5M12.5 12.5H14V14"/></svg>',
  chevron: '<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><path d="M2 3.5 5 6.5 8 3.5"/></svg>',
  phone: '<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><rect x="6" y="2.5" width="8" height="15" rx="1.6"/><path d="M9 15h2"/></svg>',
  laptop: '<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><rect x="4" y="4.5" width="12" height="8" rx="1"/><path d="M2.5 15.5h15"/></svg>',
  person: '<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><circle cx="10" cy="7" r="3.2"/><path d="M3.5 17c.8-3.3 3.3-5 6.5-5s5.7 1.7 6.5 5"/></svg>',
  plus: '<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M10 4v12M4 10h12"/></svg>',
  screen: '<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><rect x="2.5" y="3.5" width="15" height="10" rx="1"/><path d="M10 13.5v3M7 16.5h6"/></svg>',
  // Hide keyboard: a keyboard with an arrow pointing down.
  hideKeyboard: '<svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="3" width="19" height="11.5" rx="2"/><path d="M6 6.8h.01M9.3 6.8h.01M12.6 6.8h.01M15.9 6.8h.01M18 6.8h.01M6 10.6h.01M18 10.6h.01M9 10.6h6"/><path d="M8.5 18l3.5 3 3.5-3"/></svg>',
};

export const spinner = () => h('span', { class: 'spinner', 'aria-hidden': 'true' });

export const loading = (text, cls = '') => h('div', { class: `loading ${cls}`, role: 'status' }, spinner(), text);

// Floating controls over the remote screen keep their events: the screen must not see them.
export function keepEvents(el, kinds) {
  for (const k of kinds) el.addEventListener(k, (e) => e.stopPropagation());
}
