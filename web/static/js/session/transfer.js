// Files and pictures to the computer on the "file" channel (host: transfer/), one at a time:
// files dropped on the screen or picked with “Send files or images” go to its Downloads and onto its
// clipboard. The shared clipboard's large data rides the same transfers (`sendClip`); what
// comes back down (`meta`, bytes, `fin`) is the clipboard's (clipboard.js). One small card at
// the top right (progress.js) says how a file transfer goes.
//
// Public surface: stall (ms without progress that ends a transfer), current, queue, progress
// (the card), attach(channel), open(), sendMessage(m), send(files), sendClip(job),
// cancelClip(id), onMessage(data), pump(), cancel(), dismiss(), busy(), bindDrop(), destroy().
import { randomId } from '../env.js';
import { t, filesText } from '../i18n.js';
import { CAPS, DATA_CHUNK, FileMsg } from '../proto.js';
import { readFileChunk } from './file-chunks.js';
import { Progress, SHOW_AFTER, percent, whyText } from './progress.js';

const XFER = { FILE_MAX: 25 << 20, TOTAL_MAX: 50 << 20, FILES_MAX: 10, AHEAD: 1 << 20 };

export class Transfers {
  constructor(s) {
    this.s = s;
    // No progress for this long ends a transfer, so one stuck transfer never blocks the next.
    this.stall = 20000;
    this.channel = null;
    this.queue = [];
    this.current = null;
    this.progress = new Progress(s.stack);
    // The job the card shows.
    this.shown = null;
  }

  // Files dragged onto the screen go to the remote computer.
  bindDrop() {
    const stage = this.s.stage;
    const draggingFiles = (e) => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
    stage.addEventListener('dragover', (e) => {
      if (!draggingFiles(e)) return;
      e.preventDefault();
      const ready = this.s.connected && this.s.hasCap(CAPS.FILES);
      e.dataTransfer.dropEffect = ready ? 'copy' : 'none';
      stage.classList.toggle('dropping', ready);
    });
    stage.addEventListener('dragleave', (e) => { if (e.target === stage || !stage.contains(e.relatedTarget)) stage.classList.remove('dropping'); });
    stage.addEventListener('drop', (e) => {
      stage.classList.remove('dropping');
      if (!draggingFiles(e)) return;
      // A drop the computer cannot accept should still never navigate the controller away.
      e.preventDefault();
      this.send(e.dataTransfer.files);
    });
  }

  attach(channel) { this.channel = channel; }

  open() {
    return !!this.channel && this.channel.readyState === 'open';
  }

  sendMessage(m) {
    if (this.open()) this.channel.send(JSON.stringify(m));
  }

  // Transfers waiting or under way, for the question before disconnecting.
  busy() {
    return (this.current && !this.current.error && !this.current.clip ? 1 : 0) + this.queue.filter((j) => !j.clip).length;
  }

  // Files from this device to the remote computer's Downloads.
  send(list) {
    const files = [...(list || [])];
    if (!files.length || !this.s.connected || !this.s.hasCap(CAPS.FILES)) return;
    const total = files.reduce((n, f) => n + f.size, 0);
    const job = { id: randomId(), files, total, done: 0, label: filesText(files.map((f) => f.name)) };
    if (files.length > XFER.FILES_MAX || total > XFER.TOTAL_MAX || files.some((f) => f.size > XFER.FILE_MAX)) {
      job.error = t('xfer.tooBig');
      this.showError(job);
      return;
    }
    this.queue.push(job);
    this.pump();
  }

  // Clipboard data answering a paste on the computer: a transfer under the request's id and
  // generation, ahead of waiting files (someone is waiting for it). It shows no card: the
  // clipboard shows its own note, through `onProgress(done)` and `onEnd(ok)`.
  sendClip({ id, gen, kind, blob, onProgress, onEnd }) {
    const name = kind === 'image' ? 'image.png' : `clipboard.${kind === 'html' ? 'html' : 'txt'}`;
    const file = new File([blob], name, { type: blob.type });
    this.queue.unshift({ id, files: [file], total: file.size, done: 0, label: name, clip: { gen, kind }, onProgress, onEnd });
    this.pump();
  }

  // The paste was given up: its transfer stops, or never starts.
  cancelClip(id) {
    if (this.current && this.current.id === id) this.cancel();
    else this.queue = this.queue.filter((j) => j.id !== id);
  }

  pump() {
    if (this.current || !this.queue.length || !this.open()) return;
    const job = this.queue.shift();
    this.current = job;
    job.started = performance.now();
    job.moved = job.started;
    this.watch(job);
    clearTimeout(this.cardTimer);
    if (!job.clip) this.cardTimer = setTimeout(() => this.render(), SHOW_AFTER);
    const items = job.files.map((f) => ({ n: f.name, s: f.size }));
    this.sendMessage(job.clip ? { t: FileMsg.PUT, id: job.id, kind: job.clip.kind, items, gen: job.clip.gen } : { t: FileMsg.PUT, id: job.id, kind: 'files', items });
  }

