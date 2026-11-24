// The remote screen's picture: the video, where it sits on the stage (letterboxed, zoomed), the
// size it is shown at (the `view` the host encodes for), and the computer's display going away
// and coming back.
//
// Public surface: el, note, show(stream), rect(), zoom, setZoom(scale, cx, cy, panX, panY),
// refit(), zoomEnded(), constraints(), startView(), sendView(), statsMode, pictureButton(), captureState (null | 'no-display' |
// 'recovering'), onCapture(status), reset().
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { Control } from '../proto.js';
import { decodeLimit, viewport, viewMessage, watchPixelRatio } from './constraints.js';
import { picture, setPicture } from '../picture.js';
import { pictureArea } from './ime-viewport.js';

export class Video {
  constructor(s) {
    this.s = s;
    this.el = h('video', { autoplay: true, playsinline: true, muted: true });
    this.el.muted = true;
    this.note = h('div', { class: 'capture-note', hidden: true, role: 'status' });
    this.zoom = { s: 1, x: 0, y: 0 };
    // The stage and picture sizes the zoom was last set against.
    this.fitted = null;
    this.captureState = null;
    this.recovery = null;
    // What this browser decodes smoothly: asked once per session.
    this.limit = decodeLimit(navigator.mediaCapabilities).catch(() => null);
    this.lastView = null;
    this.resizes = null;
    // The first picture clears the progress message, whichever event shows it first.
    const pictured = () => { if (this.el.videoWidth > 0) this.s.onPicture(); };
    this.el.onplaying = pictured;
    this.el.onloadeddata = pictured;
    this.el.addEventListener('resize', () => { this.refit(); pictured(); });
  }

  show(stream) {
    this.el.srcObject = stream;
    this.el.play().catch(() => {});
  }

  get statsMode() { return !this.captureState && this.el.videoWidth > 0 ? picture() : null; }

  beginIme() {
    const box = this.s.stage.getBoundingClientRect();
    const image = this.rect();
    const { width, height } = box;
    this.imeView = {
      width, height,
      bottom: Math.min(height, Math.max(0, image.top + image.h - box.top)),
      zoom: this.zoom,
      fitted: { w: width, h: height, ...this.fit(width, height) },
    };
  }

  placeIme(view, stripTop) {
    const { width, height } = this.imeView;
    const area = pictureArea(view, stripTop, height);
    const stage = this.s.stage;
    this.imeActive = true;
    stage.style.setProperty('--ime-left', `${area.x}px`);
    stage.style.setProperty('--ime-top', `${area.y}px`);
    stage.style.setProperty('--ime-width', `${area.w}px`);
    stage.style.setProperty('--ime-height', `${area.h}px`);
    stage.style.setProperty('--ime-picture-width', `${width}px`);
    stage.style.setProperty('--ime-picture-height', `${height}px`);
    stage.classList.add('ime-stage');
    this.paint();
  }

  endIme() {
    const saved = this.imeView;
    this.imeView = null;
    if (!this.imeActive) return;
    this.imeActive = false;
    this.s.stage.classList.remove('ime-stage');
    this.zoom = saved.zoom;
    this.fitted = saved.fitted;
    this.refit();
    this.sendView();
  }

  // The stage clips the picture. During input the drawing frame keeps its previous size
  // and moves so the last visible row of the picture just meets the keyboard bar.
  frame(box = this.s.stage.getBoundingClientRect()) {
    if (!this.imeActive) return box;
    const { width, height, bottom } = this.imeView;
    return { left: box.left + (box.width - width) / 2, top: box.bottom - bottom, width, height };
  }

  // The on-screen video rectangle, taking letterboxing and zoom into account.
  rect() {
    const box = this.frame();
    const { vw, vh, fit } = this.fit(box.width, box.height);
    const w = vw * fit * this.zoom.s;
    const hh = vh * fit * this.zoom.s;
    const left = box.left + (box.width - vw * fit) / 2 * this.zoom.s + this.zoom.x;
    const top = box.top + (box.height - vh * fit) / 2 * this.zoom.s + this.zoom.y;
    return { left, top, w, h: hh };
  }

  // Zooms to `scale` around (cx, cy) on the page, moved by (panX, panY), never past an edge.
  setZoom(scale, cx, cy, panX, panY) {
    const stage = this.s.stage.getBoundingClientRect();
    const box = this.frame(stage);
    const px = cx - box.left;
    const py = cy - box.top;
    const k = scale / this.zoom.s;
    let x = px - (px - this.zoom.x) * k + panX;
    let y = py - (py - this.zoom.y) * k + panY;
    x = Math.min(0, Math.max(box.width * (1 - scale), x));
    const fit = this.fit(box.width, box.height);
    if (this.imeActive) {
      const pictureHeight = fit.vh * fit.fit;
      const min = stage.bottom - box.top - (box.height + pictureHeight) / 2 * scale;
      const max = stage.top - box.top - (box.height - pictureHeight) / 2 * scale;
      y = Math.max(min, Math.min(max, y));
    } else {
      y = Math.min(0, Math.max(box.height * (1 - scale), y));
    }
    this.zoom = { s: scale, x, y };
    this.fitted = { w: box.width, h: box.height, ...fit };
    this.paint(stage, box);
    this.s.pointer.draw();
  }

