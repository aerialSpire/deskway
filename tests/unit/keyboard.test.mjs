import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CAPS } from '../../web/static/js/proto.js';

class Element extends EventTarget {
  classList = { add() {}, remove() {} };
  style = {};
  append() {}
  setAttribute() {}
  setSelectionRange() {}
  focus() {}
  blur() {}
}

globalThis.document = { getElementById: () => null, createElement: () => new Element(), addEventListener() {} };
globalThis.matchMedia = () => ({ matches: false });

const { Desktop } = await import('../../web/static/js/session/desktop.js');
const { Keyboard } = await import('../../web/static/js/session/keyboard.js');
const { OnscreenKeyboard } = await import('../../web/static/js/session/onscreen-keyboard.js');
const { Session } = await import('../../web/static/js/session/session.js');

function dispatch(target, type, fields) {
  target.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), fields));
}

function desktop(caps) {
  const sent = [];
  const s = Object.assign(Object.create(Session.prototype), {
    device: { os: 'windows' },
    stage: new Element(),
    toolbar: { isFull: true },
    caps: new Set([CAPS.INPUT, ...caps]),
    connection: { state: 'active' },
    inputChannel: { readyState: 'open', send: (wire) => sent.push(wire) },
    video: {},
  });
  s.content = Object.assign(Object.create(Desktop.prototype), { s });
  s.osk = new OnscreenKeyboard(s);
  s.osk.fingers = new Map();
  s.keyboard = new Keyboard(s, s.content.keyboardSink());
  s.keyboard.bind();

  const physical = (type, code, key, held = [], repeat = false) => dispatch(s.stage, type, {
    code, key, repeat, getModifierState: (name) => held.includes(name),
  });
  const down = (code) => {
    s.keyboard.noteEvent({ type: 'pointerdown' });
    s.osk.keyDown(1, Object.assign(new Element(), { dataset: { code } }));
  };
  const up = () => s.osk.keyUp(1);
  const tap = (code) => { down(code); up(); };
  const input = (inputType, text = '') => {
    s.keyboard.box.value = text;
    dispatch(s.keyboard.box, 'input', { inputType });
  };
  return { sent, physical, down, up, tap, input, osk: s.osk, box: s.keyboard.box };
}

const plainSync = '{"t":"sync","locks":{"caps":false,"num":false,"scroll":false},"mods":{"shift":false,"ctrl":false,"alt":false,"meta":false}}';

