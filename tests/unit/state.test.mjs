import assert from 'node:assert/strict';
import { test } from 'node:test';

globalThis.document = { getElementById: () => null, addEventListener() {} };
globalThis.matchMedia = () => ({ matches: false });
const { currentSession, endCurrentSession, setCurrentSession, state } = await import('../../web/static/js/state.js');

const ours = { device: true, name: 'This browser', connecting: false };

test('ending a session preserves the computer status reported by the server', () => {
  const d = { id: 'computer', controller: ours };
  state.devices = [d];
  const s = { device: { id: d.id }, closed: true };
  setCurrentSession(s);
  assert.equal(endCurrentSession(s), true);
  assert.equal(currentSession(), null);
  assert.equal(d.controller, ours);
});

test('a late end leaves the new session and its controller report in place', () => {
  const d = { id: 'computer', controller: ours };
  state.devices = [d];
  const next = { device: d };
  setCurrentSession(next);
  assert.equal(endCurrentSession({ device: d }), false);
  assert.equal(currentSession(), next);
  assert.equal(d.controller, ours);
});
