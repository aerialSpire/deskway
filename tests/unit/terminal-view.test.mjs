import { test } from 'node:test';
import assert from 'node:assert/strict';

globalThis.document = { getElementById: () => null };
globalThis.matchMedia = () => ({ matches: false });
const { DataView } = await import('../../web/static/js/session/data-view.js');
const { TerminalView } = await import('../../web/static/js/session/terminal-view.js');

test('fresh admission reattaches an open terminal from Files at its last size', () => {
  const s = {
    connected: true, hasCap: () => true, caps: new Set(), device: {},
    stage: { hidden: true }, ready() {}, updateRemoteControls() {}, osk: { toggle() {} },
  };
  const data = Object.assign(Object.create(DataView.prototype), {
    s, currentView: 'files', openTransfers() {}, files: { update() {} }, transfers: { next() {} },
  });
  let output = '';
  const encoder = new TextEncoder();
  const terminal = Object.assign(Object.create(TerminalView.prototype), {
    s, status: {}, el: {}, loading: Promise.resolve(),
    emulator: {
      rows: 31, cols: 93,
      reset: () => { output = ''; },
      write: (bytes, done) => { output += new TextDecoder().decode(bytes); done(); },
      input: (text) => terminal.stream.input(encoder.encode(text)),
    },
  });
  data.terminal = terminal;
  Object.defineProperty(s, 'blocked', { get: () => data.blocked });
  const connect = () => {
    const sent = [];
    const channel = {
      readyState: 'open',
      send: (raw) => sent.push(typeof raw === 'string' ? JSON.parse(raw) : raw),
    };
    data.useChannels({ terminal: channel });
    data.active();
    const receive = (message) => channel.onmessage({
      data: typeof message === 'string' ? encoder.encode(message).buffer : JSON.stringify(message),
    });
    return { channel, sent, receive };
  };

  assert.equal(terminal.waiting, 'terminal.starting');
  const first = connect();
  assert.deepEqual(first.sent, []);
  s.stage.hidden = false;
  data.currentView = 'terminal';
  terminal.el.clientWidth = terminal.el.clientHeight = 1;
  terminal.fitter = { fit() {} };
  data.active();
  first.receive({ t: 'opened' });
  first.receive('earlier output');
  assert.equal(output, 'earlier output');

  s.stage.hidden = true;
  data.currentView = 'files';
  first.channel.readyState = 'closed';
  first.channel.onclose();
  assert.equal(terminal.waiting, 'terminal.reconnecting');
  assert.equal(data.hasTerminal, true);
  const second = connect();
  assert.deepEqual(second.sent, [{ t: 'open', rows: 31, cols: 93 }]);
  second.receive({ t: 'opened', replay: 14 });
  second.receive('earlier output');
  assert.equal(data.canClose, true);
  assert.deepEqual(second.sent.at(-1), { t: 'ack', bytes: 14 });

  s.stage.hidden = false;
  data.currentView = 'terminal';
  data.active();
  assert.equal(output, 'earlier output');
  assert.equal(terminal.key({ t: 'x', v: 'echo current\r' }), true);
  assert.deepEqual(second.sent.at(-1), encoder.encode('echo current\r'));
  second.receive(' current');
  assert.equal(output, 'earlier output current');
  second.receive({ t: 'exit', code: 0 });
  assert.equal(data.hasTerminal, false);
  assert.equal(terminal.waiting, 'terminal.starting');
});
