// Fingers on the remote screen (the mouse panel for touch screens is mouse-pad.js's).
// Trackpad mode: one finger moves the pointer, tap clicks, hold then move drags.
// Tap mode: tap clicks where the finger is, hold clicks right, hold then move drags.
// Both: two fingers scroll (with momentum) or pinch to zoom; a two-finger tap clicks right.
//
// Public surface: direct, modeBtn (touch screens only), down(e), move(e), up(e, cancelled),
// reset(), stopInertia(), toggleMode(), syncMode().
import { h } from '../dom.js';
import { store, buzz, HAS_TOUCH } from '../env.js';
import { t } from '../i18n.js';
import { Control } from '../proto.js';

const ZOOM_MAX = 6;

export class Touch {
  constructor(s) {
    this.s = s;
    this.touches = new Map();
    this.direct = store.get('deskway.touch') === 'direct';
    this.modeBtn = HAS_TOUCH ? h('button', { class: 'tool', 'aria-label': t('tb.modeLabel'), onclick: () => this.toggleMode() }) : null;
  }

  toggleMode() {
    this.direct = !this.direct;
    store.set('deskway.touch', this.direct ? 'direct' : 'trackpad');
    this.syncMode();
  }

  syncMode() {
    if (!this.modeBtn) return;
    this.modeBtn.textContent = this.direct ? t('tb.direct') : t('tb.trackpad');
    this.modeBtn.title = this.direct ? t('tb.directTip') : t('tb.trackpadTip');
    this.s.pointer.draw();
  }

  // Every finger lifted, nothing held (the computer lost its display).
  reset() {
    this.stopInertia();
    clearTimeout(this.longPress);
    this.touches.clear();
    // A pinch cut short still ends its zoom.
    if (this.twoFinger?.mode === 'pinch' && !this.twoFinger.zoomEnded) this.s.video.zoomEnded();
    this.twoFinger = null;
    this.dragging = false;
  }

  get pointer() { return this.s.pointer; }

  down(e) {
    const now = performance.now();
    const p = this.pointer;
    this.stopInertia();
    this.touches.set(e.pointerId, { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, t: now, lastT: now });
    if (this.touches.size === 1) {
      this.touchMoved = false;
      this.dragging = false;
      this.twoFinger = null;
      this.held = false;
      if (this.direct) {
        p.cursor = p.toNorm(e);
        p.queueMove();
      }
      this.longPress = setTimeout(() => {
        if (this.touchMoved) return;
        this.held = true;
        if (this.direct) {
          buzz(15);
        } else {
          this.dragging = true;
          p.button(0, true);
          buzz(15);
        }
      }, 400);
    } else if (this.touches.size === 2) {
      clearTimeout(this.longPress);
      if (this.dragging) { this.dragging = false; p.button(0, false); }
      const [a, b] = [...this.touches.values()];
      const midX = (a.x + b.x) / 2;
      const midY = (a.y + b.y) / 2;
      this.twoFinger = { dist: Math.hypot(a.x - b.x, a.y - b.y), midX, midY, startX: midX, startY: midY, startScale: this.s.video.zoom.s, mode: null, moved: false, t: now, vx: 0, vy: 0, lastT: now };
    }
  }

