// The browser's session connection (web/static/js/session/connection.js) with a fake peer
// connection, a fake signaling socket and a fake clock: negotiation epochs, the two deadlines,
// attaching again, the computer's presence, and every way it ends.
//
//   node --test tests/unit/connection.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Connection, DEADLINE_MS } from '../../web/static/js/session/connection.js';
import { CAPS, Control, Signal } from '../../web/static/js/proto.js';

globalThis.matchMedia = () => ({ matches: false });
globalThis.document = {
  getElementById: () => null,
  createElement: () => ({ append() {}, addEventListener() {}, replaceChildren() {}, setAttribute() {} }),
};

const { store } = await import('../../web/static/js/env.js');
const { Displays, chosenDisplay, rememberDisplay } = await import('../../web/static/js/session/displays.js');

class FakeChannel extends EventTarget {
  constructor(label) { super(); this.label = label; this.readyState = 'connecting'; this.sent = []; }
  send(d) { this.sent.push(JSON.parse(d)); }
  open() { this.readyState = 'open'; this.dispatchEvent(new Event('open')); }
  message(m) { const e = new Event('message'); e.data = JSON.stringify(m); this.dispatchEvent(e); }
}

class FakePeer {
  constructor() {
    this.channels = {};
    this.signalingState = 'stable';
    this.connectionState = 'new';
    this.offers = 0;
    this.restarts = 0;
    this.remote = [];
    this.remoteCandidates = [];
    this.closed = false;
  }
  createDataChannel(label) { this.channels[label] = new FakeChannel(label); return this.channels[label]; }
  async createOffer() { this.offers += 1; return { type: 'offer', sdp: `sdp${this.offers}` }; }
  async setLocalDescription(o) { this.localDescription = o; this.signalingState = 'have-local-offer'; }
  async setRemoteDescription(a) {
    await this.applyingAnswer;
    this.remote.push(a.sdp);
    this.signalingState = 'stable';
  }
  restartIce() { this.restarts += 1; }
  async addIceCandidate({ candidate }) { this.remoteCandidates.push(candidate); }
  close() { this.closed = true; }
  set(state) { this.connectionState = state; this.onconnectionstatechange(); }
  candidate(c) { this.onicecandidate({ candidate: { toJSON: () => ({ candidate: c }) } }); }
}

// A clock whose timers run only when it is moved on.
function fakeTimers() {
  let now = 0;
  let next = 1;
  const due = new Map();
  return {
    set: (fn, ms) => { due.set(next, { fn, at: now + ms }); return next++; },
    clear: (id) => due.delete(id),
    advance(ms) {
      now += ms;
      for (const [id, x] of [...due]) if (x.at <= now) { due.delete(id); x.fn(); }
    },
  };
}

const settle = async () => { for (let i = 0; i < 20; i++) await new Promise((r) => setImmediate(r)); };

let ids = 0;
// A connection to computer `pc1` and everything it talks to; `verdict` is what checking an
// answer says.
function setup({ force = false, verdict = 'ok', socketOpen = true, dataOnly = false, device = { id: 'pc1' } } = {}) {
  const socket = { open: socketOpen, sent: [], isOpen() { return this.open; }, send(m) { this.sent.push(m); } };
  const timers = fakeTimers();
  let peer = null;
  const env = { socket, timers, verdict, signed: [] };
  const c = new Connection({
    id: `S${++ids}`,
    device,
    force,
    dataOnly,
    deps: {
      socket,
      timers,
      iceServers: async () => [],
      Peer: class extends FakePeer { constructor() { super(); peer = this; } },
      sign: async ({ nonce, resume }) => {
        env.signed.push(resume ? 'rs' : 'dk');
        return resume ? { k: 'rs', nonce, id: resume.id, mac: 'm' } : { k: 'dk', nonce, cred: 'c', sig: 's' };
      },
      verify: async () => env.verdict,
      chosenDisplay,
      view: async () => ({ w: 1, h: 1, maxW: 1, maxH: 1, fps: 2 }),
    },
  });
  const events = { states: [], ended: null, pinned: 0 };
  c.on('state', (s) => events.states.push(s)).on('ended', (e) => { events.ended = e; }).on('pinned', () => { events.pinned += 1; });
  Object.assign(env, { c, events });
  Object.defineProperty(env, 'peer', { get: () => peer });
  // Messages of `type` the connection sent to the server.
  env.sent = (type) => socket.sent.filter((m) => m.type === type);
  env.signal = (m) => c.onSignal({ session: c.id, ...m });
  return env;
}

