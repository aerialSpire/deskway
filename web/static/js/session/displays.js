// The computer's displays: the “Screen” button (only with several) and its panel to switch.
// The choice is remembered per computer, and taken again at the next session. This module also
// owns the displays' order and numbering and the remembered choice for the computer's page
// (views/detail.js), so both say the same.
//
// Public surface: chosenDisplay(computerId), rememberDisplay(computerId, displayId),
// orderedDisplays(list); class Displays: list, current, button, onDisplays(m), choose(id),
// syncButton(), restore(), buildPanel().
import { h } from '../dom.js';
import { store } from '../env.js';
import { t } from '../i18n.js';
import { CAPS, Control } from '../proto.js';

const chosenKey = (computerId) => `deskway.display.${computerId}`;
// The display this browser used last on the computer, or null.
export const chosenDisplay = (computerId) => store.get(chosenKey(computerId));
export const rememberDisplay = (computerId, displayId) => store.set(chosenKey(computerId), displayId);

// The displays with an area, left to right (then top to bottom): display n is the n-th here.
export const orderedDisplays = (list) => (Array.isArray(list) ? list.filter((x) => x && x.w > 0 && x.h > 0) : [])
  .sort((a, b) => a.x - b.x || a.y - b.y);

export class Displays {
  constructor(s) {
    this.s = s;
    this.list = [];
    this.current = null;
    this.restored = false;
    this.button = h('button', { class: 'tool', hidden: true, 'aria-pressed': 'false', onclick: () => s.toolbar.togglePanel('displays') }, t('tb.screen'));
  }

  onDisplays(m) {
    this.list = m.list || [];
    this.current = m.cur;
    const shown = orderedDisplays(this.list);
    const idx = shown.findIndex((d) => d.id === m.cur);
    this.syncButton();
    this.button.replaceChildren(t('tb.screen'), idx >= 0 ? h('span', { class: 'tool-count' }, `${idx + 1}/${shown.length}`) : '');
    this.button.setAttribute('aria-label', t('display.switchLabel', { i: idx + 1, total: shown.length }));
    this.restore();
    this.s.toolbar.refreshPanel('displays');
  }

  syncButton() {
    this.button.hidden = !this.s.hasCap(CAPS.DISPLAYS) || this.list.length < 2;
  }

  // Return to the last display once the list is known and the session can send the choice.
  restore() {
    if (this.restored || !this.s.connected || !this.s.hasCap(CAPS.DISPLAYS) || !this.list.length) return;
    this.restored = true;
    const want = chosenDisplay(this.s.device.id);
    if (want && want !== this.current && this.list.some((d) => d.id === want)) this.choose(want);
  }

  choose(id) {
    if (!this.s.connected || !this.s.hasCap(CAPS.DISPLAYS)) return;
    rememberDisplay(this.s.device.id, id);
    if (id === this.current) return;
    this.s.send({ t: Control.DISPLAY, v: id });
    this.s.pointer.cursor = { x: 0.5, y: 0.5 };
    this.s.video.setZoom(1, 0, 0, 0, 0);
    this.s.video.zoomEnded();
  }

  buildPanel() {
    return h('div', { class: 'panel displays' }, orderedDisplays(this.list).map((d, i) => h('button', {
      class: `display-btn ${d.id === this.current ? 'current' : ''}`,
      'aria-pressed': d.id === this.current ? 'true' : 'false',
      onclick: () => { this.choose(d.id); this.s.toolbar.togglePanel('displays'); },
    },
    h('span', { class: 'display-name' }, t(d.primary ? 'display.nameMain' : 'display.name', { i: i + 1 })),
    h('span', { class: 'display-size' }, `${d.w}×${d.h}`))));
  }
}
