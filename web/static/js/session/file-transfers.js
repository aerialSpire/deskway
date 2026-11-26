// The page owns the queue and the browser's transfer offsets across channels.
import { CAPS, FileTransferMsg as Msg, FILE_TRANSFER_FRAME as FRAME, FILE_TRANSFER_HEADER as HEADER,
  FILE_TRANSFER_WINDOW as WINDOW, validate } from '../proto.js';
import { Credit } from './credit.js';
import { chooseDownload } from './download-file.js';
import { readFileChunk } from './file-chunks.js';
import { safeText } from './file-listing.js';
import { TransferList } from './transfer-list.js';
import { Sha256 } from './sha256.js';

const CHUNK = FRAME - HEADER;
const sizeOk = (size) => Number.isSafeInteger(size) && size >= 0;
// A new session view can use the same page's resume token. Its new requests still need fresh IDs.
let lastId = 0;

export class FileTransfers {
  constructor(strip, connected, update, uploaded, reopen) {
    this.connected = connected;
    this.update = update;
    this.uploaded = uploaded;
    this.reopen = reopen;
    this.jobs = [];
    this.list = new TransferList(strip, this);
  }

  get ready() { return this.channel?.readyState === 'open' && this.connected(); }
  get unfinished() { return !!this.current || this.jobs.some((job) => job.phase === 'waiting'); }

  attach(channel) {
    this.channel = channel;
    channel.binaryType = 'arraybuffer';
    channel.addEventListener('open', () => {
      if (this.channel !== channel) return;
      this.next();
      this.update();
    });
    channel.addEventListener('message', ({ data }) => {
      if (this.ready && this.channel === channel) this.receive(data);
    });
    channel.addEventListener('close', () => {
      if (this.channel !== channel) return;
      this.recover();
      if (this.connected()) this.reopen();
    });
  }

  upload(path, files) {
    if (!this.ready) return;
    const batch = {};
    for (const file of files) {
      this.add({ path, file, name: safeText(file.name), size: file.size, direction: 'upload', batch });
    }
    this.next();
  }

  download(path, entry, name, suggestedName) {
    const target = chooseDownload(suggestedName).catch((e) => e.name === 'AbortError' ? null : undefined);
    this.add({ path, entry, name, suggestedName, target, direction: 'download' });
    this.next();
  }

  add(request) {
    this.jobs.push({ request, size: request.size, phase: 'waiting', done: 0 });
  }

  next() {
    if (this.ready) {
      if (!this.current) {
        const job = this.jobs.find((job) => job.phase === 'waiting');
        if (job) {
          this.current = job;
          Object.assign(job, { id: ++lastId, phase: 'interrupted', offset: 0, hash: new Sha256(), writes: Promise.resolve() });
        }
      }
      if (this.current?.phase === 'interrupted') this.offer(this.current);
    }
    this.render();
  }

  async offer(job) {
    const channel = this.channel;
    job.phase = 'preparing';
    const { path, file, entry } = job.request;
    const target = file ? null : await job.request.target;
    if (!this.attempt(job, channel)) return;
    if (!file && !target) {
      this.end(job, target === null ? 'cancelled' : 'local_io');
      return;
    }
    job.phase = 'offered';
    if (job.accepted) this.send(job, Msg.RESUME);
    else if (file) this.send(job, Msg.UPLOAD, { path, name: file.name, size: file.size });
    else this.send(job, Msg.DOWNLOAD, { path, entry, limit: target.limit });
    this.render();
  }

  attempt(job, channel) {
    return this.current === job && this.channel === channel && this.ready;
  }

  send(job, t, fields) {
    const channel = this.channel;
    try {
      channel.send(JSON.stringify({ t, id: job.id, ...fields }));
    } catch {
      channel.close();
    }
  }

