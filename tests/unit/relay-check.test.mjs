import { test } from 'node:test';
import assert from 'node:assert/strict';
import { probeRelay } from '../../web/static/js/relay-check.js';

const turn = { urls: 'turn:relay.example:3478?transport=tcp', username: 'user', credential: 'password' };
const stun = 'stun:relay.example:3478';
const offer = { type: 'offer', sdp: 'offer' };
const answer = { type: 'answer', sdp: 'answer' };

function setup(t, createOffer = async () => offer) {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const peers = [];
  class Peer {
    constructor(config) {
      this.config = config;
      this.received = [];
      this.closes = 0;
      peers.push(this);
    }
    createDataChannel() { return this.channel = new EventTarget(); }
    createOffer() { return createOffer(); }
    async createAnswer() { return answer; }
    async setLocalDescription(description) {
      const other = peers.find((peer) => peer !== this);
      assert.equal(other.remoteDescription, description);
    }
    async setRemoteDescription(description) { this.remoteDescription = description; }
    async addIceCandidate(candidate) { this.received.push(candidate); }
    close() { this.closes += 1; }
    candidate(type) {
      const candidate = { type };
      this.onicecandidate({ candidate });
      return candidate;
    }
  }
  return { peers, result: probeRelay(turn, stun, Peer) };
}

for (const { name, relay, open, expected } of [
  { name: 'the data channel opens with a host candidate', relay: true, open: true, expected: 'connected' },
  { name: 'the data channel never opens', relay: true, expected: 'allocated' },
  { name: 'TURN does not supply a relay candidate', relay: false, expected: 'unreachable' },
]) {
  test(`relay check when ${name}`, async (t) => {
    const { peers, result } = setup(t);
    const [a, b] = peers;
    assert.deepEqual(a.config, { iceTransportPolicy: 'relay', iceServers: [turn] });
    assert.deepEqual(b.config, { iceServers: [{ urls: stun }] });
    await new Promise(setImmediate);

    const relayed = relay ? [a.candidate('relay')] : [];
    a.onicecandidate({ candidate: null });
    assert.deepEqual(b.received, relayed);
    assert.deepEqual(a.received, [b.candidate('host')]);

    if (open) {
      a.channel.dispatchEvent(new Event('open'));
    } else {
      let settled = false;
      result.then(() => { settled = true; });
      t.mock.timers.tick(4999);
      await new Promise(setImmediate);
      assert.equal(settled, false);
      t.mock.timers.tick(1);
    }
    assert.equal(await result, expected);
    assert.deepEqual(peers.map((peer) => peer.closes), [1, 1]);
  });
}

test('relay check times out even if negotiation stalls', async (t) => {
  const { peers, result } = setup(t, () => new Promise(() => {}));
  t.mock.timers.tick(5000);
  assert.equal(await result, 'unreachable');
  assert.deepEqual(peers.map((peer) => peer.closes), [1, 1]);
});

test('relay check closes both peers if negotiation fails', async (t) => {
  const { peers, result } = setup(t, async () => { throw new Error('offer failed'); });
  assert.equal(await result, 'unreachable');
  assert.deepEqual(peers.map((peer) => peer.closes), [1, 1]);
});
