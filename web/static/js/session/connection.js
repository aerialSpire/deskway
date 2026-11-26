// A session's connection (docs/session-lifecycle.md): the peer connection, its negotiation, the
// state and its two deadlines, attaching again when the signaling socket comes back, and the end.
// Everything else in the session reads the state from here and asks this to end.
//
// Public surface:
//   new Connection({ id, device, force, deps })   deps: see `browserDeps` in session.js
//   connection.start()                             challenge, offer, answer; connecting
//   connection.end(reason)                         this side ends, however (a person, a
//                                                  deadline, the peer connection or the control
//                                                  channel failing): the computer is told
//   connection.state, connection.cause             connecting | active | recovering | ended;
//                                                  why it is not active: network | host | server
//   connection.channels                            control as input, and the channels for its view
//   connection.on(event, fn)                       'state' { state, cause }, 'channels',
//                                                  'track' (the RTCTrackEvent), 'pinned' (first
//                                                  answer from this computer: its key is kept),
//                                                  'ended' { reason, by, same, controller, via,
//                                                  established, cause }
//   connection.onSignal(m)                         nonce, attached, presence, answer, candidate, bye
//   connection.onSocketUp(), onSocketDown()        the signaling socket came back or is gone
//   connection.restart()                           an ICE restart (the next epoch)
//   connection.authKind, connection.resumable, connection.epoch
//
// Reasons are the wire's (proto.js REASONS) and a few of this page's own that never travel:
// `nokey` (this browser cannot sign), and the computer's identity not confirmed: `keyChanged`
// (its key is not the one kept) and `forged` (its answer's signature is not its key's). The
// computer is told those two as `auth` (OWN_REASONS).
import { Control, Signal, REASONS } from '../proto.js';
import { unb64 } from '../auth-messages.js';

// 30 s to connect (from the challenge) and 30 s to recover (from entering recovering), the same
// on both ends.
export const DEADLINE_MS = 30000;

// Resume tokens from the computers, by computer: this page's memory only, so a reload (which
// is leaving) signs with the device key again.
const resumes = new Map();
// Reasons after which the computer does not take this page back by its token.
const DROPS_TOKEN = new Set(['taken', 'ended', 'unpaired', 'auth', 'keyChanged', 'forged']);
// This page's own reasons, as the computer is told them.
const OWN_REASONS = { keyChanged: 'auth', forged: 'auth' };

export class Connection {
  constructor({ id, device, force = false, terminal = false, deps }) {
    this.id = id;
    this.device = device;
    this.force = !!force;
    this.terminal = terminal;
    this.deps = deps;
    this.state = 'connecting';
    this.handlers = {};
    this.channels = null;
    this.pc = null;
    // The negotiation waited for: its number, and whether its answer came.
    this.epoch = 0;
    this.answered = false;
    this.offerSdp = null;
    // The last negotiation message (challenge, offer or restart), sent again when the computer
    // or this page's socket is back while it is still unanswered.
    this.outstanding = null;
    // This epoch's own candidates; they follow its description, also when that is sent again.
    this.candidates = [];
    this.described = false;
    // This epoch's incoming candidates; null once its answer has been applied.
    this.pendingRemoteCandidates = [];
    this.nonce = null;
    this.waitNonce = null;
    this.view = null;
    this.authKind = null;
    // The server has answered for this session (a nonce came): it knows it, so a new socket
    // attaches it; until then a new socket asks the challenge again.
    this.heard = false;
    this.established = false;
    this.hostAway = false;
    this.serverDown = false;
    this.timer = null;
  }

  on(event, fn) { (this.handlers[event] ||= []).push(fn); return this; }

  emit(event, v) { for (const fn of this.handlers[event] || []) fn(v); }

  get ended() { return this.state === 'ended'; }

  get resumable() { return resumes.has(this.device); }

  get cause() { return this.serverDown ? 'server' : this.hostAway ? 'host' : 'network'; }

  setState(state) {
    this.state = state;
    this.emit('state', { state, cause: this.cause });
  }

  // The one deadline running now: connecting or recovering, 30 s; none while active.
  arm() {
    this.deps.timers.clear(this.timer);
    this.timer = this.deps.timers.set(() => this.end('failed'), DEADLINE_MS);
  }

  send(m) { this.deps.socket.send(m); }

  // ---- negotiation ----