  async pump(job) {
    const channel = this.channel;
    if (job.pumping === channel || job.phase !== 'sending') return;
    job.pumping = channel;
    try {
      while (job.offset < job.size && job.credit.available >= Math.min(CHUNK, job.size - job.offset)) {
        const length = Math.min(CHUNK, job.size - job.offset);
        const bytes = new Uint8Array(await readFileChunk(job.request.file, job.offset, length));
        if (!this.attempt(job, channel) || job.phase !== 'sending') return;
        if (bytes.length !== length) throw new Error();
        const frame = new ArrayBuffer(HEADER + length);
        const header = new DataView(frame);
        header.setUint32(0, job.id);
        header.setBigUint64(4, BigInt(job.offset));
        new Uint8Array(frame, HEADER).set(bytes);
        try { channel.send(frame); } catch { channel.close(); return; }
        const hashed = job.sent || 0;
        if (job.offset + length > hashed) job.hash.update(bytes.subarray(Math.max(0, hashed - job.offset)));
        job.credit.take(length);
        job.offset += length;
        job.sent = Math.max(hashed, job.offset);
      }
      if (job.done === job.size) {
        job.digest ??= job.hash.finish();
        job.phase = 'finishing';
        this.send(job, Msg.FINISH, { offset: job.done, sha256: job.digest });
      }
    } catch {
      if (this.attempt(job, channel)) this.stop(job, 'local_io');
    } finally {
      if (job.pumping === channel) job.pumping = null;
    }
  }

  receive(data) {
    const job = this.current;
    if (!job) return;
    if (data instanceof ArrayBuffer) {
      this.receiveBytes(job, data);
      return;
    }
    let m;
    if (typeof data === 'string' && data.length <= FRAME) {
      try { m = JSON.parse(data); } catch { /* invalid reply */ }
    }
    if (validate(CAPS.FILE_TRANSFER, m)) { this.stop(job, 'protocol'); return; }
    if (m.id !== job.id) return;
    if (m.t === Msg.ERROR) {
      if (job.saving) {
        void job.saving.then(() => this.end(job), () => this.end(job, 'local_io'));
      } else this.end(job, job.problem || m.why);
      return;
    }
    if (m.t === Msg.DONE && m.offset === job.size && (job.request.file ? job.digest : job.saved)) {
      this.end(job);
      return;
    }
    if (job.phase === 'stopping') return;
    if (m.t === Msg.CONFLICT && job.request.file && ['offered', 'finishing'].includes(job.phase)) {
      job.accepted = true;
      if (job.problem) { this.sendStop(job); return; }
      this.conflict(job, m);
    } else if (m.t === Msg.READY && job.phase === 'offered' && sizeOk(m.size) && sizeOk(m.offset)) {
      this.readyAt(job, m);
    } else if (m.t === Msg.ACK && job.phase === 'sending' && sizeOk(m.offset) &&
        job.credit.acknowledge(m.offset - job.done)) {
      this.moved(job, m.offset);
      void this.pump(job);
    } else if (m.t === Msg.FINISH && job.phase === 'receiving' && m.offset === job.size && job.done === job.size) {
      if ((job.digest ??= job.hash.finish()) !== m.sha256) this.stop(job, 'integrity');
      else void this.commitTarget(job);
    } else this.stop(job, 'protocol');
  }

  readyAt(job, { size, offset }) {
    if ((job.size !== undefined && job.size !== size) || offset > (job.sent || 0)) {
      this.stop(job, 'protocol');
      return;
    }
    job.accepted = true;
    if (job.problem) { this.sendStop(job); return; }
    job.size = size;
    job.credit = new Credit(WINDOW);
    if (job.request.file) {
      job.offset = job.done = offset;
      job.rateStart = { at: performance.now(), offset };
      job.phase = 'sending';
      this.render();
      void this.pump(job);
    } else if (job.saving) void this.finishTarget(job);
    else void this.openTarget(job);
  }

  conflict(job, metadata) {
    job.phase = 'conflict';
    job.conflict = metadata;
    const choice = job.request.batch.choice;
    if (choice && !metadata.changed) this.answerConflict(job, choice);
    else {
      this.askConflict(job);
      this.render();
    }
  }

  askConflict(job) {
    if (this.current !== job || job.phase !== 'conflict') return;
    const batch = job.request.batch;
    const many = this.jobs.some((other) => other.phase === 'waiting' && other.request.batch === batch);
    this.list.conflict(job, job.conflict, many, (choice, remember) => {
      if (this.current !== job || job.phase !== 'conflict') return;
      if (remember) batch.choice = choice;
      this.answerConflict(job, choice);
    });
  }

  answerConflict(job, choice) {
    if (choice === 'replace') {
      job.phase = job.digest ? 'finishing' : 'offered';
      this.send(job, Msg.REPLACE);
      this.render();
    } else this.stop(job);
  }

