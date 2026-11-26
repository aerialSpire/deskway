// The surface the end-to-end tests use (tests/e2e), as `window.__deskway`. Installed only when
// this browser has `localStorage['deskway.test'] = '1'`, which the test harness sets before the
// page loads. Tests read and drive the page through this and the page's own elements, never
// through the modules' insides; the test doubles (recorded input, a fake file channel, messages
// as if from the computer) live here and nowhere else.
import { store } from './env.js';
import { socket } from './net.js';
import { state, setView, currentSession } from './state.js';
import { openSession, press } from './views/takeover.js';
import { Session } from './session/session.js';
import { Control } from './proto.js';
import { locating } from './here.js';

const copy = (x) => JSON.parse(JSON.stringify(x));
const byId = (id) => state.devices.find((d) => d.id === id);
const handles = new WeakMap();

function sessionHandle(s) {
  const v = s.video?.el;
  const c = s.connection;
  const real = { channel: null, send: null };
  const recording = [];
  const views = [];
  const moves = [];
  let dropMoves = false;
  const heard = [];
  return {
    // What the session shows and holds now.
    info() {
      const p = s.pointer;
      return {
        device: s.device.id,
        id: s.id,
        state: c.state,
        cause: c.cause,
        epoch: c.epoch,
        caps: s.caps ? [...s.caps] : null,
        connection: c.pc ? c.pc.connectionState : null,
        // The computer's ICE credentials: an ICE restart changes them on the same connection.
        ufrag: c.pc && c.pc.remoteDescription ? (/a=ice-ufrag:(\S+)/.exec(c.pc.remoteDescription.sdp) || [])[1] || null : null,
        input: s.inputChannel ? s.inputChannel.readyState : null,
        files: s.transfer.open() ? 'open' : null,
        w: v.videoWidth,
        h: v.videoHeight,
        auth: c.authKind,
        resumable: c.resumable,
        displays: copy(s.displays.list),
        display: s.displays.current,
        pointer: { active: p.remote.active, shape: p.remote.shape ? `${p.remote.shape.w}x${p.remote.shape.h}` : null, x: p.cursor.x, y: p.cursor.y },
        full: s.toolbar.isFull,
        direct: s.touch.direct,
        zoom: s.video.zoom.s,
        capture: s.video.captureState,
        view: s.video.lastView ? JSON.parse(s.video.lastView) : null,
        pressed: [...s.keyboard.pressed],
        mods: Object.fromEntries(s.osk.mods),
        share: s.clip.share,
        offered: !!s.clip.remote,
        news: s.clip.news,
        clip: { gen: s.clip.gen, owner: s.clip.owner, serving: !!s.clip.serving, fetching: s.clip.fetching.size, pendingText: !!s.clip.pendingText },
        transferring: !!s.transfer.current,
        locked: s.pointer.locked,
        moveSeq: s.pointer.moveSeq,
      };
    },
    // Connection and video numbers from the peer connection.
    async stats() {
      const out = { state: c.pc && c.pc.connectionState, w: v.videoWidth, h: v.videoHeight, input: s.inputChannel && s.inputChannel.readyState, route: s.toolbar.routeText.textContent };
      const t = v.srcObject && v.srcObject.getVideoTracks()[0];
      Object.assign(out, { paused: v.paused, ready: v.readyState, muted: t && t.muted, track: t && t.readyState, hidden: document.hidden });
      if (!c.pc) return out;
      const stats = await c.pc.getStats();
      let pair = null;
      stats.forEach((r) => {
        if (r.type === 'transport' && r.selectedCandidatePairId) pair = stats.get(r.selectedCandidatePairId);
        if (r.type === 'inbound-rtp' && r.kind === 'video') {
          Object.assign(out, { decoded: r.framesDecoded, bytes: r.bytesReceived, dropped: r.framesDropped, lost: r.packetsLost, codec: r.codecId, decodeMs: r.framesDecoded ? 1000 * r.totalDecodeTime / r.framesDecoded : 0, decoder: r.decoderImplementation });
          for (const k of ['packetsReceived', 'framesReceived', 'keyFramesDecoded', 'pliCount', 'firCount', 'nackCount', 'freezeCount', 'frameWidth']) out[k] = r[k];
        }
      });
      if (pair) {
        const local = stats.get(pair.localCandidateId);
        Object.assign(out, { local: local?.candidateType, relayProtocol: local?.relayProtocol, remote: stats.get(pair.remoteCandidateId)?.candidateType, rtt: pair.currentRoundTripTime });
      }
      return out;
    },
    // The session's elements, to look at and to press like a person would.
    el: {
      get stage() { return s.stage; },
      get video() { return v; },
      get toolbar() { return s.toolbar.el; },
      get panels() { return s.toolbar.panels; },
      get route() { return s.toolbar.routeText; },
      get routeDot() { return s.toolbar.routeDot; },
      get cursor() { return s.pointer.img; },
      get stack() { return s.stack; },
      get lockNote() { return s.content.lockNote; },
      get displayBtn() { return s.displays.button; },
      get kbdBtn() { return s.osk.button; },
      get clipBtn() { return s.clip.ui.button; },
      get modeBtn() { return s.touch.modeBtn; },
      get pictureBtn() { return s.video.pictureBtn; },
      get padBtn() { return s.mousePad.button; },
      get lockBtn() { return s.pointer.lockBtn; },
      get keypad() { return s.osk.keypad; },
      get typer() { return s.osk.typer; },
      get box() { return s.keyboard.box; },
      get mousePad() { return s.mousePad.el; },
      get xferCard() { return s.transfer.progress.el; },
      get clipNote() { return s.clip.ui.progress.el; },
    },
    // Connection timeline: state changes of the peer connection from now on.
    startTrace() {
      const pc = c.pc;
      const t0 = performance.now();
      this.marks = [];
      const mark = (k, val) => this.marks.push(`${k}=${val}@${Math.round(performance.now() - t0)}`);
      pc.addEventListener('icegatheringstatechange', () => mark('gather', pc.iceGatheringState));
      pc.addEventListener('iceconnectionstatechange', () => mark('ice', pc.iceConnectionState));
      pc.addEventListener('connectionstatechange', () => mark('conn', pc.connectionState));
      pc.addEventListener('signalingstatechange', () => mark('signal', pc.signalingState));
    },
    trace() { return (this.marks || []).join(' '); },

    // The connection: an ICE restart on the same peer connection, as after a network blip.
    restartIce: () => c.restart(),
    // What a person does.
    leave: () => s.leave(),
    confirmLeave: () => s.confirmLeave(),
    chooseDisplay: (id) => s.displays.choose(id),
    togglePanel: (kind) => s.toolbar.togglePanel(kind),
    keyboard: (open, opts) => s.osk.toggle(open, opts),
    mousePad: (on) => s.mousePad.toggle(on),
    showToolbar: (hold = false) => { s.toolbar.show(); s.toolbar.hover = hold; },
    hideToolbar: () => { s.toolbar.hover = false; s.toolbar.hide(); },
    fullscreen: () => s.toolbar.toggleFullscreen(),
    focus: () => s.stage.focus(),
    // Picture in picture as the browser starts it when the page goes to the background.
    pipEnter: () => s.pip.enter(),
    pipActive: () => s.pip.active,
    // The computer sends a whole picture now (it sends one only when the screen changes otherwise).
    keyframe: () => s.send({ t: Control.CAPTURE_KEYFRAME }),
    releaseAll: () => s.releaseAll(),
    sendFiles: (files) => s.transfer.send(files),
    takeFromRemote: () => s.clip.take(),
    // This device's clipboard is looked at now, as on focus.
    checkClipboard: () => s.clip.check(),
    dismissTransfer: () => s.transfer.dismiss(),
    cancelTransfer: () => s.transfer.cancel(),
    dropQueue: () => { s.transfer.queue = []; },
    resetZoom: () => { s.video.setZoom(1, 0, 0, 0, 0); s.video.zoomEnded(); },
    layout: () => s.layout(),
    videoRect: () => s.video.rect(),
    toNorm: (x, y) => s.pointer.toNorm({ clientX: x, clientY: y }),
    // Whether closing the tab now would ask first.
    leavePageAsks() { let asked = false; s.onLeavePage({ preventDefault: () => { asked = true; } }); return asked; },

    // Test doubles.
    // Input is kept here instead of sent (the view still goes: it is not input).
    // Pointer moves are noted too, and still sent (unless dropped, as if lost: dropMoves).
    record() {
      real.send ||= s.send;
      real.sendMove ||= s.sendMove;
      recording.length = 0;
      views.length = 0;
      moves.length = 0;
      s.send = (e) => {
        if (e.t === Control.VIEW) views.push(copy(e));
        return e.t === Control.VIEW ? real.send.call(s, e) : recording.push(copy(e));
      };
      s.sendMove = (json) => { moves.push(JSON.parse(json)); if (!dropMoves) real.sendMove.call(s, json); };
    },
    recorded: () => copy(recording),
    recordedMoves: () => copy(moves),
    dropMoves: (on) => { dropMoves = on; },
    // The view messages sent while recording (they still go to the computer).
    viewsSent: () => copy(views),
    clearViews: () => { views.length = 0; },
    clearRecorded: () => { recording.length = 0; moves.length = 0; },
    stopRecording() {
      if (real.send) { s.send = real.send; real.send = null; }
      if (real.sendMove) { s.sendMove = real.sendMove; real.sendMove = null; }
      dropMoves = false;
    },
    // What the computer says on the input channel is noted from now on.
    hearHost() {
      heard.length = 0;
      if (real.hear) return;
      real.hear = s.onHostMessage;
      s.onHostMessage = (d) => { try { heard.push(JSON.parse(d)); } catch { /* not JSON */ } return real.hear.call(s, d); };
    },
    heard: () => copy(heard),
    // A message straight to the computer, also while input is recorded.
    toHost: (m) => (real.send || s.send).call(s, m),
    // A message on the input channel, as if from the computer.
    fromHost: (m) => s.onHostMessage(JSON.stringify(m)),
    // As if the computer's control hello had not arrived yet.
    forgetHello: () => { s.caps = null; },
    // A message (text or bytes) on the file channel, as if from the computer.
    fromFiles: (data) => s.transfer.onMessage(data),
    // Transfers go to `fake` (an object like an RTCDataChannel) instead; null puts the real one back.
    useFileChannel(fake) {
      if (fake) { real.channel ||= s.transfer.channel; s.transfer.channel = fake; }
      else if (real.channel) { s.transfer.channel = real.channel; real.channel = null; }
    },
    setRemoteOs: (os) => { s.device.os = os; },
    // Browsers on a Mac lose keyups under Command; on (true) or off (false) for the test.
    setMacCommand: (on) => { s.keyboard.macCommand = on; },
    togglePointerLock: () => s.pointer.toggleLock(),
    setStall: (ms) => { s.transfer.stall = ms; },
    // The computer's own pointer reports are ignored (the test host's real mouse would move it).
    ignorePointerReports: () => { s.pointer.onPos = () => {}; },
    setCursor: (x, y) => { s.pointer.cursor = { x, y }; },
    // Taken as a device with (true) or without (false) a touch screen; null: as it really is.
  };
}