// Up to the offer: challenge, nonce, offer (epoch 0).
async function offered(opts) {
  const env = setup(opts);
  env.c.start();
  await settle();
  env.signal({ type: 'nonce', nonce: 'N' });
  await settle();
  return env;
}

// Answered and the control channel open: active.
async function active(opts) {
  const env = await offered(opts);
  env.signal({ type: 'answer', epoch: 0, sdp: 'answer0', auth: {} });
  await settle();
  env.c.channels.input.open();
  return env;
}

function trickle(env, candidate) {
  env.signal({ type: 'candidate', candidate: { candidate, sdpMLineIndex: 0 } });
}

test('the first desktop offer uses the remembered display when the host can start on it', async (t) => {
  const saved = new Map();
  t.mock.method(store, 'set', (key, value) => saved.set(key, value));
  t.mock.method(store, 'get', (key) => saved.get(key) ?? null);
  const list = [
    { id: 'primary', x: 0, y: 0, w: 640, h: 400 },
    { id: 'secondary', x: 640, y: 0, w: 640, h: 400 },
  ];
  const cases = [
    { id: 'known', startDisplay: true, wanted: 'secondary', display: 'secondary' },
    { id: 'missing', startDisplay: true, wanted: 'gone' },
    { id: 'stale', startDisplay: true, wanted: 'secondary', deviceDisplays: [list[0]], switchTo: 'secondary' },
    { id: 'none', startDisplay: true },
    { id: 'legacy', startDisplay: false, wanted: 'secondary', switchTo: 'secondary' },
    { id: 'terminal', startDisplay: true, wanted: 'secondary', dataOnly: true },
  ];
  for (const { id, startDisplay, wanted, display, switchTo, dataOnly, deviceDisplays = list } of cases) {
    const caps = startDisplay ? [CAPS.DISPLAYS, CAPS.START_DISPLAY] : [CAPS.DISPLAYS];
    const device = { id, caps, displays: deviceDisplays };
    // The detail page remembers the clicked display before opening the session.
    if (wanted) rememberDisplay(id, wanted);
    const env = await active({ device, dataOnly });
    const [offer] = env.sent(Signal.OFFER);
    assert.equal(Object.hasOwn(offer, 'display'), display !== undefined, id);
    assert.equal(offer.display, display, id);
    if (dataOnly) continue;
    const input = env.peer.channels.input;
    const session = {
      device,
      connection: env.c,
      connected: true,
      hasCap: (cap) => caps.includes(cap),
      send: (message) => input.send(JSON.stringify(message)),
      toolbar: { refreshPanel() {} },
      pointer: {},
      video: { setZoom() {}, zoomEnded() {} },
    };
    const displays = new Displays(session);
    displays.restore();
    displays.onDisplays({ list, cur: 'primary' });
    displays.restore();
    assert.deepEqual(input.sent, switchTo ? [{ t: Control.DISPLAY, v: switchTo }] : [], id);
  }
});

test('the data-only offer negotiates control and terminal channels and keeps them on ICE restart', async () => {
  const env = await offered({ dataOnly: true });
  const channels = env.c.channels;
  assert.deepEqual(Object.keys(env.peer.channels), ['control', 'terminal']);
  assert.equal(channels.input.label, 'control');
  channels.input.open();
  assert.equal(env.c.state, 'active');
  env.peer.set('disconnected');
  await settle();
  assert.equal(env.sent('restart').length, 1);
  assert.equal(env.c.channels, channels);
  assert.equal(env.c.channels.terminal, env.peer.channels.terminal);
});