  async openTarget(job) {
    const channel = this.channel;
    job.phase = 'opening';
    this.render();
    try {
      job.writer ??= Promise.resolve(job.request.target).then((target) => target.open(job.size));
      const writer = await job.writer;
      await job.writes;
      if (!this.attempt(job, channel) || job.phase !== 'opening') return;
      if (!writer) { this.stop(job, 'limit'); return; }
      job.offset = job.done;
      job.rateStart = { at: performance.now(), offset: job.done };
      job.phase = 'receiving';
      this.send(job, Msg.START, { offset: job.done });
      this.render();
    } catch {
      if (this.attempt(job, channel)) this.stop(job, 'local_io');
    }
  }

  receiveBytes(job, frame) {
    if (frame.byteLength <= HEADER || frame.byteLength > FRAME) { this.stop(job, 'protocol'); return; }
    const channel = this.channel;
    const header = new DataView(frame);
    if (header.getUint32(0) !== job.id || job.phase === 'stopping') return;
    const bytes = new Uint8Array(frame, HEADER);
    if (job.phase !== 'receiving' || header.getBigUint64(4) !== BigInt(job.offset)
        || bytes.length !== Math.min(CHUNK, job.size - job.offset) || !job.credit.take(bytes.length)) {
      this.stop(job, 'protocol');
      return;
    }
    job.offset += bytes.length;
    job.writes = job.writes.then(async () => {
      if (this.current !== job || job.problem) return;
      await (await job.writer).write(bytes);
      if (this.current !== job || job.problem) return;
      job.hash.update(bytes);
      this.moved(job, job.done + bytes.length);
      if (this.attempt(job, channel)) {
        job.credit.acknowledge(bytes.length);
        this.send(job, Msg.ACK, { offset: job.done });
      }
    }).catch(() => this.stop(job, 'local_io'));
  }

  commitTarget(job) {
    job.saving = job.writer.then((writer) => writer.close()).then((save) => {
      job.save = save;
      job.saved = true;
    });
    return this.finishTarget(job);
  }

  async finishTarget(job) {
    const channel = this.channel;
    job.phase = 'committing';
    this.render();
    try {
      await job.saving;
      if (!this.attempt(job, channel) || job.phase !== 'committing') return;
      job.phase = 'committed';
      this.send(job, Msg.DONE, { offset: job.done });
    } catch {
      if (this.attempt(job, channel) && job.phase === 'committing') this.stop(job, 'local_io');
    }
  }

  moved(job, done) {
    job.done = done;
    this.list.moved(job);
  }

  render() { this.list.render(this.jobs, this.current); }

  stop(job, problem = 'cancelled') {
    if (problem === 'cancelled' && job.saving) return;
    if (job.phase === 'waiting') {
      job.phase = 'ended';
      job.problem = problem;
      this.render();
      return;
    }
    if (this.current !== job || job.problem) return;
    if (job.phase === 'preparing' && !job.accepted) { this.end(job, problem); return; }
    job.problem = problem;
    this.list.closeConflict();
    this.abortWriter(job);
    if (this.ready) this.sendStop(job);
    this.render();
  }

  sendStop(job) {
    job.phase = 'stopping';
    const why = { local_io: 'io', protocol: 'protocol', integrity: 'integrity' }[job.problem];
    this.send(job, why ? Msg.ERROR : Msg.CANCEL, why ? { why } : undefined);
  }

  abortWriter(job) {
    if (job.writer && !job.saving) {
      void Promise.resolve(job.writer).then((writer) => writer?.abort()).catch(() => {});
      job.writer = null;
    }
  }

  end(job, why) {
    if (this.current !== job) return;
    this.list.closeConflict();
    job.phase = 'ended';
    job.problem = why;
    this.current = null;
    if (why) this.abortWriter(job);
    else {
      if (job.request.file) this.uploaded(job.request.path);
      job.request = { name: job.request.name, direction: job.request.direction };
    }
    for (const key of ['writer', 'writes', 'hash', 'saving', 'pumping']) delete job[key];
    this.next();
  }

  retry(job) {
    this.dismiss(job);
    if (job.request.file) {
      this.add({ ...job.request, batch: {} });
      this.next();
    } else {
      const { path, entry, name, suggestedName } = job.request;
      this.download(path, entry, name, suggestedName);
    }
  }

  dismiss(job) {
    this.jobs = this.jobs.filter((other) => other !== job);
    this.render();
  }

  recover() {
    const channel = this.channel;
    this.channel = null;
    channel?.close();
    this.list.closeConflict();
    if (this.current) this.current.phase = 'interrupted';
    this.render();
    this.update();
  }

  destroy() {
    if (this.current) this.abortWriter(this.current);
    this.current = null;
    this.jobs.length = 0;
    this.recover();
  }
}
