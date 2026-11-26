import assert from 'node:assert/strict';
import test from 'node:test';
import { keptTerminalNotice, terminalDisconnectActions } from '../../web/static/js/terminal-ui.js';

test('the detail notice needs a live terminal report and uses its elapsed time and device name', () => {
  const d = { online: true, caps: ['terminal'], kept_terminal: { since: 123, name: 'Phone' } };
  const t = (key, args) => [key, args];
  const ago = (since) => `since ${since}`;
  for (const update of [{ online: false }, { caps: ['desk'] }, { kept_terminal: undefined }]) {
    assert.equal(keptTerminalNotice({ ...d, ...update }, t, ago), null);
  }
  assert.deepEqual(keptTerminalNotice(d, t, ago), ['terminal.kept', { name: 'Phone', ago: 'since 123' }]);
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
