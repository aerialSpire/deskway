// The remote pointer: the mouse on the stage, the pointer's place and shape as the computer
// reports them, and the pointer drawn here (for fingers), so it never lags behind the video.
// With Pointer Lock (a mode of the session, for games and 3D programs), the mouse sends
// relative motion instead; Esc leaves it, as browsers require.
//
// Public surface: img, cursor {x, y} (0..1 on the remote screen), remote {shape, visible,
// active}, touchInput, locked, lockBtn, onShape(m), onPos(m), draw(), queueMove(), toNorm(e),
// button(index, down), click(index), resetMotion(), toggleLock(), syncCaps(), bind(), destroy().
import { h } from '../dom.js';
import { buzz, COARSE, HAS_MOUSE } from '../env.js';
import { t } from '../i18n.js';
import { CAPS, Control } from '../proto.js';
import { moveTo, moveBy, reportIsStale } from './input-messages.js';

const DEFAULT_CURSOR = { url: `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="20" height="28" viewBox="0 0 20 28"><path d="M1 1v22l6-6 4 9 3-1.5-4-8.5h8z" fill="#000" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/></svg>')}`, w: 20, h: 28, hx: 1, hy: 1 };
const BUTTONS = { 0: 0, 1: 1, 2: 2 };

export class Pointer {
  constructor(s) {
    this.s = s;
    this.img = h('img', { class: 'rcursor', alt: '', hidden: true, 'aria-hidden': 'true' });
    this.cursor = { x: 0.5, y: 0.5 };
    this.remote = { shape: null, visible: false, active: false };
    // One increasing sequence for absolute and relative moves; pointer reports carry the last
    // one the computer applied.
    this.moveSeq = 0;
    this.lastReportSeq = 0;
    this.movePending = false;
    // The latest move sent, repeated reliably when the motion stops (settle).
    this.lastMove = null;
    this.skipMotion = false;
    // Relative motion not sent yet, in remote pixels.
    this.rel = { x: 0, y: 0 };
    this.locked = false;
    // Which pointer drives the screen now: a finger draws the remote pointer, a mouse does not.
    this.touchInput = false;
    this.lockBtn = h('button', { class: 'tool', type: 'button', hidden: true, 'aria-pressed': 'false', title: t('tb.pointerLockLabel'), 'aria-label': t('tb.pointerLockLabel'), onclick: () => this.toggleLock() }, t('tb.pointerLock'));
  }

  // Mouse on the stage; fingers and pens go to touch.
  bind() {
    const s = this.s;
    const stage = s.stage;
    stage.addEventListener('pointerdown', (e) => {
      if (s.blocked) return;
      // Keep the local input method open when a tap sets the remote text cursor.
      if (!s.osk.imeShown) stage.focus();
      s.keyboard.noteEvent(e);
      try { stage.setPointerCapture(e.pointerId); } catch { /* the pointer is already gone */ }
      if (e.pointerType === 'mouse') {
        if (!(e.button in BUTTONS)) return;
        if (!this.locked) this.cursor = this.toNorm(e);
        this.button(BUTTONS[e.button], true);
      } else {
        s.touch.down(e);
      }
      e.preventDefault();
    });
    stage.addEventListener('pointermove', (e) => {
      if (e.pointerType === 'mouse') {
        s.keyboard.noteEvent(e);
        if (this.locked) {
          this.queueRelative(e);
          return;
        }
        // Pushing the mouse against the top edge reveals the toolbar.
        if (e.clientY - stage.getBoundingClientRect().top <= 2 && e.buttons === 0) s.toolbar.show();
        this.cursor = this.toNorm(e);
        this.queueMove();
      } else {
        s.touch.move(e);
      }
    });
    stage.addEventListener('pointerup', (e) => {
      if (e.pointerType === 'mouse') {
        if (e.button in BUTTONS) {
          if (!this.locked) this.cursor = this.toNorm(e);
          this.button(BUTTONS[e.button], false);
        }
      } else {
        s.touch.up(e);
      }
    });
    stage.addEventListener('pointercancel', (e) => { if (e.pointerType !== 'mouse') s.touch.up(e, true); });
    stage.addEventListener('wheel', (e) => {
      e.preventDefault();
      const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
      s.send({ t: Control.WHEEL, dx: e.deltaX * unit, dy: e.deltaY * unit });
    }, { passive: false });
    stage.addEventListener('pointerdown', (e) => { this.touchInput = e.pointerType === 'touch'; }, true);
    this.onLockChange = () => this.syncLock();
    document.addEventListener('pointerlockchange', this.onLockChange);
  }

  onShape(m) {
    const canvas = document.createElement('canvas');
    canvas.width = m.w;
    canvas.height = m.h;
    const bytes = Uint8ClampedArray.from(atob(m.d), (c) => c.charCodeAt(0));
    if (bytes.length !== m.w * m.h * 4) return;
    canvas.getContext('2d').putImageData(new ImageData(bytes, m.w, m.h), 0, 0);
    const url = canvas.toDataURL();
    this.remote.shape = { url, w: m.w, h: m.h, hx: m.hx, hy: m.hy };
    this.remote.active = true;
    this.img.src = url;
    // With a mouse, the local pointer takes the remote shape and moves with no delay at all.
    this.s.stage.style.cursor = `url(${url}) ${m.hx} ${m.hy}, default`;
    this.draw();
  }

