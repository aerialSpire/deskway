import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TerminalChannel } from '../../web/static/js/session/terminal-channel.js';
import { TERMINAL_FRAME, TERMINAL_INPUT_WINDOW, TERMINAL_OUTPUT_WINDOW } from '../../web/static/js/proto.js';

function setup() {
  const sent = [], output = [], status = [];
  const channel = {
    readyState: 'open', bufferedAmount: 0,
    send: (data) => sent.push(typeof data === 'string' ? JSON.parse(data) : data),
    close() { this.readyState = 'closed'; this.onclose(); },
  };
  const terminal = new TerminalChannel(channel, (bytes, done) => output.push({ bytes, done }), (m) => status.push(m));
  const receive = (m) => channel.onmessage({ data: m instanceof ArrayBuffer ? m : JSON.stringify(m) });
  terminal.size(24, 80);
  return { terminal, channel, sent, output, status, receive };
}

test('open and resize use the terminal channel', () => {
  const { terminal, sent, receive } = setup();
  assert.deepEqual(sent.shift(), { t: 'open', rows: 24, cols: 80 });
  terminal.input(Uint8Array.of(3));
  assert.equal(sent.length, 0);
  receive({ t: 'opened' });
  terminal.size(24, 80);
  terminal.size(31, 93);
  assert.deepEqual(sent.shift(), { t: 'resize', rows: 31, cols: 93 });
  terminal.leave();
  assert.deepEqual(sent.splice(0), [{ t: 'close' }]);
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
