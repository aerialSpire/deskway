import assert from 'node:assert/strict';
import { test } from 'node:test';
import { terminalKey, terminalNativeKey } from '../../web/static/js/session/terminal-keys.js';

const cases = {
  'physical key values keep the keyboard layout and case': [
    ['lowercase', { code: 'KeyA', key: 'a' }, 'a'],
    ['capital with Shift', { code: 'KeyA', key: 'A', shift: true }, 'A'],
    ['capital from Caps Lock', { code: 'KeyA', key: 'A' }, 'A'],
    ['Shift with Caps Lock', { code: 'KeyA', key: 'a', shift: true, caps: true }, 'a'],
    ['character from another layout', { code: 'KeyQ', key: 'Ä', shift: true }, 'Ä'],
    ['Space', { code: 'Space', key: ' ' }, ' '],
    ['digit', { code: 'Digit2', key: '2' }, '2'],
    ['shifted digit', { code: 'Digit2', key: '@', shift: true }, '@'],
    ['Ctrl+C', { code: 'KeyC', key: 'c', ctrl: true }, '\x03'],
    ['Alt+B', { code: 'KeyB', key: 'b', alt: true }, '\x1bb'],
  ],
  'on-screen key positions use Shift and Caps Lock': [
    ['lowercase', { code: 'KeyA' }, 'a'],
    ['capital', { code: 'KeyA', shift: true }, 'A'],
    ['Caps Lock', { code: 'KeyA', caps: true }, 'A'],
    ['Shift with Caps Lock', { code: 'KeyA', shift: true, caps: true }, 'a'],
    ['Space', { code: 'Space' }, ' '],
    ['digit', { code: 'Digit1' }, '1'],
    ['shifted digit', { code: 'Digit1', shift: true }, '!'],
    ['shifted punctuation', { code: 'Comma', shift: true }, '<'],
    ['Ctrl+C', { code: 'KeyC', ctrl: true }, '\x03'],
    ['Alt+Shift+B', { code: 'KeyB', alt: true, shift: true }, '\x1bB'],
    ['explicit Alt+B', { code: 'KeyB', alt: true }, '\x1bb'],
  ],
  'committed input with a modifier keeps the supplied character': [
    ['uppercase', { key: 'A', shift: true }, 'A'],
    ['lowercase', { key: 'a', shift: true }, 'a'],
    ['Space', { key: ' ', shift: true }, ' '],
    ['Unicode', { key: '字', shift: true }, '字'],
    ['Ctrl+C', { key: 'C', ctrl: true, shift: true }, '\x03'],
    ['Alt+B', { key: 'B', alt: true }, '\x1bB'],
  ],
  'terminal control keys use xterm sequences': [
    ['Enter', { code: 'Enter' }, '\r'],
    ['keypad Enter', { code: 'NumpadEnter', key: 'Enter' }, '\r'],
    ['Tab', { code: 'Tab', key: 'Tab' }, '\t'],
    ['Shift+Tab', { code: 'Tab', key: 'Tab', shift: true }, '\x1b[Z'],
    ['Backspace', { code: 'Backspace', key: 'Backspace' }, '\x7f'],
    ['Escape', { code: 'Escape' }, '\x1b'],
    ['Up', { code: 'ArrowUp', key: 'ArrowUp' }, '\x1b[A'],
    ['Down', { code: 'ArrowDown' }, '\x1b[B'],
    ['Right', { code: 'ArrowRight' }, '\x1b[C'],
    ['Left', { code: 'ArrowLeft' }, '\x1b[D'],
    ['Up in application mode', { code: 'ArrowUp', applicationCursor: true }, '\x1bOA'],
    ['Ctrl+Left', { code: 'ArrowLeft', ctrl: true }, '\x1b[1;5D'],
    ['Delete', { code: 'Delete' }, '\x1b[3~'],
    ['F1', { code: 'F1' }, '\x1bOP'],
    ['F12', { code: 'F12' }, '\x1b[24~'],
  ],
};

test('the terminal leaves paste and Mac Option text to the browser', () => {
  for (const [label, mac, event, expected] of [
    ['Ctrl+V', false, { code: 'KeyV', key: 'v', ctrlKey: true }, true],
    ['Ctrl+Shift+V', false, { code: 'KeyV', key: 'V', ctrlKey: true, shiftKey: true }, true],
    ['Shift+Insert', false, { code: 'Insert', shiftKey: true }, true],
    ['Cmd+V', true, { code: 'KeyV', key: 'v', metaKey: true }, true],
    ['paste with another keyboard layout', true, { code: 'KeyX', key: 'v', metaKey: true }, true],
    ['Mac Ctrl+V is a terminal shortcut', true, { code: 'KeyV', key: 'v', ctrlKey: true }, false],
    ['Mac Option produces @', true, { code: 'KeyL', key: '@', altKey: true }, true],
    ['Mac Option with Shift produces a character', true, { code: 'Digit2', key: '€', altKey: true, shiftKey: true }, true],
    ['Mac Option starts a dead key', true, { code: 'KeyE', key: 'Dead', altKey: true }, true],
    ['Mac Ctrl+Option is a terminal shortcut', true, { code: 'KeyB', key: 'b', ctrlKey: true, altKey: true }, false],
    ['Mac Option+Left is a terminal shortcut', true, { code: 'ArrowLeft', key: 'ArrowLeft', altKey: true }, false],
    ['non-Mac Alt+B is a terminal shortcut', false, { code: 'KeyB', key: 'b', altKey: true }, false],
  ]) assert.equal(!!terminalNativeKey(event, mac), expected, label);
});

for (const [name, table] of Object.entries(cases)) {
  test(name, () => {
    for (const [label, key, expected] of table) assert.equal(terminalKey(key), expected, label);
  });
}
