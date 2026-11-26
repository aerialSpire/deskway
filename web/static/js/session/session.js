// A remote session: one computer, shown and driven from this page. Its life (peer connection,
// negotiation, state, deadlines, attaching again, the end) is its `Connection`'s
// (connection.js); this module puts the session's parts together and shows what the connection
// says. Each part (video, pointer, touch, the mouse panel, keys, on-screen keyboard, transfers,
// the shared clipboard, displays, toolbar) lives in its own module and reaches the others
// through the session.
//
// Public surface, for the page:
//   new Session(device, { force, full, onEnd })  opens it (`full`: inside a tap, so it may go
//                                          full screen); `onEnd(end)` is called once when it
//                                          ends (the connection's 'ended' event)
//   session.id, session.device, session.closed, session.connection
//   session.leave()                        the person ends it (Disconnect, the back gesture, leaving
//                                          the page)
// For its parts:
//   send(e), sendMove(json), connected, blocked, hasCap(cap), note(text, ms), releaseAll(),
//   layout(), stage, root, stack, waiting, onPicture(), showNotice(text)
import { $app, h, spinner } from '../dom.js';
import { store, randomId, HAS_TOUCH, HAS_MOUSE } from '../env.js';
import { t } from '../i18n.js';
import { socket, iceServers } from '../net.js';
import { connectAuth, answerSigned, pinnedHostKey, pinHostKey } from '../auth.js';
import { PROTOCOL_VERSION, MIN_VERSION, CAPS, Control, Move } from '../proto.js';
import { dialog } from '../dialogs.js';
import { Connection } from './connection.js';
import { ConnectionStats } from './connection-stats.js';
import { ConnectionDetails } from './connection-details.js';
import { Video } from './video.js';
import { Pointer } from './pointer.js';
import { Touch } from './touch.js';
import { MousePad } from './mouse-pad.js';
import { Keyboard } from './keyboard.js';
import { OnscreenKeyboard } from './onscreen-keyboard.js';
import { Transfers } from './transfer.js';
import { Clipboard } from './clipboard.js';
import { Displays } from './displays.js';
import { Toolbar } from './toolbar.js';
import { Pip } from './pip.js';

// Input that a computer without a capturable display must not get.
const BLOCKABLE = new Set([Control.MOVE, Control.MOVE_BY, Control.BUTTON, Control.KEY, Control.CHAR, Control.WHEEL, Control.TEXT]);
// Everything that is input: not sent to a computer without the `input` cap.
const INPUT = new Set([...BLOCKABLE, Control.SYNC]);
// New user actions require the active session. In-flight protocol replies may still use an
// open channel while ICE recovers, so a clipboard or capture exchange can finish.
const ACTIVE_ONLY = new Set([...INPUT, Control.DISPLAY, Control.VIEW, Control.CLIP_SHARE, Control.CLIPBOARD, Control.CLIPBOARD_REQUEST]);
// The shared clipboard's messages on the control channel.
const CLIPBOARD = new Set([Control.CLIP_SHARE, Control.CLIPBOARD, Control.CLIPBOARD_REQUEST, Control.CLIPBOARD_DATA, Control.CLIPBOARD_ERROR]);
// What the stage says while the session is not active, by why.
const AWAY_TEXT = { network: 'session.reconnecting', host: 'session.hostAway', server: 'session.serverAway' };

// Whether the computer's answer is signed by it, and by the computer this browser met before:
// 'ok', 'first' (no key kept yet: it is kept now), 'changed' or 'bad'.
async function verifyAnswer({ hostId, session, offerSdp, answerSdp, auth }) {
  const ok = await answerSigned({ hostId, session, offerSdp, answerSdp, hk: auth.hk, sig: auth.sig });
  if (!ok) return 'bad';
  const pinned = pinnedHostKey(hostId);
  if (pinned && pinned !== auth.hk) return 'changed';
  if (pinned) return 'ok';
  pinHostKey(hostId, auth.hk);
  return 'first';
}