  async start() {
    this.setState('connecting');
    this.arm();
    this.challenge();
    const ice = await this.deps.iceServers();
    if (this.ended) return;
    const pc = new this.deps.Peer({ iceServers: ice });
    this.pc = pc;
    const input = pc.createDataChannel(this.terminal ? 'control' : 'input');
    this.channels = this.terminal
      ? { input, terminal: pc.createDataChannel('terminal') }
      : { input, move: pc.createDataChannel('move', { ordered: false, maxRetransmits: 0 }), file: pc.createDataChannel('file') };
    this.emit('channels', this.channels);
    input.addEventListener('open', () => this.onChannelOpen());
    input.addEventListener('message', (e) => this.onControl(e.data));
    // A closed control channel ends the session at once (an `end` before it said why); the
    // computer is told, as for every end.
    input.addEventListener('close', () => this.end('failed'));
    pc.ontrack = (e) => this.emit('track', e);
    pc.onicecandidate = (e) => {
      if (!e.candidate) return;
      const m = { type: Signal.CANDIDATE, session: this.id, candidate: e.candidate.toJSON() };
      this.candidates.push(m);
      if (this.described) this.send(m);
    };
    pc.onconnectionstatechange = () => this.onPeerState();
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    this.offerSdp = pc.localDescription.sdp;
    this.view = await this.deps.view();
    await this.offer();
  }

  // Asks the computer for its nonce (again, when the first may not have arrived: the computer
  // gives the same one until it is used).
  challenge() {
    this.nonce = null;
    this.outstanding = { type: Signal.CHALLENGE, session: this.id, device: this.device };
    this.send(this.outstanding);
  }

  nextNonce() {
    return this.nonce ? Promise.resolve(this.nonce) : new Promise((resolve) => { this.waitNonce = resolve; });
  }

  // The offer (epoch 0), signed over the computer's nonce: with this page's resume token for
  // the computer if it has one, else with the device key.
  async offer() {
    const nonce = await this.nextNonce();
    if (this.ended) return;
    let auth;
    try {
      auth = await this.deps.sign({ hostId: this.device, session: this.id, nonce, sdp: this.offerSdp, force: this.force, resume: resumes.get(this.device) });
    } catch (e) {
      this.end(e && e.message === 'nokey' ? 'nokey' : 'failed');
      return;
    }
    if (this.ended) return;
    this.authKind = auth ? auth.k : null;
    this.describe({ type: Signal.OFFER, session: this.id, device: this.device, epoch: 0, sdp: this.offerSdp, view: this.view, force: this.force, auth });
  }

  // Sends this epoch's description, then its candidates found so far.
  describe(m) {
    this.outstanding = m;
    this.send(m);
    this.described = true;
    for (const c of this.candidates) this.send(c);
  }

  // An ICE restart on the same peer connection: new ICE credentials, the same DTLS, so the
  // computer takes it without a new proof. It replaces any restart still unanswered. While the
  // socket is down or the computer away it waits: attaching again, or the computer coming back,
  // restarts afresh.
  async restart() {
    const pc = this.pc;
    if (!pc || (this.state !== 'active' && this.state !== 'recovering')) return;
    if (this.hostAway || !this.deps.socket.isOpen()) return;
    const epoch = ++this.epoch;
    this.answered = false;
    this.described = false;
    this.candidates = [];
    this.pendingRemoteCandidates = [];
    pc.restartIce();
    const offer = await pc.createOffer();
    if (epoch !== this.epoch || this.ended) return;
    await pc.setLocalDescription(offer);
    if (epoch !== this.epoch || this.ended) return;
    this.offerSdp = pc.localDescription.sdp;
    this.describe({ type: Signal.RESTART, session: this.id, epoch, sdp: this.offerSdp });
  }

  // The computer is reachable again (attached, or its presence back): what was asked and not
  // answered may be lost. Recovering restarts afresh; connecting asks again.
  reachable() {
    if (this.state === 'recovering') this.restart();
    else if (this.state === 'connecting' && this.outstanding && !this.answered) {
      if (this.outstanding.type === Signal.CHALLENGE) this.challenge();
      else this.describe(this.outstanding);
    }
  }

  async onAnswer(m) {
    if (m.epoch !== this.epoch || this.answered) return;
    this.answered = true;
    const pc = this.pc;
    const verdict = await this.deps.verify({ hostId: this.device, session: this.id, offerSdp: this.offerSdp, answerSdp: m.sdp, auth: m.auth || {} });
    if (this.ended || m.epoch !== this.epoch) return;
    if (verdict === 'bad') { this.end('forged'); return; }
    if (verdict === 'changed') { this.end('keyChanged'); return; }
    if (verdict === 'first') this.emit('pinned');
    if (pc.signalingState !== 'have-local-offer') return;
    await pc.setRemoteDescription({ type: 'answer', sdp: m.sdp });
    if (this.ended || m.epoch !== this.epoch) return;
    const pending = this.pendingRemoteCandidates;
    this.pendingRemoteCandidates = null;
    for (const candidate of pending) this.addRemoteCandidate(candidate);
  }

