// Module test: web/static/js/session/constraints.js with a stubbed MediaCapabilities.
// Run: node --test tests/unit/constraints.test.mjs (no dependencies).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FPS, SHARP_FPS, LADDER, rungConfig, decodeLimit, viewport, viewMessage, watchPixelRatio } from '../../web/static/js/session/constraints.js';
import { validate } from '../../web/static/js/proto.js';


// A MediaCapabilities that decodes smoothly up to `maxPixels`, supports up to `supportPixels`,
// and records what it was asked.
function stub({ maxPixels = Infinity, supportPixels = Infinity, fail = false } = {}) {
  const asked = [];
  return {
    asked,
    async decodingInfo(config) {
      asked.push(config);
      if (fail) throw new TypeError('unsupported configuration');
      const px = config.video.width * config.video.height;
      return { supported: px <= supportPixels, smooth: px <= maxPixels, powerEfficient: true };
    },
  };
}

test('each rung asks about H.264 High over WebRTC at its own level and at the most frames a second the host sends', async () => {
  const mc = stub();
  await decodeLimit(mc);
  assert.equal(mc.asked.length, LADDER.length);
  const [first] = mc.asked;
  assert.equal(first.type, 'webrtc');
  assert.equal(first.video.contentType, 'video/H264;profile-level-id=64001f;packetization-mode=1');
  assert.deepEqual([first.video.width, first.video.height, first.video.framerate], [1280, 720, FPS]);
  assert.equal(rungConfig([3840, 2160, 0x33]).video.contentType, 'video/H264;profile-level-id=640033;packetization-mode=1');
  assert.equal(rungConfig([1920, 1080, 0x28]).video.bitrate, Math.round(1920 * 1080 * FPS * 0.07));
});

test('the largest smooth rung is the limit', async () => {
  assert.deepEqual(await decodeLimit(stub({ maxPixels: 3840 * 2160 })), { w: 3840, h: 2160, fps: FPS });
  assert.deepEqual(await decodeLimit(stub({ maxPixels: 1920 * 1080 })), { w: 1920, h: 1080, fps: FPS });
  assert.deepEqual(await decodeLimit(stub()), { w: 7680, h: 4320, fps: FPS });
});

test('supported but not smooth, or not supported, ends the ladder', async () => {
  const mc = stub({ supportPixels: 2560 * 1440 });
  assert.deepEqual(await decodeLimit(mc), { w: 2560, h: 1440, fps: FPS });
  assert.equal(mc.asked.length, 4, 'stops at the first refusal');
  assert.equal(await decodeLimit(stub({ maxPixels: 100 })), null, 'not even the smallest rung');
  assert.equal(await decodeLimit(stub({ fail: true })), null, 'the API throws');
});

test('without MediaCapabilities the viewport stands in', async () => {
  assert.equal(await decodeLimit(undefined), null);
  assert.equal(await decodeLimit({}), null);
  const m = viewMessage({ w: 1170, h: 2532 }, null);
  assert.deepEqual(m, { t: 'view', w: 1170, h: 2532, maxW: 1170, maxH: 2532, fps: FPS });
});

test('the viewport is the stage in device pixels, times the zoom', () => {
  assert.deepEqual(viewport({ width: 390, height: 844 }, 3, 1), { w: 1170, h: 2532 });
  assert.deepEqual(viewport({ width: 1440, height: 900 }, 1, 2), { w: 2880, h: 1800 });
  assert.equal(viewport({ width: 0, height: 0 }, 2, 1), null, 'a stage with no area has no viewport');
  assert.equal(viewport({ width: 390, height: 0.2 }, 1, 1), null, 'nor one that rounds to nothing');
  assert.deepEqual(viewport({ width: 100.4, height: 50.6 }, undefined, undefined), { w: 100, h: 51 });
});

test('a device pixel ratio change is reported, and the watch is armed again for the new ratio', () => {
  // A window whose media queries match only the current ratio and fire once when it changes.
  const queries = [];
  const win = {
    devicePixelRatio: 1,
    matchMedia(q) {
      const listeners = [];
      const mq = { q, listeners, addEventListener: (type, fn, opts) => listeners.push({ type, fn, opts }), removeEventListener: (type, fn) => { const i = listeners.findIndex((l) => l.fn === fn); if (i >= 0) listeners.splice(i, 1); } };
      queries.push(mq);
      return mq;
    },
  };
  const move = (dpr) => {
    const live = queries.filter((mq) => mq.listeners.length);
    win.devicePixelRatio = dpr;
    for (const mq of live) for (const l of mq.listeners.splice(0)) l.fn();
  };
  let calls = 0;
  const stop = watchPixelRatio(win, () => calls++);
  assert.equal(queries[0].q, '(resolution: 1dppx)');
  assert.deepEqual(queries[0].listeners.map((l) => [l.type, l.opts.once]), [['change', true]]);
  move(2);
  assert.equal(calls, 1);
  assert.equal(queries.at(-1).q, '(resolution: 2dppx)', 're-armed for the new ratio');
  move(1.5);
  assert.equal(calls, 2);
  stop();
  assert.equal(queries.filter((mq) => mq.listeners.length).length, 0, 'stopping removes the listener');
  move(3);
  assert.equal(calls, 2);
});

test('the message follows the protocol', async () => {
  const m = viewMessage(viewport({ width: 390, height: 844 }, 3, 1), await decodeLimit(stub({ maxPixels: 3840 * 2160 })));
  assert.deepEqual(m, { t: 'view', w: 1170, h: 2532, maxW: 3840, maxH: 2160, fps: FPS });
  assert.equal(validate('control', m, { strict: true }), null);
});

test('the host sends at most 24 frames a second; a sharper picture asks for 2, never more than decodes smoothly', () => {
  assert.equal(FPS, 24);
  const vp = { w: 3840, h: 2160 };
  assert.equal(viewMessage(vp, { w: 3840, h: 2160, fps: FPS }).fps, FPS);
  assert.equal(viewMessage(vp, { w: 3840, h: 2160, fps: FPS }, true).fps, SHARP_FPS);
  assert.equal(viewMessage(vp, null, true).fps, SHARP_FPS);
  assert.equal(viewMessage(vp, { w: 3840, h: 2160, fps: 1 }, true).fps, 1);
});