export class Session {
  constructor(device, { force = false, full = true, onEnd = () => {} } = {}) {
    this.device = device;
    this.onEnd = onEnd;
    this.closed = false;
    this.id = randomId();
    // The computer's capabilities, from its hello on the control channel.
    this.caps = null;
    this.waiting = null;

    this.video = new Video(this);
    this.pointer = new Pointer(this);
    this.notice = h('div', { class: 'notice dark passive', hidden: true, role: 'status' });
    // Everything the session has to say, one under the other at the top right: connection,
    // lock, transfers, hints. Never over the middle of the screen, never moving it.
    this.lockNote = h('div', { class: 'notice dark warn', hidden: true, role: 'status' }, t('session.locked'));
    this.stack = h('div', { class: 'stack' }, this.notice, this.lockNote);
    // The words shown while files are dragged over it come from here (style.css: .dropping).
    this.stage = h('div', { class: 'stage', tabindex: '0', 'aria-label': t('session.screen'), 'data-drop': t('session.drop') }, this.video.el, this.pointer.img, this.video.note, this.stack);
    this.displays = new Displays(this);
    this.touch = new Touch(this);
    this.mousePad = new MousePad(this);
    this.keyboard = new Keyboard(this);
    this.osk = new OnscreenKeyboard(this);
    this.transfer = new Transfers(this);
    this.clip = new Clipboard(this);
    this.pip = new Pip(this.video.el);
    const remoteButtons = [this.displays.button, this.touch.modeBtn, this.mousePad.button, this.osk.button, this.pointer.lockBtn, this.clip.ui.button];
    this.toolbar = new Toolbar(this, remoteButtons, [this.video.pictureButton()]);
    const details = new ConnectionDetails(() => this.toolbar.closePanel(this.toolbar.routeButton));
    this.toolbar.addPanel('connection', this.toolbar.routeButton, () => details.el);
    this.connectionStats = new ConnectionStats(() => this.video.statsMode, (sample) => {
      this.toolbar.route(sample.path.route, details.update(sample));
    });
    this.toolbar.showAi(this.device);
    this.toolbar.addPanel('displays', this.displays.button, () => this.displays.buildPanel());
    this.toolbar.addPanel('clip', this.clip.ui.button, () => this.clip.ui.buildPanel());
    this.root = h('div', { class: 'session' }, this.toolbar.el, this.toolbar.tab, this.stage, this.toolbar.panels);
    $app.replaceChildren(this.root);

    this.toolbar.bind();
    this.pointer.bind();
    this.keyboard.bind();
    this.transfer.bindDrop();
    this.bindPage();
    this.touch.syncMode();
    this.pointer.syncCaps();
    this.clip.ui.syncButton();
    this.stage.focus();
    this.toolbar.show();

    // Taking over goes full screen, unless this device turned that off. Runs inside the tap
    // that started the session, which browsers require.
    if (full && store.get('deskway.autofull') !== '0') this.toolbar.toggleFullscreen();
    // Until the first picture, the stage says what is happening instead of staying black.
    this.setWaiting(t('session.connecting'));

    this.connection = new Connection({
      id: this.id,
      device: device.id,
      force,
      deps: {
        socket,
        iceServers,
        Peer: RTCPeerConnection,
        sign: connectAuth,
        verify: verifyAnswer,
        // The picture's size now: the computer starts at it while the connection is set up
        // (zeros, "not known", while the stage has no area).
        view: async () => {
          const { t: _view, ...view } = (await this.video.constraints()) || { w: 0, h: 0, maxW: 0, maxH: 0, fps: 0 };
          return view;
        },
        timers: { set: (fn, ms) => setTimeout(fn, ms), clear: (id) => clearTimeout(id) },
      },
    });
    this.connection
      .on('channels', (c) => this.useChannels(c))
      .on('track', (e) => this.video.show(e.streams[0] || new MediaStream([e.track])))
      .on('state', (s) => this.onState(s))
      .on('pinned', () => this.flash(t('session.firstTime')))
      .on('ended', (end) => this.onEnded(end));
    this.connection.start();
  }