test('the offer is epoch 0 over the nonce, its candidates follow it', async () => {
  const env = setup();
  env.c.start();
  await settle();
  assert.deepEqual(env.sent('challenge').map((m) => m.session), [env.c.id]);
  env.peer.candidate('early');
  assert.equal(env.sent('candidate').length, 0, 'no candidate before the offer');
  env.signal({ type: 'nonce', nonce: 'N' });
  await settle();
  const [offer] = env.sent('offer');
  assert.equal(offer.epoch, 0);
  assert.equal(offer.auth.nonce, 'N');
  assert.deepEqual(env.sent('candidate').map((m) => m.candidate.candidate), ['early']);
  env.peer.candidate('late');
  assert.equal(env.sent('candidate').length, 2);
});

test('an answer for another epoch is ignored; the current one is verified once and applied', async () => {
  const env = await offered({ verdict: 'first' });
  env.signal({ type: 'answer', epoch: 3, sdp: 'stale', auth: {} });
  await settle();
  assert.deepEqual(env.peer.remote, []);
  env.signal({ type: 'answer', epoch: 0, sdp: 'answer0', auth: {} });
  env.signal({ type: 'answer', epoch: 0, sdp: 'again', auth: {} });
  await settle();
  assert.deepEqual(env.peer.remote, ['answer0']);
  assert.equal(env.events.pinned, 1, 'the first answer from this computer keeps its key');
});

test('early candidates are added in order once the answer is applied, and later candidates are added immediately', async () => {
  const env = await offered();
  const application = Promise.withResolvers();
  env.peer.applyingAnswer = application.promise;
  trickle(env, 'first');
  env.signal({ type: 'answer', epoch: 0, sdp: 'answer0' });
  await settle();
  trickle(env, 'second');
  assert.deepEqual(env.peer.remoteCandidates, []);
  application.resolve();
  await settle();
  assert.deepEqual(env.peer.remoteCandidates, ['first', 'second']);
  trickle(env, 'late');
  assert.deepEqual(env.peer.remoteCandidates, ['first', 'second', 'late']);
});

test('a bad signature on the current epoch ends the session as forged, told to the computer as auth', async () => {
  const env = await offered({ verdict: 'bad' });
  env.signal({ type: 'answer', epoch: 0, sdp: 'forged', auth: {} });
  await settle();
  assert.equal(env.events.ended.reason, 'forged');
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['auth'], 'the computer is told through the server');
  assert.ok(env.peer.closed);
});

test('A10 connect deadline: no control channel within 30 s ends failed, told through the server', async () => {
  const env = await offered();
  env.timers.advance(DEADLINE_MS - 1);
  assert.equal(env.c.state, 'connecting');
  env.timers.advance(1);
  assert.equal(env.events.ended.reason, 'failed');
  assert.equal(env.events.ended.established, false);
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['failed']);
});

test('the channel opening is active, and the connect deadline is gone', async () => {
  const env = await active();
  assert.equal(env.c.state, 'active');
  env.timers.advance(DEADLINE_MS * 2);
  assert.equal(env.events.ended, null);
});

test('B3 recovering: a new restart replaces the outstanding restart and discards its pending candidates', async () => {
  const env = await active();
  env.peer.set('disconnected');
  await settle();
  assert.equal(env.c.state, 'recovering');
  assert.equal(env.events.states.at(-1).cause, 'network');
  trickle(env, 'superseded');
  env.c.restart();
  await settle();
  assert.deepEqual(env.sent('restart').map((m) => m.epoch), [1, 2]);
  env.signal({ type: 'answer', epoch: 1, sdp: 'answer1', auth: {} });
  await settle();
  assert.deepEqual(env.peer.remote, ['answer0'], 'the superseded epoch is not this negotiation');
  trickle(env, 'current');
  assert.deepEqual(env.peer.remoteCandidates, []);
  env.signal({ type: 'answer', epoch: 2, sdp: 'answer2', auth: {} });
  await settle();
  assert.deepEqual(env.peer.remote, ['answer0', 'answer2']);
  assert.deepEqual(env.peer.remoteCandidates, ['current']);
  env.peer.set('connected');
  assert.equal(env.c.state, 'active');
  env.timers.advance(DEADLINE_MS);
  assert.equal(env.events.ended, null, 'recovered: no deadline');
});