// Literal desktop control messages from main's keyboard, onscreen-keyboard and text-box paths.
const cases = [
  ['a letter', [], ({ physical }) => {
    physical('keydown', 'KeyA', 'a');
    physical('keyup', 'KeyA', 'a');
  }, [
    plainSync,
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
  ]],
  ['Shift+letter', [], ({ physical }) => {
    physical('keydown', 'ShiftLeft', 'Shift', ['Shift']);
    physical('keydown', 'KeyA', 'A', ['Shift']);
    physical('keyup', 'KeyA', 'A', ['Shift']);
    physical('keyup', 'ShiftLeft', 'Shift');
  }, [
    '{"t":"sync","locks":{"caps":false,"num":false,"scroll":false},"mods":{"shift":true,"ctrl":false,"alt":false,"meta":false}}',
    '{"t":"k","c":"ShiftLeft","d":true}',
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
    '{"t":"k","c":"ShiftLeft","d":false}',
  ]],
  ['Ctrl+C', [], ({ physical }) => {
    physical('keydown', 'ControlLeft', 'Control', ['Control']);
    physical('keydown', 'KeyC', 'c', ['Control']);
    physical('keyup', 'KeyC', 'c', ['Control']);
    physical('keyup', 'ControlLeft', 'Control');
  }, [
    '{"t":"sync","locks":{"caps":false,"num":false,"scroll":false},"mods":{"shift":false,"ctrl":true,"alt":false,"meta":false}}',
    '{"t":"k","c":"ControlLeft","d":true}',
    '{"t":"k","c":"KeyC","d":true}',
    '{"t":"k","c":"KeyC","d":false}',
    '{"t":"k","c":"ControlLeft","d":false}',
  ]],
  ['a physical key repeating', [], ({ physical }) => {
    physical('keydown', 'KeyA', 'a');
    physical('keydown', 'KeyA', 'a', [], true);
    physical('keyup', 'KeyA', 'a');
  }, [
    plainSync,
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
  ]],
  ['an on-screen key repeating', [], ({ down, up }, t) => {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    down('KeyA');
    t.mock.timers.tick(500);
    up();
  }, [
    plainSync,
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
  ]],
  ['a tapped on-screen modifier then a key', [], ({ tap }) => {
    tap('ShiftLeft');
    tap('KeyA');
  }, [
    plainSync,
    '{"t":"k","c":"ShiftLeft","d":true}',
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
    '{"t":"k","c":"ShiftLeft","d":false}',
  ]],
  ['a locked on-screen modifier then two keys', [], ({ tap }, t) => {
    t.mock.method(performance, 'now', () => 0);
    tap('ShiftLeft');
    tap('ShiftLeft');
    tap('KeyA');
    tap('KeyB');
  }, [
    plainSync,
    '{"t":"k","c":"ShiftLeft","d":true}',
    '{"t":"k","c":"KeyA","d":true}',
    '{"t":"k","c":"KeyA","d":false}',
    '{"t":"k","c":"ShiftLeft","d":false}',
    '{"t":"k","c":"ShiftLeft","d":true}',
    '{"t":"k","c":"KeyB","d":true}',
    '{"t":"k","c":"KeyB","d":false}',
    '{"t":"k","c":"ShiftLeft","d":false}',
  ]],
  ['Enter and Backspace from the input-method box', [], ({ osk }) => {
    const typer = new Element();
    osk.bindTyperKeys(typer);
    dispatch(typer, 'keydown', { key: 'Enter', code: 'NumpadEnter' });
    dispatch(typer, 'keydown', { key: 'Backspace', code: 'Backspace' });
  }, [
    '{"t":"k","c":"Enter","d":true}',
    '{"t":"k","c":"Enter","d":false}',
    '{"t":"k","c":"Backspace","d":true}',
    '{"t":"k","c":"Backspace","d":false}',
  ]],
  ['text committed with a newline', [], ({ input }) => {
    input('insertText', 'Abé\nCD');
  }, [
    '{"t":"x","v":"Abé"}',
    '{"t":"k","c":"Enter","d":true}',
    '{"t":"k","c":"Enter","d":false}',
    '{"t":"x","v":"CD"}',
  ]],
  ['a latched modifier and committed character with chars', [CAPS.CHARS], ({ tap, input }) => {
    tap('ControlLeft');
    input('insertText', 'C');
  }, [
    plainSync,
    '{"t":"kc","ch":"C","mods":{"shift":false,"ctrl":true,"alt":false,"meta":false}}',
  ]],
  ['a latched modifier and committed character without chars', [], ({ tap, input }) => {
    tap('ControlLeft');
    input('insertText', 'C');
  }, [
    plainSync,
    '{"t":"x","v":"C"}',
  ]],
  ['a latched modifier and committed character when only terminal is advertised for text', [CAPS.TERMINAL], ({ tap, input }) => {
    tap('ControlLeft');
    input('insertText', 'C');
  }, [
    plainSync,
    '{"t":"x","v":"C"}',
  ]],
  ['a latched modifier and composition update', [CAPS.CHARS], ({ tap, box }) => {
    tap('ShiftLeft');
    dispatch(box, 'compositionstart');
    dispatch(box, 'compositionupdate', { data: 'Ca' });
    dispatch(box, 'compositionend', { data: 'Ca' });
  }, [
    plainSync,
    '{"t":"kc","ch":"c","mods":{"shift":true,"ctrl":false,"alt":false,"meta":false}}',
  ]],
];

for (const [name, caps, type, expected] of cases) {
  test('desktop input matches main for ' + name, (t) => {
    const s = desktop(caps);
    type(s, t);
    assert.deepEqual(s.sent, expected);
  });
}
