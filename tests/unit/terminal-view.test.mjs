import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Element } from './dom.mjs';

globalThis.document = { getElementById: () => null, createElement: (tag) => new Element(tag) };
globalThis.matchMedia = () => ({ matches: false });
globalThis.getComputedStyle = () => ({ fontSize: '14px', getPropertyValue: () => '' });
const { DataView } = await import('../../web/static/js/session/data-view.js');
const { TerminalView } = await import('../../web/static/js/session/terminal-view.js');

function setup(view = 'files', { queueWrites = false } = {}) {
  const s = {
    connected: true, hasCap: () => true, caps: new Set(), device: {}, waiting: null,
    stage: { hidden: view === 'files' }, updateRemoteControls() {}, releaseAll() {}, osk: { toggle() {} },
    ready() { this.waiting = null; },
    setWaiting(notice) { this.waiting = notice; },
  };
  const data = Object.assign(Object.create(DataView.prototype), {
    s, currentView: view, openTransfers() {}, files: { update() {} }, transfers: { next() {} },
  });
  const encoder = new TextEncoder();
  const writes = [];
  class Emulator {
    constructor({ rows, cols }) { Object.assign(this, { rows, cols, output: '', textarea: {} }); }
    loadAddon() {}
    open() {}
    onData() {}
    dispose() { this.disposed = true; }
    reset() { this.output = ''; }
    write(bytes, done) {
      const consume = () => { this.output += new TextDecoder().decode(bytes); done(); };
      if (queueWrites) writes.push(consume);
      else consume();
    }
    input(text) { terminal.stream.input(encoder.encode(text)); }
  }
  class Fitter { fit() {} }
  const terminal = Object.assign(Object.create(TerminalView.prototype), {
    s, status: {}, el: { clientWidth: 1, clientHeight: 1 }, fitter: new Fitter(), loading: Promise.resolve(),
    xterm: { Terminal: Emulator, FitAddon: Fitter }, emulator: new Emulator({ rows: 31, cols: 93 }),
  });
  data.terminal = terminal;
  Object.defineProperty(s, 'blocked', { get: () => data.blocked });
  const makeChannel = () => {
    const sent = [];
    const channel = {
      readyState: 'open',
      send: (raw) => sent.push(typeof raw === 'string' ? JSON.parse(raw) : raw),
      close() { this.readyState = 'closing'; },
    };
    const receive = (message) => channel.onmessage({
      data: typeof message === 'string' ? encoder.encode(message).buffer : JSON.stringify(message),
    });
    const closed = () => { channel.readyState = 'closed'; channel.onclose(); };
    return { channel, sent, receive, closed };
  };
  const connect = () => {
    const stream = makeChannel();
    data.useChannels({ terminal: stream.channel });
    data.active();
    return stream;
  };
  const flushWrites = () => { for (const consume of writes.splice(0)) consume(); };
  return { s, data, terminal, connect, makeChannel, encoder, flushWrites, output: () => terminal.emulator.output };
}

test('fresh admission reattaches an open terminal from Files at its last size', () => {
  const { s, data, terminal, connect, encoder, output } = setup();
  assert.equal(terminal.waiting, 'terminal.starting');
  const first = connect();
  assert.deepEqual(first.sent, []);
  s.stage.hidden = false;
  data.currentView = 'terminal';
  data.active();
  first.receive({ t: 'opened' });
  first.receive('earlier output');
  assert.equal(output(), 'earlier output');

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
  assert.equal(output(), 'earlier output');
  assert.equal(terminal.key({ t: 'x', v: 'echo current\r' }), true);
  assert.deepEqual(second.sent.at(-1), encoder.encode('echo current\r'));
  second.receive(' current');
  assert.equal(output(), 'earlier output current');
  second.receive({ t: 'exit', code: 0 });
  assert.equal(data.hasTerminal, false);
  assert.equal(terminal.waiting, 'terminal.starting');
});

test('fresh admission uses a pending selection before the previous channel finishes closing', () => {
  for (const [target, selection] of [[9, { id: 9 }], [null, { new: true }]]) {
    const { terminal, connect } = setup('terminal');
    const first = connect();
    first.receive({ t: 'opened', id: 7 });
    terminal.select(target);
    assert.equal(terminal.selection, target);
    assert.equal(first.channel.readyState, 'closing');
    const lateClose = first.channel.onclose;
    const reconnected = connect();
    first.channel.readyState = 'closed';
    lateClose();
    assert.deepEqual(reconnected.sent, [{ t: 'open', rows: 31, cols: 93, ...selection }]);
  }
});

test('readmission during a switch keeps queued writes out of the new terminal', () => {
  const { terminal, connect, flushWrites, output } = setup('terminal', { queueWrites: true });
  const first = connect();
  first.receive({ t: 'opened', id: 7 });
  const replaced = terminal.emulator;
  first.receive('old output');
  terminal.select(9);

  const next = connect();
  next.receive({ t: 'opened', id: 9 });
  next.receive('new output');
  flushWrites();

  assert.equal(replaced.disposed, true);
  assert.equal(output(), 'new output');
  assert.deepEqual(first.sent.at(-1), { t: 'close' });
  assert.deepEqual(next.sent.at(-1), { t: 'ack', bytes: 10 });
});

test('terminal switch progress follows stage visibility and clears on open', () => {
  for (const [hiddenOnClose, hiddenOnOpen] of [[false, false], [true, true], [false, true]]) {
    const { s, terminal, connect, makeChannel } = setup('terminal');
    const first = connect();
    const next = makeChannel();
    s.connection = { pc: { createDataChannel: () => next.channel } };
    first.receive({ t: 'opened', id: 7 });
    terminal.select(9);

    s.stage.hidden = hiddenOnClose;
    first.closed();
    assert.equal(s.waiting, hiddenOnClose ? null : 'terminal.reconnecting');

    s.stage.hidden = hiddenOnOpen;
    next.receive({ t: 'opened', id: 9 });
    assert.equal(s.waiting, null);
  }
});

test('opened refreshes the toolbar and a picker drawn before attachment', () => {
  const { s, data, connect } = setup('terminal');
  let devices = {};
  let panel;
  Object.assign(data, {
    deviceStatus: () => devices,
    terminalText: { textContent: 'terminal.label' }, terminalArrow: { hidden: true }, terminalButton: new Element('button'),
  });
  s.toolbar = {
    panelKind: 'terminals', fit() {},
    refreshPanel(kind) { if (this.panelKind === kind) panel = data.terminalPanel(); },
  };
  s.updateRemoteControls = () => data.updateControls();

  const first = connect();
  devices = { terminals: [{ id: 7, attached: true }, { id: 9, attached: false }] };
  data.refreshTerminalList();
  const firstAction = () => panel.firstElementChild.childNodes[1];
  assert.equal(firstAction().getAttribute('disabled'), null);
  assert.equal(data.terminalText.textContent, 'terminal.label');

  first.receive({ t: 'opened', id: 7 });
  assert.equal(firstAction().getAttribute('disabled'), '');
  assert.equal(firstAction().childNodes[0], 'terminal.current');
  assert.equal(data.terminalText.textContent, 'terminal.numbered');
});

test('only the live session can enable explicit terminal selection', () => {
  const { s, terminal, connect } = setup('terminal');
  s.device.caps = ['terminal', 'terminals'];
  s.hasCap = (cap) => cap === 'terminal';
  terminal.target = 9;
  const { sent } = connect();
  assert.equal(terminal.canSwitch, false);
  assert.deepEqual(sent, [{ t: 'open', rows: 31, cols: 93 }]);
});