test('B5 recover deadline: 30 s recovering ends failed, on the channel and through the server', async () => {
  const env = await active();
  env.peer.set('disconnected');
  await settle();
  env.timers.advance(DEADLINE_MS);
  assert.equal(env.events.ended.reason, 'failed');
  assert.equal(env.events.ended.established, true);
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['failed']);
  assert.deepEqual(env.peer.channels.input.sent.map((m) => m.reason), ['failed']);
});

test('B20 the signaling socket back: attach once the server answered for the session, else the challenge again', async () => {
  const env = await offered();
  env.c.onSocketUp();
  assert.deepEqual(env.sent('attach').map((m) => m.session), [env.c.id]);
  assert.equal(env.sent('challenge').length, 1, 'answered for: no challenge again');
  // No nonce yet: what went out on the old socket may still arrive, so no attach; the
  // challenge again (the computer gives the same nonce), and the offer follows its nonce.
  const early = setup();
  early.c.start();
  await settle();
  early.c.onSocketUp();
  assert.equal(early.sent('attach').length, 0);
  assert.equal(early.sent('challenge').length, 2);
  early.signal({ type: 'nonce', nonce: 'N' });
  await settle();
  assert.deepEqual(early.sent('offer').map((m) => m.auth.nonce), ['N']);
  early.c.onSocketUp();
  assert.equal(early.sent('attach').length, 1, 'answered for now: attached');
});

test('B7 attached or presence: away waits and says why, online restarts a recovering session', async () => {
  const env = await active();
  env.peer.set('disconnected');
  await settle();
  const restarts = () => env.sent('restart').length;
  assert.equal(restarts(), 1);
  env.c.onSocketDown();
  assert.equal(env.events.states.at(-1).cause, 'server');
  env.c.onSocketUp();
  env.signal({ type: 'attached', host: 'away' });
  await settle();
  assert.equal(env.events.states.at(-1).cause, 'host');
  assert.equal(restarts(), 1, 'nothing is sent to a computer that is away');
  env.signal({ type: 'presence', host: 'online' });
  await settle();
  assert.equal(restarts(), 2, 'back: restarted again, what was sent meanwhile was lost');
  assert.equal(env.events.states.at(-1).cause, 'network');
  env.signal({ type: 'attached', host: 'online' });
  await settle();
  assert.equal(restarts(), 3);
});

test('A11 connecting: the computer back asks again for what was not answered', async () => {
  const env = await offered();
  env.peer.candidate('c1');
  env.signal({ type: 'presence', host: 'away' });
  env.signal({ type: 'presence', host: 'online' });
  assert.equal(env.sent('offer').length, 2, 'the same offer again');
  assert.deepEqual(env.sent('offer').map((m) => m.epoch), [0, 0]);
  assert.equal(env.sent('candidate').length, 2, 'its candidates follow it again');
  // Before the nonce: the challenge goes again.
  const early = setup();
  early.c.start();
  await settle();
  early.signal({ type: 'presence', host: 'online' });
  assert.equal(early.sent('challenge').length, 2);
});

test('A12 bye gone (an attach the server does not know) ends the session at once', async () => {
  const env = await offered();
  env.c.onSocketUp();
  env.signal({ type: 'bye', reason: 'gone' });
  assert.equal(env.events.ended.reason, 'gone');
  assert.equal(env.events.ended.via, 'bye');
  assert.equal(env.sent('bye').length, 0, 'nothing goes back');
});

test('B10 bye ended through the server ends an active session', async () => {
  const env = await active();
  env.signal({ type: 'bye', reason: 'ended' });
  assert.deepEqual([env.events.ended.reason, env.events.ended.via, env.events.ended.established], ['ended', 'bye', true]);
});

