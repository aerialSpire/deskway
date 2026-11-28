import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TerminalChannel } from '../../web/static/js/session/terminal-channel.js';
import { TERMINAL_FRAME, TERMINAL_INPUT_WINDOW, TERMINAL_OUTPUT_WINDOW } from '../../web/static/js/proto.js';

function setup(target) {
  const sent = [], output = [], status = [];
  const channel = {
    readyState: 'open', bufferedAmount: 0,
    send: (data) => sent.push(typeof data === 'string' ? JSON.parse(data) : data),
    close() { this.readyState = 'closed'; this.onclose(); },
  };
  const terminal = new TerminalChannel(channel, (bytes, done) => output.push({ bytes, done }), (m) => status.push(m));
  const receive = (m) => channel.onmessage({ data: m instanceof ArrayBuffer ? m : JSON.stringify(m) });
  terminal.size(24, 80, target);
  return { terminal, channel, sent, output, status, receive };
}

test('open and resize use the terminal channel', () => {
  for (const [target, id, selection] of [
    [undefined, undefined, {}], [null, 21, { new: true }], [7, 7, { id: 7 }],
  ]) {
    const { terminal, sent, receive } = setup(target);
    assert.deepEqual(sent.shift(), { t: 'open', rows: 24, cols: 80, ...selection });
    terminal.input(Uint8Array.of(3));
    assert.equal(sent.length, 0);
    receive({ t: 'opened', id });
    assert.equal(terminal.id, id);
    terminal.size(24, 80, target);
    terminal.size(31, 93, target);
    assert.deepEqual(sent.shift(), { t: 'resize', rows: 31, cols: 93 });
  }
});

test('switching sends close and waits for the channel and outstanding emulator writes', () => {
  for (const channelClosesFirst of [false, true]) {
    const { terminal, channel, sent, output, status, receive } = setup(4);
    receive({ t: 'opened', id: 4 });
    receive(new ArrayBuffer(5));
    channel.close = () => { channel.readyState = 'closing'; };
    const closed = () => { channel.readyState = 'closed'; channel.onclose(); };
    let left = 0;
    terminal.leave(() => left++);
    assert.equal(channel.readyState, 'closing');
    assert.deepEqual(sent.at(-1), { t: 'close' });
    const consumed = output[0].done;
    const [before, after] = channelClosesFirst ? [closed, consumed] : [consumed, closed];
    before();
    assert.equal(left, 0);
    after();
    assert.equal(left, 1);
    assert.deepEqual(status, [{ t: 'opened', id: 4 }]);
  }
});

test('replay consumes ordinary output credit and enables input after the emulator catches up', () => {
  const { terminal, sent, output, status, receive } = setup();
  const opened = { t: 'opened', replay: 5, dropped: true };
  receive(opened);
  assert.equal(terminal.attached, true);
  receive(new ArrayBuffer(3));
  receive(new ArrayBuffer(2));
  output[0].done();
  terminal.input(Uint8Array.of(1));
  assert.equal(terminal.running, false);
  assert.deepEqual(status, []);
  assert.deepEqual(sent.slice(1), [{ t: 'ack', bytes: 3 }]);
  output[1].done();
  assert.deepEqual(status, [opened]);
  assert.equal(terminal.running, true);
  assert.deepEqual(sent.at(-1), { t: 'ack', bytes: 2 });
});

test('explicit close completes after confirmation or shell exit and pending output', () => {
  for (const result of [{ t: 'closed' }, { t: 'exit', code: 17 }]) {
    const { terminal, sent, receive, output, status } = setup();
    assert.equal(terminal.terminate(), false);
    receive({ t: 'opened' });
    receive(new ArrayBuffer(1));
    assert.equal(terminal.terminate(), true);
    assert.equal(terminal.terminate(), false);
    assert.equal(terminal.running, false);
    assert.deepEqual(sent.slice(1), [{ t: 'terminate' }]);
    receive(result);
    assert.deepEqual(status, [{ t: 'opened' }]);
    output[0].done();
    assert.deepEqual(status.at(-1), { t: 'closed' });
  }
});