  move(e) {
    const f = this.touches.get(e.pointerId);
    if (!f) return;
    const p = this.pointer;
    const now = performance.now();
    const dx = e.clientX - f.x;
    const dy = e.clientY - f.y;
    const dt = Math.max(1, now - f.lastT);
    f.x = e.clientX;
    f.y = e.clientY;
    f.lastT = now;
    if (this.touches.size === 1) {
      if (Math.hypot(f.x - f.x0, f.y - f.y0) > 8) {
        if (!this.touchMoved && this.direct && this.held && !this.dragging) {
          // Held, then moved: drag from where the finger went down.
          this.dragging = true;
          p.button(0, true);
        }
        this.touchMoved = true;
        clearTimeout(this.longPress);
      }
      if (!this.touchMoved && !this.dragging) return;
      if (this.direct) {
        p.cursor = p.toNorm(e);
      } else {
        const r = this.s.video.rect();
        // Pointer acceleration: slow strokes are precise, fast ones cross the screen.
        const speed = Math.hypot(dx, dy) / dt;
        const gain = 1.1 + Math.min(3, Math.max(0, speed - 0.25) * 2.5);
        p.cursor = {
          x: Math.min(1, Math.max(0, p.cursor.x + (dx * gain) / r.w)),
          y: Math.min(1, Math.max(0, p.cursor.y + (dy * gain) / r.h)),
        };
        this.followCursor();
      }
      p.queueMove();
    } else if (this.touches.size === 2 && this.twoFinger) {
      const tf = this.twoFinger;
      const [a, b] = [...this.touches.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      const midX = (a.x + b.x) / 2;
      const midY = (a.y + b.y) / 2;
      // Decided once, from the whole movement so far: fingers report a few pixels at a time,
      // and in a pinch they rarely move exactly together.
      if (!tf.mode) {
        const spread = Math.abs(dist - tf.dist);
        const travel = Math.hypot(midX - tf.startX, midY - tf.startY);
        if (spread > 24 || travel > 10) tf.mode = spread > travel ? 'pinch' : 'scroll';
      }
      if (tf.mode) tf.moved = true;
      const mdx = midX - tf.midX;
      const mdy = midY - tf.midY;
      const mdt = Math.max(1, now - tf.lastT);
      if (tf.mode === 'scroll') {
        this.s.send({ t: Control.WHEEL, dx: -mdx * 3, dy: -mdy * 3 });
        // Smoothed finger velocity, for momentum after the fingers lift.
        tf.vx = tf.vx * 0.6 + (mdx / mdt) * 0.4;
        tf.vy = tf.vy * 0.6 + (mdy / mdt) * 0.4;
      } else if (tf.mode === 'pinch') {
        this.s.video.setZoom(Math.min(ZOOM_MAX, Math.max(1, tf.startScale * (dist / tf.dist))), midX, midY, mdx, mdy);
      }
      tf.midX = midX;
      tf.midY = midY;
      tf.lastT = now;
    }
  }

  up(e, cancelled) {
    const f = this.touches.get(e.pointerId);
    if (!f) return;
    const p = this.pointer;
    this.touches.delete(e.pointerId);
    clearTimeout(this.longPress);
    if (this.twoFinger) {
      // A finger lifting (or the touch cancelled) ends the pinch: the zoom is at rest now.
      if (this.twoFinger.mode === 'pinch' && !this.twoFinger.zoomEnded) {
        this.twoFinger.zoomEnded = true;
        this.s.video.zoomEnded();
      }
      if (this.touches.size === 0) {
        const tf = this.twoFinger;
        // Two fingers tapped together: a right click.
        if (!cancelled && !tf.moved && performance.now() - tf.t < 400) p.click(2);
        else if (!cancelled && tf.mode === 'scroll' && performance.now() - tf.lastT < 80) this.startInertia(tf.vx, tf.vy);
        this.twoFinger = null;
      }
      return;
    }
    if (this.dragging) {
      this.dragging = false;
      p.button(0, false);
    } else if (!cancelled && !this.touchMoved) {
      if (this.direct && this.held) p.click(2);
      else if (performance.now() - f.t < 300) p.click(0);
    }
  }

  startInertia(vx, vy) {
    if (Math.hypot(vx, vy) < 0.3) return;
    let last = performance.now();
    const step = (now) => {
      const dt = Math.min(50, now - last);
      last = now;
      vx *= Math.pow(0.95, dt / 16);
      vy *= Math.pow(0.95, dt / 16);
      if (Math.hypot(vx, vy) < 0.05) { this.inertia = null; return; }
      this.s.send({ t: Control.WHEEL, dx: -vx * dt * 3, dy: -vy * dt * 3 });
      this.inertia = requestAnimationFrame(step);
    };
    this.inertia = requestAnimationFrame(step);
  }

  stopInertia() {
    if (this.inertia) cancelAnimationFrame(this.inertia);
    this.inertia = null;
  }

  // When zoomed in, the view pans to keep the pointer away from the edges.
  followCursor() {
    const video = this.s.video;
    if (video.zoom.s <= 1 && !video.imeActive) return;
    const r = video.rect();
    const box = this.s.stage.getBoundingClientRect();
    const px = r.left + this.pointer.cursor.x * r.w;
    const py = r.top + this.pointer.cursor.y * r.h;
    const mx = box.width * 0.15;
    const my = box.height * 0.15;
    let panX = 0;
    let panY = 0;
    if (px < box.left + mx) panX = box.left + mx - px;
    else if (px > box.right - mx) panX = box.right - mx - px;
    if (py < box.top + my) panY = box.top + my - py;
    else if (py > box.bottom - my) panY = box.bottom - my - py;
    if (panX || panY) video.setZoom(video.zoom.s, box.left, box.top, panX, panY);
  }
}