  addRemoteCandidate(candidate) {
    this.pc.addIceCandidate(candidate).catch(() => { /* a stale candidate */ });
  }

  // ---- what the server and the computer say ----

  onSignal(m) {
    if (this.ended) return;
    switch (m.type) {
      case Signal.NONCE:
        this.heard = true;
        this.nonce = m.nonce;
        if (this.waitNonce) { this.waitNonce(m.nonce); this.waitNonce = null; }
        break;
      case Signal.ATTACHED:
      case Signal.PRESENCE:
        this.serverDown = false;
        this.hostAway = m.host === 'away';
        this.emit('state', { state: this.state, cause: this.cause });
        if (!this.hostAway) this.reachable();
        break;
      case Signal.ANSWER:
        this.onAnswer(m);
        break;
      case Signal.CANDIDATE:
        if (this.pendingRemoteCandidates) this.pendingRemoteCandidates.push(m.candidate);
        else this.addRemoteCandidate(m.candidate);
        break;
      case Signal.BYE:
        this.onBye(m);
        break;
    }
  }

  onBye(m) {
    // A resume token the computer no longer knows (it restarted, or the token expired): once
    // more, the same session with the device key, without asking.
    if (m.reason === 'auth' && this.authKind === 'rs' && this.state === 'connecting' && !this.answered) {
      resumes.delete(this.device);
      this.challenge();
      this.offer();
      return;
    }
    this.finish(m.reason, { by: m.by, same: m.same, controller: m.controller, via: 'bye' });
  }

  onControl(data) {
    let m;
    try { m = JSON.parse(data); } catch { return; }
    if (m.t === Control.END) this.finish(m.reason, { by: m.by, same: m.same, via: 'end' });
    else if (m.t === Control.RESUME && m.id && m.k) resumes.set(this.device, { id: m.id, k: unb64(m.k) });
  }

  onChannelOpen() {
    if (this.ended) return;
    this.established = true;
    this.deps.timers.clear(this.timer);
    this.setState('active');
  }

  // The peer connection is the authority on whether the session lives. Established, the path
  // lost (`disconnected`, and `failed` alike: a phone back from the background often reports
  // `failed` at once, and a restart may still bring it back) is recovering: an ICE restart on
  // this same connection, ended only by the recover deadline. A `failed` while recovering
  // restarts again once the restart before it was answered. Never established, `failed` ends it.
  onPeerState() {
    if (this.ended) return;
    const st = this.pc.connectionState;
    if (st === 'connected' && this.state === 'recovering') {
      this.deps.timers.clear(this.timer);
      this.setState('active');
    } else if ((st === 'disconnected' || st === 'failed') && this.state === 'active') {
      this.arm();
      this.setState('recovering');
      this.restart();
    } else if (st === 'failed' && this.state === 'recovering') {
      if (this.answered) this.restart();
    } else if (st === 'failed') {
      this.end('failed');
    }
  }

  // A new socket: a session the server has answered for is attached to it; one it has not
  // answered for asks its challenge again (never attach then: what went out on the old socket
  // may arrive after it, and the attach would be answered gone).
  onSocketUp() {
    if (this.ended) return;
    this.serverDown = false;
    if (this.heard) this.send({ type: Signal.ATTACH, session: this.id, device: this.device });
    else this.challenge();
    this.emit('state', { state: this.state, cause: this.cause });
  }

  onSocketDown() {
    if (this.ended) return;
    this.serverDown = true;
    this.emit('state', { state: this.state, cause: this.cause });
  }

  // ---- the end ----

  // This side ends the session, however it ends, and tells the computer: always through the
  // server (a socket being renewed sends it on the new one), and also on the control channel
  // while it is open. Whichever arrives first ends it there.
  end(reason) {
    if (this.ended) return;
    const wire = REASONS.includes(reason) ? reason : OWN_REASONS[reason] || 'left';
    const input = this.channels && this.channels.input;
    if (input && input.readyState === 'open') {
      try { input.send(JSON.stringify({ t: Control.END, reason: wire })); } catch { /* closing anyway */ }
    }
    this.send({ type: Signal.BYE, session: this.id, reason: wire });
    this.finish(reason, { via: 'local' });
  }

  // The session is over: nothing more is sent or taken.
  finish(reason, { by, same, controller, via }) {
    if (this.ended) return;
    if (DROPS_TOKEN.has(reason)) resumes.delete(this.device);
    this.deps.timers.clear(this.timer);
    const cause = this.cause;
    this.state = 'ended';
    if (this.pc) {
      this.pc.onconnectionstatechange = null;
      this.pc.onicecandidate = null;
      this.pc.close();
    }
    this.emit('ended', { reason, by, same: !!same, controller, via, established: this.established, cause });
  }
}
