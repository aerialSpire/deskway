// One ordered transfer queue for the Files and Terminal views.
import { t } from '../i18n.js';
import { CAPS, FileTransferMsg as Msg, FILE_TRANSFER_FRAME as FRAME, FILE_TRANSFER_HEADER as HEADER,
  FILE_TRANSFER_WINDOW as WINDOW, validate } from '../proto.js';
import { Credit } from './credit.js';
import { chooseDownload, DOWNLOAD_LIMIT } from './download-file.js';
import { readFileChunk } from './file-chunks.js';
import { safeText } from './file-listing.js';
import { Progress, percent } from './progress.js';

const CHUNK = FRAME - HEADER;
const number = new Intl.NumberFormat();
const count = (n) => number.format(n);
const sizeOk = (size) => Number.isSafeInteger(size) && size >= 0;
const errors = {
  changed: 'files.transfer.changed', limit: 'files.transfer.limit', not_regular: 'files.transfer.notRegular',
  size: 'files.transfer.size', protocol: 'files.error.io', local_io: 'files.transfer.localIo',
};

export class FileTransfers {
  constructor(strip, connected, update, uploaded) {
    this.connected = connected;
    this.update = update;
    this.uploaded = uploaded;
    this.queue = [];
    this.id = 0;
    this.progress = new Progress(strip);
    this.failure = new Progress(strip);
  }

  get ready() { return this.channel?.readyState === 'open' && this.connected(); }

  attach(channel) {
    this.channel = channel;
    channel.binaryType = 'arraybuffer';
    channel.addEventListener('open', () => this.update());
    channel.addEventListener('message', ({ data }) => {
      if (this.channel === channel) this.receive(data);
    });
    channel.addEventListener('close', () => {
      if (this.channel === channel) {
        this.destroy();
        this.update();
      }
    });
  }

  upload(path, files) {
    if (!this.ready) return;
    for (const file of files) {
      this.queue.push({ path, file, name: safeText(file.name), size: file.size, direction: 'upload' });
    }
    this.next();
  }

  download(path, entry, name, suggestedName) {
    if (!this.ready) return;
    const target = chooseDownload(suggestedName).catch((e) => e.name === 'AbortError' ? null : undefined);
    this.queue.push({ path, entry, name, target, direction: 'download' });
    this.next();
  }

  next() {
    if (!this.current && this.ready && this.queue.length) {
      const job = this.queue.shift();
      Object.assign(job, { id: ++this.id, phase: 'preparing', offset: 0, done: 0, credit: new Credit(WINDOW) });
      this.current = job;
      void this.begin(job);
    }
    this.render();
  }

  async begin(job) {
    if (job.file) {
      job.phase = 'offered';
      this.send(job, Msg.UPLOAD, { path: job.path, name: job.file.name, size: job.file.size });
      return;
    }
    const target = await job.target;
    if (this.current !== job || job.phase !== 'preparing') return;
    if (!target) {
      this.end(job, target === null ? 'cancelled' : 'local_io');
      return;
    }
    job.target = target;
    job.phase = 'offered';
    this.send(job, Msg.DOWNLOAD, { path: job.path, entry: job.entry, limit: target.limit });
  }

  send(job, t, fields) {
    try {
      this.channel.send(JSON.stringify({ t, id: job.id, ...fields }));
    } catch {
      this.channel?.close();
    }
  }

  async pump(job) {
    if (job.pumping || job.phase !== 'sending') return;
    job.pumping = true;
    try {
      while (job.offset < job.size && job.credit.available >= Math.min(CHUNK, job.size - job.offset)) {
        const length = Math.min(CHUNK, job.size - job.offset);
        const bytes = await readFileChunk(job.file, job.offset, length);
        if (this.current !== job || job.phase !== 'sending') return;
        if (bytes.byteLength !== length) throw new Error();
        const frame = new ArrayBuffer(HEADER + length);
        const header = new DataView(frame);
        header.setUint32(0, job.id);
        header.setBigUint64(4, BigInt(job.offset));
        new Uint8Array(frame, HEADER).set(new Uint8Array(bytes));
        this.channel.send(frame);
        job.credit.take(length);
        job.offset += length;
      }
      if (job.done === job.size) {
        job.phase = 'finishing';
        this.send(job, Msg.FINISH, { offset: job.done });
      }
    } catch {
      this.stop(job, 'local_io');
    } finally {
      job.pumping = false;
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
      this.end(job, job.problem || m.why);
      return;
    }
    if (m.t === Msg.DONE && m.offset === job.size &&
        (job.phase === 'finishing' || job.phase === 'committed' || job.phase === 'stopping')) {
      this.end(job);
      return;
    }
    if (job.phase === 'stopping') return;
    if (m.t === Msg.READY && job.phase === 'offered' && sizeOk(m.size)) {
      if (job.file && job.size === m.size) {
        job.phase = 'sending';
        void this.pump(job);
      } else if (!job.file) {
        job.size = m.size;
        void this.openTarget(job);
      } else this.stop(job, 'protocol');
    } else if (m.t === Msg.ACK && job.phase === 'sending' && sizeOk(m.offset) &&
        job.credit.acknowledge(m.offset - job.done)) {
      this.moved(job, m.offset);
      void this.pump(job);
    } else if (m.t === Msg.FINISH && job.phase === 'receiving' && m.offset === job.size && job.done === job.size) {
      void this.commitTarget(job);
    } else this.stop(job, 'protocol');
  }

