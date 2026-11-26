// The terminal byte stream. Each receiver returns credit as it consumes bytes.
import { TerminalMsg as Msg, TERMINAL_FRAME, TERMINAL_INPUT_WINDOW, TERMINAL_OUTPUT_WINDOW, validate } from '../proto.js';

export class TerminalChannel {
  constructor(channel, write, status) {
    this.channel = channel;
    this.write = write;
    this.status = status;
    this.phase = 'ready';
    this.pending = 0;
    this.queuedInput = [];
    this.inputOutstanding = 0;
    channel.binaryType = 'arraybuffer';
    channel.onmessage = ({ data }) => this.receive(data);
    channel.onclose = () => this.finish({ t: Msg.ERROR, why: 'io' });
  }

  get running() { return this.phase === 'running'; }

  send(m) {
    if (this.channel.readyState !== 'open') return false;
    this.channel.send(JSON.stringify(m));
    return true;
  }

  size(rows, cols) {
    if (this.phase !== 'ready' && !this.running) return;
    const size = `${rows}:${cols}`;
    if (this.lastSize === size) return;
    if (this.send({ t: this.running ? Msg.RESIZE : Msg.OPEN, rows, cols })) {
      this.lastSize = size;
      if (this.phase === 'ready') this.phase = 'opening';
    }
  }

  input(bytes) {
    if (!this.running || !bytes.length || this.channel.readyState !== 'open') return;
    this.queuedInput.push(bytes);
    this.flushInput();
  }

  flushInput() {
    while (this.queuedInput.length && this.inputOutstanding < TERMINAL_INPUT_WINDOW && this.channel.readyState === 'open') {
      const bytes = this.queuedInput[0];
      const n = Math.min(bytes.length, TERMINAL_FRAME, TERMINAL_INPUT_WINDOW - this.inputOutstanding);
      this.channel.send(bytes.subarray(0, n));
      this.inputOutstanding += n;
      if (n === bytes.length) this.queuedInput.shift();
      else this.queuedInput[0] = bytes.subarray(n);
    }
  }

  receive(data) {
    if (this.phase === 'done') return;
    if (data instanceof ArrayBuffer) {
      const n = data.byteLength;
      if (!this.running || !n || n > TERMINAL_FRAME || this.pending + n > TERMINAL_OUTPUT_WINDOW) {
        this.protocolError();
        return;
      }
      this.pending += n;
      this.write(new Uint8Array(data), () => {
        this.pending -= n;
        if (!this.disposed) this.send({ t: Msg.ACK, bytes: n });
        this.reportResult();
      });
      return;
    }
    let m;
    if (typeof data === 'string' && data.length <= TERMINAL_FRAME) {
      try { m = JSON.parse(data); } catch { /* invalid message */ }
    }
    if (validate('terminal', m)) { this.protocolError(); return; }
    if (m.t === Msg.OPENED && this.phase === 'opening') {
      this.phase = 'running';
      this.status(m);
    } else if (m.t === Msg.ACK && this.running && m.bytes > 0 && m.bytes <= this.inputOutstanding) {
      this.inputOutstanding -= m.bytes;
      this.flushInput();
    } else if (m.t === Msg.ERROR || (m.t === Msg.EXIT && this.running)) {
      this.finish(m);
    } else {
      this.protocolError();
    }
  }

  protocolError() {
    this.finish({ t: Msg.ERROR, why: 'protocol' });
    this.channel.close();
  }

  finish(result) {
    if (this.phase === 'done') return;
    this.phase = 'done';
    this.queuedInput.length = 0;
    this.result = result;
    this.reportResult();
  }

  reportResult() {
    if (this.pending || !this.result || this.disposed) return;
    const result = this.result;
    this.result = null;
    this.status(result);
  }

  leave() { this.send({ t: Msg.CLOSE }); }

  dispose() {
    this.disposed = true;
    this.queuedInput.length = 0;
    this.channel.onmessage = null;
    this.channel.onclose = null;
  }
}