  paint(stage = this.s.stage.getBoundingClientRect(), frame = this.frame(stage)) {
    const { s, x, y } = this.zoom;
    const dx = frame.left - stage.left + x;
    const dy = frame.top - stage.top + y;
    const transform = s !== 1 || dx || dy ? `translate(${dx}px, ${dy}px) scale(${s})` : '';
    this.el.style.transform = transform;
  }

  // The picture's size and its letterboxed scale on a stage of w x h.
  fit(w, h) {
    const vw = this.el.videoWidth || w;
    const vh = this.el.videoHeight || h;
    return { vw, vh, fit: Math.min(w / vw, h / vh) };
  }

  // The stage or the picture changed size (keyboard, toolbar, rotation, full screen, a new
  // resolution): what was in the middle of the stage stays in the middle, as far as the edges
  // allow. Unzoomed, the picture simply sits centred.
  refit() {
    const box = this.frame();
    const { s, x, y } = this.zoom;
    const was = this.fitted;
    if (was && s > 1) {
      // The middle as a point of the picture (0..1), then the pan that puts it in the new middle.
      const u = ((was.w / 2 - x) / s - (was.w - was.vw * was.fit) / 2) / (was.vw * was.fit);
      const v = ((was.h / 2 - y) / s - (was.h - was.vh * was.fit) / 2) / (was.vh * was.fit);
      const now = this.fit(box.width, box.height);
      this.zoom = {
        s,
        x: box.width / 2 - s * ((box.width - now.vw * now.fit) / 2 + u * now.vw * now.fit),
        y: box.height / 2 - s * ((box.height - now.vh * now.fit) / 2 + v * now.vh * now.fit),
      };
    }
    this.setZoom(s, 0, 0, 0, 0);
  }

  // A zoom has come to rest (the pinch's fingers lifted, or a zoom set in one step): the host
  // hears the new view once, not at every step of the gesture.
  zoomEnded() {
    this.sendView();
  }

  // The connection is up: the host hears the view now, whenever the stage changes size
  // (window, rotation, full screen, the keyboard), when the device pixel ratio changes (another
  // monitor), and when a zoom ends. A stage that changed size also refits the zoom.
  startView() {
    this.lastView = null;
    if (!this.resizes) {
      this.resizes = new ResizeObserver(() => {
        if (!this.imeActive) this.refit();
        this.sendView();
      });
      this.resizes.observe(this.s.stage);
      this.unwatchPixelRatio = watchPixelRatio(window, () => this.sendView());
    }
    this.sendView();
  }

  // The viewer's constraints now: the stage in device pixels times the zoom, and what this
  // browser decodes smoothly. Sent with the offer, then as `view` on the control channel. None
  // while the stage has no area: the computer keeps the last ones.
  async constraints() {
    const limit = await this.limit;
    const vp = viewport(this.frame(), window.devicePixelRatio, this.zoom.s);
    return vp && viewMessage(vp, limit, picture() === 'sharp');
  }

  // Sharp / Smooth in the toolbar: switches this browser's picture at once (the host hears a new
  // view), and is remembered like the setting on My devices.
  pictureButton() {
    const label = () => {
      const now = picture();
      this.pictureBtn.textContent = t(`tb.${now}`);
      this.pictureBtn.setAttribute('aria-label', t('tb.pictureLabel', { mode: t(`picture.${now}`) }));
    };
    this.pictureBtn = h('button', { class: 'tool', onclick: () => {
      setPicture(picture() === 'sharp' ? 'smooth' : 'sharp');
      label();
      this.sendView();
    } });
    label();
    return this.pictureBtn;
  }

  async sendView() {
    if (!this.resizes || !this.s.connected) return;
    const m = await this.constraints();
    const key = JSON.stringify(m);
    if (!m || !this.resizes || key === this.lastView) return;
    if (this.s.send(m)) this.lastView = key;
  }

  // The computer has no display to capture ('no-display'), or has one again ('ready'). Input
  // stops meanwhile, and starts again with the first new frame actually presented.
  onCapture(status) {
    const s = this.s;
    if (status === 'no-display') {
      s.releaseAll();
      s.touch.reset();
      this.recovery = null;
      this.captureState = 'no-display';
      s.waiting = null;
      s.showNotice(null);
      this.note.textContent = t('session.noDisplay');
      this.note.hidden = false;
      s.pointer.draw();
      return;
    }
    if (status !== 'ready' || this.captureState !== 'no-display' || s.closed) return;
    this.captureState = 'recovering';
    this.note.textContent = t('session.displayBack');
    const recovery = this.recovery = {};
    // The first new frame may have been presented before this message: the keyframe asked for
    // here is presented after it, and that is the frame waited for.
    this.el.requestVideoFrameCallback(() => {
      if (s.closed || this.recovery !== recovery) return;
      this.finishRecovery();
    });
    s.send({ t: Control.CAPTURE_KEYFRAME });
  }

  finishRecovery() {
    this.s.releaseAll();
    this.recovery = null;
    this.captureState = null;
    this.note.hidden = true;
    this.s.pointer.draw();
  }

  // The session ended: no more watching the stage, and no picture held.
  reset() {
    this.resizes?.disconnect();
    this.resizes = null;
    this.unwatchPixelRatio?.();
    this.unwatchPixelRatio = null;
    this.lastView = null;
    this.recovery = null;
    if (this.captureState) this.el.srcObject = null;
    this.captureState = null;
    this.note.hidden = true;
  }
}