test('a failed close shows an error and resumes input on the same attachment', () => {
  const { terminal, sent, receive, status } = setup();
  receive({ t: 'opened' });
  assert.equal(terminal.terminate(), true);
  const error = { t: 'error', why: 'close' };
  receive(error);
  assert.deepEqual(status.at(-1), error);
  assert.equal(terminal.running, true);
  const input = Uint8Array.of(1);
  terminal.input(input);
  assert.deepEqual(sent.at(-1), input);
  assert.equal(terminal.terminate(), true);
});

test('input waits for PTY credit and preserves order across a paste and a key', () => {
  const { terminal, channel, sent, receive } = setup();
  receive({ t: 'opened' });
  sent.length = 0;
  const paste = Uint8Array.from({ length: 5 * TERMINAL_INPUT_WINDOW + 11 }, (_, i) => i % 251);
  const key = Uint8Array.of(4);
  terminal.input(paste);
  assert.equal(sent.length, TERMINAL_INPUT_WINDOW / TERMINAL_FRAME);
  assert.equal(sent.reduce((n, frame) => n + frame.length, 0), TERMINAL_INPUT_WINDOW);
  assert.equal(channel.bufferedAmount, 0);
  terminal.input(key);
  assert.equal(sent.length, TERMINAL_INPUT_WINDOW / TERMINAL_FRAME);
  receive({ t: 'ack', bytes: 7 });
  assert.equal(sent.at(-1).length, 7);
  for (let remaining = paste.length + key.length - TERMINAL_INPUT_WINDOW - 7; remaining > 0; remaining -= TERMINAL_FRAME) {
    receive({ t: 'ack', bytes: TERMINAL_FRAME });
  }
  assert(sent.every((frame) => frame.length <= TERMINAL_FRAME));
  assert.deepEqual(Buffer.concat(sent), Buffer.concat([paste, key]));
});

test('credit returns after processing and exit follows the last output', () => {
  const { terminal, sent, output, status, receive } = setup();
  receive({ t: 'opened' });
  receive(Uint8Array.of(27, 255, 10).buffer);
  receive(Uint8Array.of(5).buffer);
  receive({ t: 'exit', code: -15 });
  assert.equal(terminal.running, false);
  assert.deepEqual(status, [{ t: 'opened' }]);
  assert.deepEqual(sent.slice(1), []);
  assert.deepEqual(output[0].bytes, Uint8Array.of(27, 255, 10));
  output[0].done();
  assert.deepEqual(sent.slice(1), [{ t: 'ack', bytes: 3 }]);
  assert.equal(status.length, 1);
  output[1].done();
  assert.deepEqual(sent.slice(1), [{ t: 'ack', bytes: 3 }, { t: 'ack', bytes: 1 }]);
  assert.deepEqual(status.at(-1), { t: 'exit', code: -15 });
});

test('terminal messages cannot overrun the output window or reverse message direction', () => {
  for (const invalid of [new ArrayBuffer(TERMINAL_FRAME + 1), { t: 'resize', rows: 24, cols: 80 }, { t: 'exit', code: '0' }]) {
    const { receive, status } = setup();
    receive({ t: 'opened' });
    receive(invalid);
    assert.deepEqual(status.at(-1), { t: 'error', why: 'protocol' });
  }
  for (const bytes of [0, -1, 2, 1.5]) {
    const { terminal, receive, status } = setup();
    receive({ t: 'opened' });
    terminal.input(Uint8Array.of(1));
    receive({ t: 'ack', bytes });
    assert.deepEqual(status.at(-1), { t: 'error', why: 'protocol' });
  }
  const { receive, status, output, channel } = setup();
  receive({ t: 'opened' });
  for (let n = 0; n < TERMINAL_OUTPUT_WINDOW; n += TERMINAL_FRAME) receive(new ArrayBuffer(TERMINAL_FRAME));
  receive(new ArrayBuffer(1));
  assert.equal(channel.readyState, 'closed');
  assert.equal(output.length, TERMINAL_OUTPUT_WINDOW / TERMINAL_FRAME);
  assert.deepEqual(status, [{ t: 'opened' }]);
  for (const chunk of output) chunk.done();
  assert.deepEqual(status.at(-1), { t: 'error', why: 'protocol' });
});
