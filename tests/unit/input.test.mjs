// The browser's input messages (web/static/js/session/input-messages.js): each is valid against
// proto/messages.json, and the decisions behind them hold.
// Run: node --test tests/unit/input.test.mjs (no dependencies).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validate } from '../../web/static/js/proto.js';
import * as input from '../../web/static/js/session/input-messages.js';


// An event as browsers give it: getModifierState by key name.
const event = (...on) => ({ getModifierState: (k) => on.includes(k) });

test('sync carries the locks and modifiers of the event', () => {
  const m = input.syncMessage(event('CapsLock', 'Shift', 'Meta'));
  assert.equal(validate('control', m, { strict: true }), null);
  assert.deepEqual(m, { t: 'sync', locks: { caps: true, num: false, scroll: false }, mods: { shift: true, ctrl: false, alt: false, meta: true } });
});

test('sync adds the modifiers this page holds down itself', () => {
  const m = input.syncMessage(event('NumLock'), ['ControlRight', 'KeyA', 'AltLeft']);
  assert.deepEqual(m.locks, { caps: false, num: true, scroll: false });
  assert.deepEqual(m.mods, { shift: false, ctrl: true, alt: true, meta: false });
  assert.equal(validate('control', input.syncMessage(null), { strict: true }), null, 'no event: all off');
});

test('a character goes with the modifiers held, by state', () => {
  const m = input.charMessage('c', ['ControlLeft', 'ShiftRight']);
  assert.equal(validate('control', m, { strict: true }), null);
  assert.deepEqual(m, { t: 'kc', ch: 'c', mods: { shift: true, ctrl: true, alt: false, meta: false } });
});

test('Command going up lets go of the other keys, not the modifiers', () => {
  assert.deepEqual(input.releasedWithCommand(new Set(['ShiftLeft', 'KeyC', 'ArrowLeft', 'AltRight'])), ['KeyC', 'ArrowLeft']);
  assert.deepEqual(input.releasedWithCommand(new Set()), []);
});

test('pointer reports older than the latest move, or with a move on its way, are ignored', () => {
  assert.equal(input.reportIsStale(4, 5, false), true);
  assert.equal(input.reportIsStale(5, 5, false), false);
  assert.equal(input.reportIsStale(9, 5, false), false, 'the computer applied a later one');
  assert.equal(input.reportIsStale(5, 5, true), true, 'a move is about to go');
  assert.equal(input.reportIsStale(undefined, 0, false), true, 'a report without a sequence is not trusted');
});

test('moves are valid on the move channel', () => {
  assert.equal(validate('move', input.moveTo(0.5, 0.25, 3), { strict: true }), null);
  assert.equal(validate('move', input.moveBy(-2.5, 4, 4), { strict: true }), null);
  assert.notEqual(validate('move', input.moveBy(1, 1, 1.5)), null, 'sequences are integers');
});

test('labels from the layout: capitals alone on letters, both characters on the others', () => {
  const layout = { labels: { KeyQ: 'a', Digit2: 'é', BracketLeft: '^', KeyM: ',' }, shift: { KeyQ: 'A', Digit2: '2', BracketLeft: '¨', KeyM: '?' } };
  assert.deepEqual(input.layoutLabel(layout, 'KeyQ'), ['A', null]);
  assert.deepEqual(input.layoutLabel(layout, 'Digit2'), ['é', '2']);
  assert.deepEqual(input.layoutLabel(layout, 'KeyM'), [',', '?']);
  assert.equal(input.layoutLabel(layout, 'Enter'), null, 'not in the layout: the default label');
  assert.equal(input.layoutLabel(null, 'KeyQ'), null, 'no layout yet');
  assert.equal(validate('control', { t: 'layout', name: 'fr-FR', ...layout }, { strict: true }), null);
});
