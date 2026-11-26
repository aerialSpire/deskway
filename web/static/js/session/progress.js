// The progress card: one small line at the top right of the screen (in the session's stack),
// the progress filling it, saying how a file transfer or a clipboard exchange goes or how it
// ended. Quick ones show nothing: a card appears only after SHOW_AFTER.
//
// Public surface: SHOW_AFTER, percent(done, total), whyText(code), class Progress: el,
// draw(card), setPercent(p), remove().
import { h, keepEvents } from '../dom.js';
import { t } from '../i18n.js';

export const SHOW_AFTER = 500;

export const percent = (done, total) => (total ? Math.min(100, Math.floor((done / total) * 100)) : 0);

// Why a transfer or a clipboard exchange ended, by the code the computer (or this page) gives.
const WHY = {
  locked: 'xfer.why.locked', big: 'xfer.why.big', unsupported: 'xfer.why.unsupported', io: 'xfer.why.io',
  size: 'xfer.why.size', image: 'xfer.why.image', gone: 'xfer.why.gone', net: 'xfer.why.net',
  timeout: 'xfer.why.timeout', denied: 'clip.denied',
};
export const whyText = (code) => t(WHY[code] || 'xfer.failed');

export class Progress {
  // `cls`: the card's own class beside `xfer` (the clipboard's is `clip-note`).
  constructor(stack, cls = '') {
    this.stack = stack;
    this.cls = cls;
    this.el = null;
  }

  // The whole card: { tone ('ok' | 'bad' | null), icon, label, pct (number, or null for none),
  // detail (optional text), more (how many wait), actions: [{ text, run }], close: { label, run } }.
  draw({ tone = null, icon = null, label, detail, pct = null, more = 0, actions = [], close }) {
    if (!this.el) {
      this.el = h('div', { class: `xfer ${this.cls}`.trim(), role: 'status' });
      keepEvents(this.el, ['pointerdown', 'pointermove', 'touchstart', 'contextmenu', 'wheel']);
      this.stack.append(this.el);
    }
    this.el.classList.toggle('bad', tone === 'bad');
    this.el.classList.toggle('ok', tone === 'ok');
    this.el.style.setProperty('--p', `${pct || 0}%`);
    this.el.replaceChildren(...[
      icon ? h('span', { 'aria-hidden': 'true' }, icon) : null,
      h('span', { class: 'x-name' }, label),
      detail ? h('span', { class: 'x-detail' }, detail) : null,
      pct === null ? null : h('span', { class: 'x-pct' }, `${pct}%`),
      more > 0 ? h('span', { class: 'x-more' }, ` · ${t('xfer.more', { n: more })}`) : null,
      ...actions.map((a) => h('button', { class: 'x-btn', onclick: a.run }, a.text)),
      close ? h('button', { class: 'x-btn', 'aria-label': close.label, onclick: close.run }, '✕') : null,
    ].filter(Boolean));
  }

  setDetail(text) {
    const detail = this.el?.querySelector('.x-detail');
    if (detail) detail.textContent = text;
  }

  // Only the number, while the rest stays as drawn.
  setPercent(p) {
    if (!this.el) return;
    this.el.style.setProperty('--p', `${p}%`);
    const pct = this.el.querySelector('.x-pct');
    if (pct) pct.textContent = `${p}%`;
  }

  remove() {
    if (this.el) { this.el.remove(); this.el = null; }
  }
}