  async openTarget(job) {
    job.phase = 'opening';
    this.render();
    try {
      const writer = await job.target.open(job.size);
      if (this.current !== job || job.phase !== 'opening') {
        await writer?.abort();
        return;
      }
      if (!writer) { this.stop(job, 'limit'); return; }
      job.writer = writer;
      job.writes = Promise.resolve();
      job.phase = 'receiving';
      this.send(job, Msg.START, { offset: 0 });
    } catch {
      this.stop(job, 'local_io');
    }
  }

  receiveBytes(job, frame) {
    if (frame.byteLength <= HEADER || frame.byteLength > FRAME) { this.stop(job, 'protocol'); return; }
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
      if (job.phase !== 'receiving') return;
      await job.writer.write(bytes);
      if (job.phase !== 'receiving') return;
      job.credit.acknowledge(bytes.length);
      this.moved(job, job.done + bytes.length);
      this.send(job, Msg.ACK, { offset: job.done });
    }).catch(() => this.stop(job, 'local_io'));
  }

  async commitTarget(job) {
    job.phase = 'committing';
    this.render();
    try {
      await job.writer.close();
      if (this.current !== job || job.phase !== 'committing') return;
      job.phase = 'committed';
      this.send(job, Msg.DONE, { offset: job.done });
    } catch {
      this.stop(job, 'local_io');
    }
  }

  moved(job, done) {
    job.done = done;
    this.progress.setDetail(this.detail(job));
    this.progress.setPercent(percent(done, job.size));
  }

  detail(job) {
    return job.size === undefined ? null : t('files.transfer.bytes', {
      done: count(job.done), total: count(job.size),
    });
  }

  render() {
    const job = this.current;
    if (!job) { this.progress.remove(); return; }
    const actions = this.queue.length ? [{ text: t('files.transfer.cancelWaiting'), run: () => this.cancelWaiting() }] : [];
    const close = job.phase === 'committing' || job.phase === 'committed' || job.phase === 'stopping'
      ? undefined : { label: t('common.cancel'), run: () => this.stop(job) };
    this.progress.draw({
      label: t('files.transfer.' + job.direction, { name: job.name }),
      detail: this.detail(job), pct: percent(job.done, job.size), more: this.queue.length, actions, close,
    });
  }

  cancelWaiting() {
    this.queue.length = 0;
    this.render();
  }

  stop(job, problem = 'cancelled') {
    if (this.current !== job || job.phase === 'stopping') return;
    if (job.phase === 'preparing') { this.end(job, problem); return; }
    job.phase = 'stopping';
    job.problem = problem;
    this.abortWriter(job);
    const why = problem === 'local_io' ? 'io' : problem === 'protocol' ? 'protocol' : undefined;
    this.send(job, why ? Msg.ERROR : Msg.CANCEL, why ? { why } : undefined);
    this.render();
  }

  abortWriter(job) {
    if (!job.writer) return;
    void Promise.resolve(job.writer.abort()).catch(() => {});
    job.writer = null;
  }

  end(job, why) {
    job.phase = 'ended';
    this.current = null;
    if (why) this.abortWriter(job);
    if (why && why !== 'cancelled') {
      const reason = t(errors[why] || ('files.error.' + why), { limit: count(DOWNLOAD_LIMIT / 1048576) });
      this.failure.draw({ tone: 'bad', label: t('files.transfer.failure', { name: job.name, reason }),
        close: { label: t('common.close'), run: () => this.failure.remove() } });
    }
    if (!why && job.file) this.uploaded(job.path);
    this.next();
  }

  recover() {
    this.cancelWaiting();
    if (this.current) this.stop(this.current);
  }

  destroy() {
    this.queue.length = 0;
    if (this.current) this.end(this.current, 'cancelled');
    this.channel = null;
    this.progress.remove();
    this.failure.remove();
  }
}
