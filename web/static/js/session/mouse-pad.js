// The mouse panel for touch screens: left and right buttons and a scroll strip, at the screen's
// edge. Portrait: a row low on the screen, below the picture where the phone has room.
// Landscape: a column at the right edge. Either way it moves by its grip, and where it was put
// is remembered per orientation.
//
// Public surface: button (touch screens only), el (null while closed), toggle(on), release(), layout().
import { h, keepEvents } from '../dom.js';
import { store, buzz, HAS_TOUCH, orientation } from '../env.js';
import { t } from '../i18n.js';
import { Control } from '../proto.js';
import { draggable, savedPlace, savePlace } from './floating.js';

export class MousePad {
  constructor(s) {
    this.s = s;
    this.el = null;
    this.button = HAS_TOUCH ? h('button', { class: 'tool', 'aria-pressed': 'false', onclick: () => this.toggle() }, t('tb.mouse')) : null;
  }

  build() {
    const p = this.s.pointer;
    const left = h('button', { class: 'pad-btn', 'aria-label': t('pad.leftLabel') }, t('pad.left'));
    const right = h('button', { class: 'pad-btn', 'aria-label': t('pad.rightLabel') }, t('pad.right'));
    // The hint after the word, shown in a row only, comes from data-hint (style.css).
    const strip = h('div', { class: 'pad-scroll', role: 'slider', 'aria-label': t('pad.scrollLabel') }, h('span', { 'data-hint': t('pad.scrollHint') }, t('pad.scroll')));
    // Left: a tap clicks; holding it keeps the button down so a finger on the screen drags,
    // until the next tap. The press stays this button's while the finger wanders on it.
    let holdTimer = null;
    let heldDown = false;
    left.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      try { left.setPointerCapture(e.pointerId); } catch { /* the pointer is already gone */ }
      holdTimer = setTimeout(() => {
        holdTimer = null;
        heldDown = true;
        left.classList.add('down');
        p.button(0, true);
        buzz(15);
      }, 450);
    });
    left.addEventListener('pointerup', () => {
      if (holdTimer) {
        clearTimeout(holdTimer);
        holdTimer = null;
        if (heldDown) {
          heldDown = false;
          left.classList.remove('down');
          p.button(0, false);
        } else {
          p.click(0);
        }
      }
    });
    // Taken away by the system mid-press: not a click; a hold already made stays until the next tap.
    left.addEventListener('pointercancel', () => {
      clearTimeout(holdTimer);
      holdTimer = null;
    });
    right.addEventListener('pointerdown', (e) => e.preventDefault());
    right.addEventListener('pointerup', () => p.click(2));
    // The strip: dragging down scrolls down, as fast as the finger goes.
    let lastY = null;
    strip.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      lastY = e.clientY;
      try { strip.setPointerCapture(e.pointerId); } catch { /* fine */ }
      strip.classList.add('active');
    });
    strip.addEventListener('pointermove', (e) => {
      if (lastY === null) return;
      const dy = e.clientY - lastY;
      lastY = e.clientY;
      if (dy) this.s.send({ t: Control.WHEEL, dx: 0, dy: dy * 4 });
    });
    const end = () => { lastY = null; strip.classList.remove('active'); };
    strip.addEventListener('pointerup', end);
    strip.addEventListener('pointercancel', end);
    // A grip to move the panel anywhere over the screen.
    const grip = h('div', { class: 'pad-grip', 'aria-label': t('pad.gripLabel') }, h('span'), h('span'), h('span'));
    const pad = h('div', { class: 'mousepad', 'aria-label': t('pad.label') }, grip, left, right, strip);
    let from = null;
    draggable(grip, {
      start: () => {
        const box = this.s.stage.getBoundingClientRect();
        const r = pad.getBoundingClientRect();
        from = { x: r.left - box.left, y: r.top - box.top };
      },
      move: (dx, dy) => {
        const box = this.s.stage.getBoundingClientRect();
        const r = pad.getBoundingClientRect();
        const x = Math.min(box.width - r.width, Math.max(0, from.x + dx));
        const y = Math.min(box.height - r.height, Math.max(0, from.y + dy));
        this.place({ x: x / box.width, y: y / box.height });
      },
      end: () => { if (pad.place) savePlace('pad', pad.place); },
    });
    // The panel's pointer events are its own: the screen under it never sees them.
    keepEvents(pad, ['pointerdown', 'pointerup', 'pointercancel', 'pointermove', 'touchstart', 'contextmenu']);
    pad.release = () => {
      clearTimeout(holdTimer);
      holdTimer = null;
      end();
      if (!heldDown) return;
      heldDown = false;
      left.classList.remove('down');
      p.button(0, false);
    };
    return pad;
  }

  release() { this.el?.release(); }

  layout() {
    const pad = this.el;
    if (!pad) return;
    pad.classList.toggle('row', orientation() === 'portrait');
    this.place(savedPlace('pad'));
  }

  place(place) {
    const pad = this.el;
    if (!pad) return;
    pad.place = place;
    if (place && Number.isFinite(place.x) && Number.isFinite(place.y)) {
      Object.assign(pad.style, { left: `${place.x * 100}%`, top: `${place.y * 100}%`, right: 'auto', bottom: 'auto', transform: 'none' });
    } else {
      Object.assign(pad.style, { left: '', top: '', right: '', bottom: '', transform: '' });
    }
  }

  toggle(on) {
    const show = on ?? !this.el;
    if (show && !this.el) {
      this.el = this.build();
      this.s.stage.append(this.el);
      this.layout();
    } else if (!show && this.el) {
      this.release();
      this.el.remove();
      this.el = null;
    }
    if (this.button) this.button.setAttribute('aria-pressed', String(!!this.el));
    store.set('deskway.mousepad', this.el ? '1' : '0');
  }
}