  // The page around the session: resizing, leaving, hiding.
  bindPage() {
    // Keep the input bar and picture together within the visible viewport.
    this.onResize = () => this.layout();
    window.addEventListener('resize', this.onResize);
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', this.onResize);
      window.visualViewport.addEventListener('scroll', this.onResize);
    }
    navigator.virtualKeyboard?.addEventListener('geometrychange', this.onResize);
    // The browser takes Ctrl+W and the like before the page outside full screen: closing the
    // tab with a session open asks first.
    this.onLeavePage = (e) => { if (!this.closed) { e.preventDefault(); e.returnValue = ''; } };
    window.addEventListener('beforeunload', this.onLeavePage);
    this.onHidden = () => this.releaseAll();
    document.addEventListener('visibilitychange', this.onHidden);
    window.addEventListener('blur', this.onHidden);
    // Back in front: the lock and modifier state may have changed meanwhile.
    this.onFront = () => { if (!document.hidden) this.keyboard.needSync(); };
    document.addEventListener('visibilitychange', this.onFront);
    window.addEventListener('focus', this.onFront);
  }

  // Everything floating over the screen, placed again. The keyboard first: the room it leaves
  // is where the others go.
  layout() {
    this.osk.layout();
    if (this.toolbar.panelKind) this.toolbar.placePanels();
    this.mousePad.layout();
    this.pointer.draw();
  }

  // ---- for the session's parts ----

  get connected() { return this.connection?.state === 'active' && this.channelOpen; }

  get channelOpen() { return this.inputChannel?.readyState === 'open'; }

  // Input waits for a working control channel, the computer's permission and a display.
  get blocked() { return !this.connected || !this.hasCap(CAPS.INPUT) || !!this.video.captureState; }

  send(e) {
    if (!this.channelOpen || this.connection.ended || (ACTIVE_ONLY.has(e.t) && !this.connected)) return false;
    if (BLOCKABLE.has(e.t) && this.video.captureState) return false;
    if (INPUT.has(e.t) && !this.hasCap(CAPS.INPUT)) return false;
    this.inputChannel.send(JSON.stringify(e));
    return true;
  }

  sendMove(json) {
    if (this.blocked) return;
    if (this.moveChannel && this.moveChannel.readyState === 'open') this.moveChannel.send(json);
  }

  // Lets go of every key held, on the screen and on the physical keyboard.
  releaseAll() {
    this.osk.release();
    this.keyboard.release();
  }

  // A short note in the column at the top right, gone after a moment.
  note(text, ms = 3000) {
    const el = h('div', { class: 'notice dark', role: 'status' }, text);
    this.stack.append(el);
    setTimeout(() => el.remove(), ms);
  }

  // ---- what the connection says ----

  // The connection's data channels, as soon as they exist.
  useChannels({ input, move, file }) {
    this.inputChannel = input;
    this.moveChannel = move;
    file.binaryType = 'arraybuffer';
    file.bufferedAmountLowThreshold = 256 * 1024;
    file.onmessage = (e) => this.transfer.onMessage(e.data);
    file.onopen = () => this.transfer.pump();
    this.transfer.attach(file);
    input.onmessage = (e) => this.onHostMessage(e.data);
    move.onmessage = (e) => this.onHostMessage(e.data);
    // The host chooses H.264 from what this offer lists.
    const tr = this.connection.pc.addTransceiver('video', { direction: 'recvonly' });
    if ('playoutDelayHint' in tr.receiver) tr.receiver.playoutDelayHint = 0;
    if ('jitterBufferTarget' in tr.receiver) tr.receiver.jitterBufferTarget = 0;
  }

  // The stage explains what is being waited for. Only a change of connection state starts or
  // stops measurements or changes the controls; a signaling event can repeat the same state.
  onState({ state, cause }) {
    const was = this.lastConnectionState;
    this.lastConnectionState = state;
    if (was !== state) this.updateRemoteControls();
    if (state === 'active') {
      if (was === state) return;
      if (this.waiting) this.setWaiting(t('session.waitingPicture'));
      else this.showNotice(null);
      this.toolbar.route('connected');
      this.connectionStats.start(this.connection.pc);
      if (!this.helloSent) {
        this.helloSent = this.send({ t: Control.HELLO, version: PROTOCOL_VERSION, caps: [] });
      }
      this.video.sendView();
      this.displays.restore();
      this.clip.check();
    } else if (state === 'recovering') {
      if (was !== state) {
        this.connectionStats.stop();
        this.releaseAll();
        this.keyboard.needSync();
        this.touch.reset();
        this.mousePad.release();
        this.pointer.resetMotion();
        this.stage.classList.remove('dropping');
      }
      this.toolbar.route('reconnecting');
      this.showNotice(t(AWAY_TEXT[cause]));
    } else if (state === 'connecting') {
      this.setWaiting(t(cause === 'network' ? 'session.connecting' : AWAY_TEXT[cause]));
    }
  }

  onHostMessage(data) {
    let m;
    try { m = JSON.parse(data); } catch { return; }
    if (m.t === Control.HELLO) { this.onHello(m); return; }
    // Until the computer's control hello nothing it says is taken, and nothing says anything
    // about its version (pointer positions on the move channel can arrive first): `old` is
    // decided from the hello only.
    if (!this.caps) return;
    if (m.t === Move.CURSOR_POS) this.pointer.onPos(m);
    else if (m.t === Control.CURSOR_SHAPE) this.pointer.onShape(m);
    else if (m.t === Control.DISPLAYS) this.displays.onDisplays(m);
    else if (m.t === Control.CAPTURE) this.video.onCapture(m.v);
    else if (CLIPBOARD.has(m.t)) this.clip.onHostMessage(m);
    else if (m.t === Control.DESK) this.lockNote.hidden = m.v === 'default';
    else if (m.t === Control.LAYOUT) this.osk.setLayout(m);
  }

  // The computer's hello says what it can do; a later one (a newer one from the same computer)
  // updates that. The first also starts what waits for the computer to be known.
  onHello(m) {
    if (!(m.version >= MIN_VERSION)) { this.connection.end('old'); return; }
    const first = !this.caps;
    this.caps = new Set(Array.isArray(m.caps) ? m.caps : []);
    this.applyCaps();
    if (!first) return;
    // This page draws the pointer; a computer that reports it stops drawing it into the video.
    if (this.hasCap(CAPS.CURSOR)) this.send({ t: Control.LOCAL_CURSOR, on: true });
    // Lock and modifier state, and the viewer's constraints.
    this.keyboard.needSync();
    this.video.startView();
  }

  // What the computer can do, from its hello; nothing is guessed. The one place the client
  // reads caps from.
  hasCap(cap) { return !!this.caps && this.caps.has(cap); }

  applyCaps() {
    const input = this.hasCap(CAPS.INPUT);
    for (const b of [this.osk.button, this.touch.modeBtn, this.mousePad.button]) {
      if (b) b.hidden = !input;
    }
    if (!input) this.osk.toggle(false);
    this.displays.syncButton();
    this.pointer.syncCaps();
    this.clip.ui.button.hidden = !this.hasCap(CAPS.CLIPBOARD) && !this.hasCap(CAPS.FILES);
    this.updateRemoteControls();
    this.layout();
  }

  updateRemoteControls() {
    const ready = this.connected && !!this.caps;
    this.toolbar.setRemoteAvailable(ready);
    this.osk.keypad?.toggleAttribute('inert', !ready);
    this.mousePad.el?.toggleAttribute('inert', !ready);
    this.pointer.draw();
    if (ready && HAS_TOUCH && this.hasCap(CAPS.INPUT) && !this.inputPanelsRestored) {
      this.inputPanelsRestored = true;
      if (store.get('deskway.mousepad') === '1') this.mousePad.toggle(true);
      // With a touch input and no pointer or known physical keyboard, open the keyboard once.
      if (!HAS_MOUSE() && store.get('deskway.hwkbd') !== '1') this.osk.toggle(true, { auto: true });
    }
  }

  // ---- what the stage says about the connection ----

  // A neutral progress message on the stage, cleared by the first picture.
  setWaiting(text) {
    this.waiting = text;
    this.notice.hidden = false;
    this.notice.replaceChildren(spinner(), text);
  }

  // The first picture is on the screen.
  onPicture() {
    if (this.waiting) { this.waiting = null; this.showNotice(null); }
  }

  showNotice(text) {
    // The first-picture message stays until a picture arrives.
    if (!text && this.waiting) return;
    this.notice.hidden = !text;
    this.notice.replaceChildren();
    if (text) {
      this.notice.append(h('span', { class: 'dot warn', 'aria-hidden': 'true' }), text);
    }
  }

  flash(text) {
    clearTimeout(this.flashTimer);
    this.notice.hidden = false;
    this.notice.replaceChildren(h('span', { class: 'dot ok', 'aria-hidden': 'true' }), text);
    this.flashTimer = setTimeout(() => { if (this.notice.textContent === text) this.notice.hidden = true; }, 4000);
  }

  // ---- ending ----

  // The person ends the session (Disconnect, the back gesture, leaving the page).
  leave() { this.connection.end('left'); }

  // Ending a session by accident is costly (a long task loses its watcher), so it asks.
  confirmLeave() {
    const busy = this.transfer.busy();
    dialog(t('leave.title', { name: this.device.name }), busy ? t('leave.textBusy', { n: busy }) : t('leave.text'), [
      { label: t('common.cancel'), run: () => this.stage.focus({ preventScroll: true }) },
      { label: t('tb.disconnect'), primary: true, run: () => this.leave() },
    ]);
  }

  // The connection ended, however: everything of the session stops, then the page is told.
  onEnded(end) {
    if (this.closed) return;
    this.releaseAll();
    this.closed = true;
    this.touch.stopInertia();
    this.mousePad.release();
    this.connectionStats.stop();
    clearTimeout(this.flashTimer);
    this.pip.destroy();
    this.video.reset();
    this.transfer.destroy();
    this.clip.destroy();
    this.keyboard.destroy();
    this.pointer.destroy();
    document.removeEventListener('visibilitychange', this.onFront);
    window.removeEventListener('focus', this.onFront);
    window.removeEventListener('beforeunload', this.onLeavePage);
    document.removeEventListener('visibilitychange', this.onHidden);
    window.removeEventListener('blur', this.onHidden);
    window.removeEventListener('resize', this.onResize);
    if (window.visualViewport) {
      window.visualViewport.removeEventListener('resize', this.onResize);
      window.visualViewport.removeEventListener('scroll', this.onResize);
    }
    navigator.virtualKeyboard?.removeEventListener('geometrychange', this.onResize);
    this.toolbar.destroy();
    this.onEnd(end);
  }
}