  // Absolute moves already set the pointer's local position. During Pointer Lock, the computer
  // supplies the position, so its latest report is useful even with newer moves in flight.
  onPos(m) {
    this.remote.active = true;
    const requiredSeq = this.locked ? this.lastReportSeq : this.moveSeq;
    if (reportIsStale(m.s, requiredSeq, !this.locked && this.movePending)) return;
    this.lastReportSeq = m.s;
    this.remote.visible = !!m.v;
    if (m.v) this.cursor = { x: m.x, y: m.y };
    this.draw();
  }

  draw() {
    const img = this.img;
    const show = !this.s.blocked && (this.locked || COARSE || this.touchInput) && this.remote.active && this.remote.visible;
    img.hidden = !show;
    if (!show) return;
    const shape = this.remote.shape || DEFAULT_CURSOR;
    if (img.src !== shape.url) img.src = shape.url;
    const r = this.s.video.rect();
    const box = this.s.stage.getBoundingClientRect();
    const vw = this.s.video.el.videoWidth || r.w;
    // Remote pixels per screen pixel, kept within reason so the pointer stays easy to see.
    const k = Math.min(1.25, Math.max(0.7, r.w / vw));
    img.style.width = `${shape.w * k}px`;
    img.style.height = `${shape.h * k}px`;
    const x = r.left - box.left + this.cursor.x * r.w - shape.hx * k;
    const y = r.top - box.top + this.cursor.y * r.h - shape.hy * k;
    img.style.transform = `translate(${x}px, ${y}px)`;
  }

  // Moves go on the unordered channel, at most once a frame, numbered.
  queueMove() {
    // A finger moving the pointer shows it until the computer says otherwise.
    if (this.touchInput) this.remote.visible = true;
    this.draw();
    this.schedule();
  }

  // Motion under Pointer Lock, in remote pixels: the picture's scale, so the pointer there moves
  // as far as the mouse moves over the picture here.
  queueRelative(e) {
    if (this.s.blocked) return;
    // Chrome reports a large, bogus movement in the first event after the lock changes (the
    // distance to where the pointer was): it is left out.
    if (this.skipMotion) { this.skipMotion = false; return; }
    const r = this.s.video.rect();
    const k = (this.s.video.el.videoWidth || r.w) / (r.w || 1);
    this.rel.x += e.movementX * k;
    this.rel.y += e.movementY * k;
    this.schedule();
  }

  schedule() {
    if (this.movePending) return;
    this.movePending = true;
    requestAnimationFrame(() => this.sendMove());
  }

  sendMove() {
    this.movePending = false;
    if (this.s.blocked) return;
    let m;
    if (this.locked) {
      const { x, y } = this.rel;
      this.rel = { x: 0, y: 0 };
      if (!x && !y) return;
      m = moveBy(x, y, this.moveSeq + 1);
    } else {
      m = moveTo(this.cursor.x, this.cursor.y, this.moveSeq + 1);
    }
    this.moveSeq = m.s;
    this.lastMove = m;
    this.s.sendMove(JSON.stringify(m));
    requestAnimationFrame(() => this.settle(m));
  }

  resetMotion() {
    this.rel = { x: 0, y: 0 };
    this.lastMove = null;
  }

  // No new move by the next frame: the motion stopped. Its last move goes again on the reliable
  // channel, so a loss on the move channel never leaves the pointer (and its reports) behind;
  // the computer drops the copy if the first one arrived.
  settle(m) {
    if (this.lastMove !== m || this.movePending) return;
    this.s.send(m);
  }

  // ---- Pointer Lock ----

  // Offered when the computer takes relative moves, this browser can lock the pointer, and a
  // mouse or touchpad is attached (a capability, not a kind of device: a phone with a mouse
  // plugged in gets it too). Re-checked when a mouse comes or goes.
  syncCaps() {
    this.lockBtn.hidden = !(this.s.hasCap(CAPS.RELMOUSE) && 'requestPointerLock' in Element.prototype && HAS_MOUSE());
    if (!this.mouseWatch) {
      this.mouseWatch = matchMedia('(any-pointer: fine)');
      this.onMouseChange = () => this.syncCaps();
      this.mouseWatch.addEventListener('change', this.onMouseChange);
    }
  }

  // Must run inside a click, which browsers require.
  async toggleLock() {
    const stage = this.s.stage;
    if (this.locked) { document.exitPointerLock(); return; }
    try {
      // Raw motion, without this computer's acceleration, where the browser has it.
      await stage.requestPointerLock({ unadjustedMovement: true });
    } catch {
      try { await stage.requestPointerLock(); } catch { /* refused: the mode stays off */ }
    }
  }

  syncLock() {
    const locked = document.pointerLockElement === this.s.stage;
    if (locked === this.locked) return;
    this.locked = locked;
    this.rel = { x: 0, y: 0 };
    this.skipMotion = true;
    this.lockBtn.setAttribute('aria-pressed', String(locked));
    if (locked) {
      this.s.toolbar.hide();
      this.s.note(t('session.pointerLocked'), 4000);
    }
    this.draw();
  }

  // A point on the page as a place on the remote screen.
  toNorm(e) {
    const r = this.s.video.rect();
    return {
      x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.w)),
      y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.h)),
    };
  }

  button(index, down) {
    this.s.send({ t: Control.BUTTON, b: index, d: down, x: this.cursor.x, y: this.cursor.y });
  }

  click(index) {
    this.button(index, true);
    this.button(index, false);
    buzz(index === 2 ? 15 : 6);
  }

  destroy() {
    if (this.locked) document.exitPointerLock();
    document.removeEventListener('pointerlockchange', this.onLockChange);
    if (this.mouseWatch) this.mouseWatch.removeEventListener('change', this.onMouseChange);
  }
}
