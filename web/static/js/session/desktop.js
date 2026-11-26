// The picture and its controls, fitted into the browser session.
import { h } from '../dom.js';
import { store, HAS_TOUCH, HAS_MOUSE, orientation } from '../env.js';
import { t } from '../i18n.js';
import { CAPS, Control, Move } from '../proto.js';
import { Video } from './video.js';
import { Pointer } from './pointer.js';
import { Touch } from './touch.js';
import { MousePad } from './mouse-pad.js';
import { Transfers } from './transfer.js';
import { Clipboard } from './clipboard.js';
import { Displays } from './displays.js';
import { Pip } from './pip.js';

const BLOCKABLE = new Set([Control.MOVE, Control.MOVE_BY, Control.BUTTON, Control.KEY, Control.CHAR, Control.WHEEL, Control.TEXT]);
const INPUT = new Set([...BLOCKABLE, Control.SYNC]);
const ACTIVE_ONLY = new Set([...INPUT, Control.DISPLAY, Control.VIEW, Control.CLIP_SHARE, Control.CLIPBOARD, Control.CLIPBOARD_REQUEST]);
const CLIPBOARD = new Set([Control.CLIP_SHARE, Control.CLIPBOARD, Control.CLIPBOARD_REQUEST, Control.CLIPBOARD_DATA, Control.CLIPBOARD_ERROR]);

export class Desktop {
  constructor(s) {
    this.s = s;
    s.video = new Video(s);
    s.pointer = new Pointer(s);
    s.displays = new Displays(s);
    s.touch = new Touch(s);
    s.mousePad = new MousePad(s);
    s.transfer = new Transfers(s);
    s.clip = new Clipboard(s);
    s.pip = new Pip(s.video.el);
    this.lockNote = h('div', { class: 'notice dark warn', hidden: true, role: 'status' }, t('session.locked'));
    s.stack.append(this.lockNote);
    s.stage.prepend(s.video.el, s.pointer.img, s.video.note);
    s.stage.setAttribute('data-drop', t('session.drop'));
    this.buttons = [s.displays.button, s.touch.modeBtn, s.mousePad.button, s.osk.button, s.pointer.lockBtn, s.clip.ui.button];
    this.picture = [s.video.pictureButton()];
    this.waiting = 'session.waitingPicture';
  }

  bind() {
    const s = this.s;
    s.toolbar.addPanel('displays', s.displays.button, () => s.displays.buildPanel());
    s.toolbar.addPanel('clip', s.clip.ui.button, () => s.clip.ui.buildPanel());
    s.pointer.bind();
    s.transfer.bindDrop();
    s.touch.syncMode();
    s.pointer.syncCaps();
    s.clip.ui.syncButton();
  }

  async view() {
    const { t: _view, ...view } = (await this.s.video.constraints()) || { w: 0, h: 0, maxW: 0, maxH: 0, fps: 0 };
    return view;
  }

  get statsMode() { return this.s.video.statsMode; }
  get blocked() { return !this.s.hasCap(CAPS.INPUT) || !!this.s.video.captureState; }
  get busy() { return this.s.transfer.busy(); }

  keyboardSink() {
    const s = this.s;
    return {
      send: (m) => s.send(m),
      get acceptsModifiedText() { return s.hasCap(CAPS.CHARS); },
      compositionKey: (ch) => ch.toLowerCase(),
      get keyLabels() {
        return s.device.os === 'macos' ? {
          MetaLeft: ['⌘', 'Command'],
          AltLeft: ['⌥', t('kb.key.macAltLeft')],
          AltRight: ['⌥', t('kb.key.macAltRight')],
        } : null;
      },
    };
  }

  canSend(e) {
    const s = this.s;
    return (!ACTIVE_ONLY.has(e.t) || s.connected)
      && (!BLOCKABLE.has(e.t) || !s.video.captureState)
      && (!INPUT.has(e.t) || s.hasCap(CAPS.INPUT));
  }

  useChannels({ move, file }) {
    const s = this.s;
    s.moveChannel = move;
    file.binaryType = 'arraybuffer';
    file.bufferedAmountLowThreshold = 256 * 1024;
    file.onmessage = (e) => s.transfer.onMessage(e.data);
    file.onopen = () => s.transfer.pump();
    s.transfer.attach(file);
    move.onmessage = (e) => s.onHostMessage(e.data);
    // The host chooses H.264 from what this offer lists.
    const tr = s.connection.pc.addTransceiver('video', { direction: 'recvonly' });
    if ('playoutDelayHint' in tr.receiver) tr.receiver.playoutDelayHint = 0;
    if ('jitterBufferTarget' in tr.receiver) tr.receiver.jitterBufferTarget = 0;
  }

  track(e) { this.s.video.show(e.streams[0] || new MediaStream([e.track])); }

  active() {
    const s = this.s;
    s.video.sendView();
    s.displays.restore();
    s.clip.check();
  }

  recover() {
    const s = this.s;
    s.touch.reset();
    s.mousePad.release();
    s.pointer.resetMotion();
    s.stage.classList.remove('dropping');
  }

  message(m) {
    const s = this.s;
    if (m.t === Move.CURSOR_POS) s.pointer.onPos(m);
    else if (m.t === Control.CURSOR_SHAPE) s.pointer.onShape(m);
    else if (m.t === Control.DISPLAYS) s.displays.onDisplays(m);
    else if (m.t === Control.CAPTURE) s.video.onCapture(m.v);
    else if (CLIPBOARD.has(m.t)) s.clip.onHostMessage(m);
    else if (m.t === Control.DESK) this.lockNote.hidden = m.v === 'default';
    else if (m.t === Control.LAYOUT) s.osk.setLayout(m);
  }

  applyCaps(first) {
    const s = this.s;
    const input = s.hasCap(CAPS.INPUT);
    for (const b of [s.osk.button, s.touch.modeBtn, s.mousePad.button]) {
      if (b) b.hidden = !input;
    }
    if (!input) s.osk.toggle(false);
    s.displays.syncButton();
    s.pointer.syncCaps();
    s.clip.ui.button.hidden = !s.hasCap(CAPS.CLIPBOARD) && !s.hasCap(CAPS.FILES);
    if (!first) return;
    if (s.hasCap(CAPS.CURSOR)) s.send({ t: Control.LOCAL_CURSOR, on: true });
    s.keyboard.needSync();
    s.video.startView();
  }

  updateControls(ready) {
    const s = this.s;
    s.mousePad.el?.toggleAttribute('inert', !ready);
    s.pointer.draw();
    if (ready && HAS_TOUCH && s.hasCap(CAPS.INPUT) && !this.inputPanelsRestored) {
      this.inputPanelsRestored = true;
      if (store.get('deskway.mousepad') === '1') s.mousePad.toggle(true);
      if (!HAS_MOUSE() && store.get('deskway.hwkbd') !== '1') s.osk.toggle(true, { auto: true });
    }
  }

  beginIme() { this.s.video.beginIme(); }
  placeIme(area, stripTop) { this.s.video.placeIme(area, stripTop); }
  endIme() { this.s.video.endIme(); }
  placeKeypad(pad) {
    this.s.root.style.paddingBottom = pad && orientation() === 'portrait' ? `${pad.offsetHeight + 8}px` : '';
  }

  layout() {
    this.s.mousePad.layout();
    this.s.pointer.draw();
  }

  destroy() {
    const s = this.s;
    s.touch.stopInertia();
    s.mousePad.release();
    s.pip.destroy();
    s.video.reset();
    s.transfer.destroy();
    s.clip.destroy();
    s.pointer.destroy();
  }
}
