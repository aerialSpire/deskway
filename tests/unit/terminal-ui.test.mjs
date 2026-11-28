import assert from 'node:assert/strict';
import test from 'node:test';
import { keptTerminalNotice, terminalActions, terminalDisconnectActions, terminalTakenNotice } from '../../web/static/js/terminal-ui.js';

test('the computer page shows a kept-terminal notice only for online legacy hosts', () => {
  const d = { online: true, caps: ['terminal'], kept_terminal: { since: 123, name: 'Phone' } };
  const t = (key, args) => [key, args];
  const ago = (since) => `since ${since}`;
  for (const update of [{ online: false }, { caps: ['desk'] }, { caps: ['terminal', 'terminals'] }, { kept_terminal: undefined }]) {
    assert.equal(keptTerminalNotice({ ...d, ...update }, t, ago), null);
  }
  assert.deepEqual(keptTerminalNotice(d, t, ago), ['terminal.kept', { name: 'Phone', ago: 'since 123' }]);
});

test('the panel describes the running terminals and disables Return for the current one', () => {
  const d = { terminals: [
    { id: 2, since: 12, attached: false, left: 123, name: 'Phone' },
    { id: 4, since: 34, attached: true, name: 'Browser' },
  ] };
  const t = (key, args) => args ? [key, args] : key;
  const ago = (since) => `since ${since}`;
  const calls = [];
  const open = (target) => calls.push(target);
  for (const current of [undefined, 4]) {
    const { rows: [detached, attached], newTerminal } = terminalActions(d, t, ago, open, current);
    assert.deepEqual(detached.text, ['terminal.detached', { id: 2, name: 'Phone', ago: 'since 123' }]);
    assert.deepEqual(attached.text, ['terminal.attached', { id: 4, name: 'Browser' }]);
    assert.equal(detached.label, 'terminal.return');
    assert.equal(detached.disabled, false);
    assert.equal(attached.label, current ? 'terminal.current' : 'terminal.return');
    assert.equal(attached.disabled, !!current);
    assert.equal(newTerminal.label, 'terminal.new');
    detached.run();
    if (!attached.disabled) attached.run();
    newTerminal.run();
  }
  const { rows, newTerminal } = terminalActions({}, t, ago, open);
  assert.deepEqual(rows, []);
  newTerminal.run();
  assert.deepEqual(calls, [2, 4, null, 2, null, null]);
});

test('disconnect offers cancel, destructive close, and primary leave in that order', () => {
  const cancel = () => {};
  const close = () => {};
  const leave = () => {};
  for (const canClose of [true, false]) {
    assert.deepEqual(terminalDisconnectActions((key) => key, cancel, close, leave, canClose), [
      { label: 'common.cancel', run: cancel },
      { label: 'terminal.closeAndDisconnect', danger: true, disabled: !canClose, run: close },
      { label: 'tb.disconnect', primary: true, run: leave },
    ]);
  }
});

test('a terminal here offers takeover only with the live capability', () => {
  const t = (key, args) => args ? [key, args] : key;
  const device = { terminals: [{ id: 7, name: 'This computer', attached: true, here: true }] };
  for (const takeover of [false, true]) {
    const calls = [];
    const { rows: [row] } = terminalActions(device, t, null, (...args) => calls.push(args), undefined, takeover);
    assert.deepEqual(row.text, takeover ? ['terminal.here', { id: 7 }] : ['terminal.attached', { id: 7, name: 'This computer' }]);
    assert.equal(row.label, takeover ? 'terminal.takeOver' : 'terminal.return');
    row.run();
    assert.deepEqual(calls, [takeover ? [7, true] : [7]]);
  }
  assert.deepEqual(terminalTakenNotice(t, 7), ['terminal.takenHere', { id: 7 }]);
});

test('a terminal left at this computer uses the browser wording when marked here', () => {
  const t = (key, args) => args ? [key, args] : key;
  const ago = (since) => `since ${since}`;
  const terminal = { id: 7, name: 'This computer', attached: false, left: 123 };
  for (const [status, text] of [
    [terminal, ['terminal.detached', { id: 7, name: 'This computer', ago: 'since 123' }]],
    [{ ...terminal, here: true }, ['terminal.detachedHere', { id: 7, ago: 'since 123' }]],
  ]) {
    const calls = [];
    const { rows: [row] } = terminalActions({ terminals: [status] }, t, ago, (...args) => calls.push(args), undefined, true);
    assert.deepEqual(row.text, text);
    assert.equal(row.label, 'terminal.return');
    row.run();
    assert.deepEqual(calls, [[7]]);
  }
});