  // Checks every few seconds that the transfer is still moving; a stalled one is cancelled at
  // the computer and ends here with a message, and the next one goes.
  watch(job) {
    clearInterval(this.watchTimer);
    this.watchTimer = setInterval(() => {
      if (this.current !== job) { clearInterval(this.watchTimer); return; }
      if (performance.now() - job.moved < this.stall) return;
      clearInterval(this.watchTimer);
      this.sendMessage({ t: FileMsg.CANCEL, id: job.id });
      this.fail(job, 'timeout');
    }, 2000);
  }

  onMessage(data) {
    const job = this.current;
    if (job) job.moved = performance.now();
    // Everything coming down is clipboard data.
    if (typeof data !== 'string') { this.s.clip.onFileBytes(data); return; }
    let m;
    try { m = JSON.parse(data); } catch { return; }
    if (m.t === FileMsg.META || m.t === FileMsg.FIN) { this.s.clip.onFileText(m); return; }
    if (!job || m.id !== job.id) return;
    if (m.t === FileMsg.READY) this.upload(job);
    else if (m.t === FileMsg.DONE) this.finishUp(job);
    else if (m.t === FileMsg.ERR) this.fail(job, m.why);
  }

  async upload(job) {
    const ch = this.channel;
    const live = () => this.current === job && ch === this.channel && ch.readyState === 'open';
    for (const f of job.files) {
      for (let off = 0; off < f.size; off += DATA_CHUNK) {
        while (live() && ch.bufferedAmount > XFER.AHEAD) {
          await new Promise((r) => { ch.addEventListener('bufferedamountlow', r, { once: true }); setTimeout(r, 300); });
        }
        if (!live()) return;
        const buf = await readFileChunk(f, off);
        if (!live()) return;
        ch.send(buf);
        job.done += buf.byteLength;
        job.moved = performance.now();
        if (job.onProgress) job.onProgress(job.done);
        else this.render();
      }
    }
  }

  finishUp(job) {
    job.result = t('xfer.sent', { keys: this.s.device.os === 'macos' ? '⌘V' : 'Ctrl+V' });
    this.end(job);
  }

  // Told only when it took long enough for the card to show.
  end(job) {
    this.current = null;
    clearTimeout(this.cardTimer);
    if (job.onEnd) {
      job.onEnd(true);
      this.render();
    } else if (performance.now() - job.started >= SHOW_AFTER) {
      this.render(job);
      this.cardTimer = setTimeout(() => this.render(), 2500);
    } else {
      this.render();
    }
    this.pump();
  }

  fail(job, why) {
    if (job.onEnd) {
      if (this.current === job) this.current = null;
      job.onEnd(false);
      this.pump();
      return;
    }
    job.error = whyText(why);
    job.retry = ['net', 'io', 'size'].includes(why);
    if (this.current === job) this.current = null;
    this.showError(job);
    this.pump();
  }

  showError(job) {
    clearTimeout(this.cardTimer);
    this.render(job);
  }

  cancel() {
    const job = this.current;
    if (job) {
      this.sendMessage({ t: FileMsg.CANCEL, id: job.id });
      this.current = null;
    }
    this.render();
    this.pump();
  }

  // The card: what moves, how far; or how it ended.
  render(ended) {
    const job = ended || this.current;
    // Clipboard transfers show in the clipboard's own note.
    const show = job && !job.clip && (ended || performance.now() - job.started >= SHOW_AFTER);
    if (!show) {
      this.dismissCard();
      return;
    }
    const pct = () => percent(job.done, job.total);
    if (this.shown === job && this.progress.el && !ended && !job.error) {
      // Only the numbers change while it runs; rebuilt at most a few times a second.
      if (this.frame) return;
      this.frame = requestAnimationFrame(() => {
        this.frame = null;
        this.progress.setPercent(pct());
      });
      return;
    }
    this.shown = job;
    const over = ended || job.error;
    const close = { label: over ? t('common.close') : t('xfer.cancel'), run: () => (over ? this.dismiss() : this.cancel()) };
    if (job.error) {
      const retry = { text: t('common.retry'), run: () => { this.dismiss(); this.queue.unshift({ ...job, id: randomId(), done: 0, error: null }); this.pump(); } };
      this.progress.draw({ tone: 'bad', label: job.error, actions: job.retry ? [retry] : [], close });
    } else if (ended) {
      this.progress.draw({ tone: 'ok', label: `✓ ${job.result}`, close });
    } else {
      this.progress.draw({ icon: '↑', label: job.label, pct: pct(), more: this.queue.length, close });
    }
  }

  dismissCard() {
    this.progress.remove();
    this.shown = null;
  }

  dismiss() {
    clearTimeout(this.cardTimer);
    this.dismissCard();
    if (this.current) this.cardTimer = setTimeout(() => this.render(), 0);
  }

  destroy() {
    this.queue = [];
    clearTimeout(this.cardTimer);
    clearInterval(this.watchTimer);
  }
}