// The computer's caps as a test double: the test host runs view-only (it must not type on the
// machine running the tests), so its hello lacks input and files. With
// `localStorage['deskway.test.caps']` (a JSON list) the page takes those caps from the hello
// instead; the harness sets every cap so the input and file UIs are tested.
let capsOverride = null;
function overrideHelloCaps() {
  const real = Session.prototype.onHostMessage;
  Session.prototype.onHostMessage = function (data) {
    if (capsOverride && !this.caps) {
      try {
        const m = JSON.parse(data);
        if (m.t === Control.HELLO) data = JSON.stringify({ ...m, caps: capsOverride });
      } catch { /* not ours to judge */ }
    }
    return real.call(this, data);
  };
}

export function installTestApi() {
  const original = socket.onMessage;
  try { capsOverride = JSON.parse(store.get('deskway.test.caps') || 'null'); } catch { capsOverride = null; }
  overrideHelloCaps();
  // The tests' browser runs on the computer they test: it says so only when a test asks.
  locating(false);
  window.__deskway = {
    view: () => state.view,
    deviceId: () => state.deviceId,
    devices: () => copy(state.devices),
    me: () => copy(state.me),
    socketOpen: () => socket.isOpen(),
    show: (view, deviceId) => setView(view, deviceId),
    open: (deviceId, { force = false } = {}) => openSession(byId(deviceId), force),
    // Pressing the computer's picture, as a person does.
    press: (deviceId) => press(byId(deviceId)),
    // A signaling message, as if the server had sent it.
    serverSays: (m) => socket.onMessage(m),
    // Whether this page says where it runs (here.js).
    locating: (on) => locating(on),
    // `fn(m)` sees (and may change) every signaling message before the page; null stops it.
    interceptSignals(fn) { socket.onMessage = fn ? (m) => original(fn(m)) : original; },
    // The caps sessions opened from now on take from the computer's hello; null: the real ones.
    hostCaps(list) { capsOverride = list; },
    // Drops the signaling socket as a network change would; it reconnects by itself.
    dropSignaling: () => socket.ws && socket.ws.close(),
    // The socket as a sleeping phone may leave it: still open, but nothing it is sent arrives.
    deafenSignaling: () => { if (socket.ws) socket.ws.onmessage = null; },
    session() {
      const s = currentSession();
      if (!s) return null;
      if (!handles.has(s)) handles.set(s, sessionHandle(s));
      return handles.get(s);
    },
  };
}
