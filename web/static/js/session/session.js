// A browser session and the chrome around its desktop or data-only views.
// Connection owns negotiation and liveness. The view owns what that connection carries.
import { $app, h, spinner } from '../dom.js';
import { store, randomId } from '../env.js';
import { t } from '../i18n.js';
import { socket, iceServers } from '../net.js';
import { connectAuth, answerSigned, pinnedHostKey, pinHostKey } from '../auth.js';
import { PROTOCOL_VERSION, MIN_VERSION, Control } from '../proto.js';
import { dialog } from '../dialogs.js';
import { Connection } from './connection.js';
import { ConnectionStats } from './connection-stats.js';
import { ConnectionDetails } from './connection-details.js';
import { Keyboard } from './keyboard.js';
import { OnscreenKeyboard } from './onscreen-keyboard.js';
import { Toolbar } from './toolbar.js';
import { Desktop } from './desktop.js';
import { DataView } from './data-view.js';
import { terminalDisconnectActions } from '../terminal-ui.js';

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
  constructor(device, { force = false, view = 'desktop', full = true, onEnd = () => {} } = {}) {
    this.device = device;
    this.onEnd = onEnd;
    this.closed = false;
    this.id = randomId();
    // The computer's capabilities, from its hello on the control channel.
    this.caps = null;
    this.waiting = null;

    this.notice = h('div', { class: 'notice dark passive', hidden: true, role: 'status' });
    this.stack = h('div', { class: 'stack' }, this.notice);
    const dataOnly = view !== 'desktop';
    this.stage = h('div', { class: dataOnly ? 'stage terminal-stage' : 'stage', tabindex: '0',
      'aria-label': t(dataOnly ? 'terminal.title' : 'session.screen') }, dataOnly ? null : this.stack);
    this.osk = new OnscreenKeyboard(this);
    this.content = dataOnly ? new DataView(this, view) : new Desktop(this);
    this.keyboard = new Keyboard(this, this.content.keyboardSink());
    this.toolbar = new Toolbar(this, this.content.buttons, this.content.picture || [], this.content.navigation);
    const details = new ConnectionDetails(() => this.toolbar.closePanel(this.toolbar.routeButton));
    this.toolbar.addPanel('connection', this.toolbar.routeButton, () => details.el);
    this.connectionStats = new ConnectionStats(() => this.content.statsMode, (sample) => {
      this.toolbar.route(sample.path.route, details.update(sample));
    });
    this.toolbar.showAi(this.device);
    this.root = h('div', { class: 'session' }, this.toolbar.el, this.toolbar.tab, this.stage,
      dataOnly ? this.content.files.el : null, dataOnly ? this.stack : null, this.toolbar.panels);
    $app.replaceChildren(this.root);

    this.toolbar.bind();
    this.keyboard.bind();
    this.bindPage();
    this.content.bind();
    if (!this.stage.hidden) this.stage.focus();
    this.toolbar.show();

    // Taking over goes full screen, unless this device turned that off. Runs inside the tap
    // that started the session, which browsers require.
    if (full && store.get('deskway.autofull') !== '0') this.toolbar.toggleFullscreen();
    // The stage reports progress until its contents are ready.
    this.setWaiting(t(this.content.connectingNotice || 'session.connecting'));

    this.connection = new Connection({
      id: this.id,
      device: device.id,
      force,
      dataOnly,
      deps: {
        socket,
        iceServers,
        Peer: RTCPeerConnection,
        sign: connectAuth,
        verify: verifyAnswer,
        view: () => this.content.view?.() || { w: 0, h: 0, maxW: 0, maxH: 0, fps: 0 },
        timers: { set: (fn, ms) => setTimeout(fn, ms), clear: (id) => clearTimeout(id) },
      },
    });
    this.connection
      .on('channels', (c) => this.useChannels(c))
      .on('track', (e) => this.content.track?.(e))
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
    this.content.layout?.();
  }

  // ---- for the session's parts ----

  get connected() { return this.connection?.state === 'active' && this.channelOpen; }

  get channelOpen() { return this.inputChannel?.readyState === 'open'; }

  get blocked() { return !this.connected || this.content.blocked; }

  send(e) {
    if (!this.channelOpen || this.connection.ended || this.content.canSend?.(e) === false) return false;
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
  useChannels(channels) {
    this.inputChannel = channels.input;
    channels.input.onmessage = (e) => this.onHostMessage(e.data);
    this.content.useChannels(channels);
  }

  // The stage explains what is being waited for. Only a change of connection state starts or
  // stops measurements or changes the controls; a signaling event can repeat the same state.
  onState({ state, cause }) {
    const was = this.lastConnectionState;
    this.lastConnectionState = state;
    if (was !== state) this.updateRemoteControls();
    if (state === 'active') {
      if (was === state) return;
      if (this.waiting) this.setWaiting(t(this.content.waiting));
      else this.showNotice(null);
      this.toolbar.route('connected');
      this.connectionStats.start(this.connection.pc);
      if (!this.helloSent) {
        this.helloSent = this.send({ t: Control.HELLO, version: PROTOCOL_VERSION, caps: [] });
      }
      this.content.active();
    } else if (state === 'recovering') {
      if (was !== state) {
        this.connectionStats.stop();
        this.releaseAll();
        this.keyboard.needSync();
        this.content.recover?.();
      }
      this.toolbar.route('reconnecting');
      this.showNotice(t(this.content.recoveringNotice || AWAY_TEXT[cause]));
    } else if (state === 'connecting') {
      this.setWaiting(t(this.content.connectingNotice || (cause === 'network' ? 'session.connecting' : AWAY_TEXT[cause])));
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
    this.content.message?.(m);
  }

  // The computer's hello says what it can do; a later one (a newer one from the same computer)
  // updates that. The first also starts what waits for the computer to be known.
  onHello(m) {
    if (!(m.version >= MIN_VERSION)) { this.connection.end('old'); return; }
    const first = !this.caps;
    this.caps = new Set(Array.isArray(m.caps) ? m.caps : []);
    this.content.applyCaps(first);
    if (this.closed) return;
    this.updateRemoteControls();
    this.layout();
  }

  // What the computer can do, from its hello; nothing is guessed. The one place the client
  // reads caps from.
  hasCap(cap) { return !!this.caps && this.caps.has(cap); }

  updateRemoteControls() {
    const ready = this.connected && !!this.caps && (this.content.controlsReady ?? true);
    this.toolbar.setRemoteAvailable(ready);
    this.osk.keypad?.toggleAttribute('inert', !ready);
    this.content.updateControls?.(ready);
  }

  // ---- what the stage says about the connection ----

  // A neutral progress message on the stage, cleared when the view is ready.
  setWaiting(text) {
    this.waiting = text;
    this.notice.hidden = false;
    this.notice.replaceChildren(spinner(), text);
  }

  ready() {
    if (this.waiting) { this.waiting = null; this.showNotice(null); }
  }

  showNotice(text) {
    // The waiting message stays until the view is ready.
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
  leave() {
    this.connection.end('left');
  }

  // Ending a session by accident is costly (a long task loses its watcher), so it asks.
  confirmLeave() {
    const busy = this.content.busy;
    const cancel = () => (this.content.filesVisible ? this.content.files.el : this.stage).focus({ preventScroll: true });
    const leave = () => this.leave();
    const title = t('leave.title', { name: this.device.name });
    if (this.content.hasTerminal && this.content.keeps) {
      dialog(title, t('terminal.leaveKeeping'),
        terminalDisconnectActions(t, cancel, () => this.content.terminate(), leave, this.connected && this.content.canClose));
      return;
    }
    const text = this.content.hasTerminal ? t('terminal.leave')
      : this.content.currentView ? t('files.leave') : busy ? t('leave.textBusy', { n: busy }) : t('leave.text');
    dialog(title, text, [
      { label: t('common.cancel'), run: cancel },
      { label: t('tb.disconnect'), primary: true, run: leave },
    ]);
  }

  // The connection ended, however: everything of the session stops, then the page is told.
  onEnded(end) {
    if (this.closed) return;
    this.releaseAll();
    this.closed = true;
    this.osk.toggle(false);
    this.connectionStats.stop();
    clearTimeout(this.flashTimer);
    this.content.destroy();
    this.keyboard.destroy();
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