test('B1 A13 ending here: through the server always, and on the channel while it is open', async () => {
  const live = await active();
  live.c.end('left');
  assert.deepEqual(live.peer.channels.input.sent.map((m) => m.t + ':' + m.reason), ['end:left']);
  assert.deepEqual(live.sent('bye').map((m) => m.reason), ['left']);
  assert.equal(live.events.ended.reason, 'left');
  const early = await offered();
  early.c.end('left');
  assert.deepEqual(early.sent('bye').map((m) => m.reason), ['left']);
  early.c.end('left');
  assert.equal(early.sent('bye').length, 1, 'once');
});

test('B13 D1 the computer ends it on the channel: taken, by whom, the same device or not', async () => {
  const env = await active();
  env.peer.channels.input.message({ t: 'end', reason: 'taken', by: 'Phone', same: true });
  assert.deepEqual([env.events.ended.reason, env.events.ended.by, env.events.ended.same, env.events.ended.via], ['taken', 'Phone', true, 'end']);
  assert.equal(env.peer.channels.input.sent.length, 0, 'nothing goes back');
});

test('resume tokens: used by the next session of this page, dropped on taken; an unknown one signs again with the device key', async () => {
  const first = await active();
  first.peer.channels.input.message({ t: 'resume', id: 'R', k: 'AAAA' });
  assert.ok(first.c.resumable);
  first.c.end('left');
  const second = await offered();
  assert.deepEqual(second.signed, ['rs']);
  // The computer restarted: it knows the token no more.
  second.signal({ type: 'bye', reason: 'auth' });
  assert.equal(second.events.ended, null);
  assert.equal(second.sent('challenge').length, 2, 'the same session asks again');
  second.signal({ type: 'nonce', nonce: 'N2' });
  await settle();
  assert.deepEqual(second.signed, ['rs', 'dk']);
  assert.ok(!second.c.resumable);
  assert.equal(second.sent('offer').at(-1).session, second.c.id);
  // Taken over: the token goes.
  second.signal({ type: 'answer', epoch: 0, sdp: 'a', auth: {} });
  await settle();
  second.peer.channels.input.open();
  second.peer.channels.input.message({ t: 'resume', id: 'R2', k: 'AAAA' });
  second.peer.channels.input.message({ t: 'end', reason: 'taken', by: 'X' });
  assert.ok(!second.c.resumable);
});

test('B6 failed recovers: restarted, again once answered, and only the deadline ends it, told to the computer', async () => {
  const env = await active();
  env.peer.set('failed');
  await settle();
  assert.equal(env.c.state, 'recovering');
  assert.equal(env.events.ended, null);
  assert.deepEqual(env.sent('restart').map((m) => m.epoch), [1]);
  env.peer.set('failed');
  await settle();
  assert.equal(env.sent('restart').length, 1, 'the restart still outstanding is not replaced');
  env.signal({ type: 'answer', epoch: 1, sdp: 'answer1', auth: {} });
  await settle();
  env.peer.set('failed');
  await settle();
  assert.deepEqual(env.sent('restart').map((m) => m.epoch), [1, 2], 'answered and failed again: restarted again');
  env.timers.advance(DEADLINE_MS - 1);
  assert.equal(env.events.ended, null, 'the deadline counts from entering recovering');
  env.timers.advance(1);
  assert.equal(env.events.ended.reason, 'failed');
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['failed']);
});

test('a failed peer connection never established ends the session, told through the server', async () => {
  const env = await offered();
  env.peer.set('failed');
  assert.equal(env.events.ended.reason, 'failed');
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['failed']);
});

test('the control channel closing ends the session, and the computer is told', async () => {
  const env = await active();
  env.peer.channels.input.readyState = 'closed';
  env.peer.channels.input.dispatchEvent(new Event('close'));
  assert.deepEqual([env.events.ended.reason, env.events.ended.via], ['failed', 'local']);
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['failed']);
});

test('B19 an end while the socket renews goes through the server (queued for the new socket)', async () => {
  const env = await active();
  env.socket.open = false;
  env.c.end('left');
  assert.deepEqual(env.sent('bye').map((m) => m.reason), ['left']);
  env.c.onSocketUp();
  assert.equal(env.sent('attach').length, 0, 'an ended session attaches nothing');
});
