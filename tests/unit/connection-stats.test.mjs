import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ConnectionHistory, ConnectionStats } from '../../web/static/js/session/connection-stats.js';

function report(second, { path = 'direct', rtt = .032, video = 'video', counters = {} } = {}) {
  return new Map([
    ['transport', { type: 'transport', selectedCandidatePairId: path }],
    [path, { id: path, type: 'candidate-pair', currentRoundTripTime: rtt, localCandidateId: 'local', remoteCandidateId: 'remote' }],
    ['local', { candidateType: 'host' }],
    ['remote', { candidateType: path === 'relay' ? 'relay' : 'srflx' }],
    [video, {
      id: video, type: 'inbound-rtp', kind: 'video', transportId: 'transport', timestamp: second * 1000,
      packetsReceived: 5000 + 99 * second, packetsLost: 100 + second,
      framesDecoded: 100 + 2 * second, bytesReceived: 50000 + 120000 * second,
      ...counters,
    }],
  ]);
}

test('the comparison uses two sampled windows, and video starts after the first picture', () => {
  const history = new ConnectionHistory();
  assert.deepEqual(history.update(report(0), 0), { route: 'direct', current: { rtt: 32 }, previous: {}, mode: null });
  const early = history.update(report(1), 1000, 'sharp');
  assert.deepEqual(early.current, { rtt: 32 });
  let sample;
  for (let i = 2; i <= 20; i++) {
    sample = history.update(report(i, { rtt: i <= 10 ? .032 : .084 }), i * 1000, 'sharp');
    if (i < 20) assert.deepEqual(sample.previous, {});
  }
  const picture = { loss: .01, fps: 2, bitrate: 960_000 };
  assert.deepEqual(sample.current, { rtt: 84, ...picture });
  assert.deepEqual(sample.previous, { rtt: 32 });
  sample = history.update(report(21, { rtt: .084 }), 21000, 'sharp');
  assert.deepEqual(sample.previous, { rtt: 32, ...picture });
});

test('a new path, stream or picture setting cannot inherit incompatible readings', () => {
  const history = new ConnectionHistory();
  for (let i = 0; i <= 20; i++) history.update(report(i), i * 1000, 'sharp');
  let sample = history.update(report(21), 21000, 'smooth');
  assert.deepEqual(sample.current, { rtt: 32 });
  assert.deepEqual(sample.previous, { rtt: 32 });
  sample = history.update(report(22, { path: 'relay', rtt: .084 }), 22000, 'smooth');
  assert.deepEqual(sample, { route: 'relay', current: { rtt: 84 }, previous: {}, mode: 'smooth' });
  assert.equal(history.update(report(23, { path: 'relay' }), 23000, 'smooth').current.fps, 2);
  assert.equal(history.update(report(24, { path: 'relay', video: 'new-stream' }), 24000, 'smooth').current.fps, undefined);
  assert.equal(history.update(report(35, { path: 'relay', video: 'new-stream' }), 35000, 'smooth').current.fps, undefined);
});

test('idle video keeps zero rates and leaves packet loss unspecified', () => {
  const history = new ConnectionHistory();
  const idle = { packetsReceived: 500, packetsLost: 50, framesDecoded: 100, bytesReceived: 4000 };
  history.update(report(0, { counters: idle }), 0, 'sharp');
  const sample = history.update(report(1, { counters: { ...idle, packetsLost: 49 } }), 1000, 'sharp');
  assert.deepEqual(sample.current, { rtt: 32, loss: null, fps: 0, bitrate: 0 });
});

test('a pending sample cannot publish over a restarted connection', async () => {
  const finish = [];
  const peer = { getStats: () => new Promise((resolve) => { finish.push(resolve); }) };
  const published = [];
  const stats = new ConnectionStats(() => null, (sample) => published.push(sample));
  try {
    stats.start(peer);
    stats.stop();
    stats.start(peer);
    finish[1](report(2, { path: 'relay' }));
    await Promise.resolve();
    finish[0](report(1));
    await Promise.resolve();
    assert.deepEqual(published.map((s) => s.route), ['relay']);
  } finally {
    stats.stop();
  }
});
