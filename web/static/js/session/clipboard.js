// The shared clipboard, this browser's side (RDP CLIPRDR model; host: host/src/clipboard).
// Copying announces formats only; data moves when something pastes:
// - Copied here: the format list goes to the computer (`cb`), found by `clipboardchange`
//   where the browser has it, else when the page comes to the front and once a second while
//   it is there. A paste on the computer asks (`cbreq`); this page reads its clipboard and
//   answers (`cbdata`, large data as a transfer on the file channel), or `cberr`.
// - Copied on the computer (`cb` from the host): text goes onto this device's clipboard at
//   once (when the page is in front, else when it is again, if still the latest). Pictures and
//   files stay a description until “Get it here”.
// Paste keys are ordinary keys: nothing here looks at them. Sharing is the computer's
// setting; nothing is read or announced while it is off. What it shows is clipboard-ui.js's.
//
// Public surface: share, gen, owner, remote, news, serving, fetching, pendingText, ui,
// setShare(on), toggleShare(), onHostMessage(m), onFileText(m), onFileBytes(data), check(),
// take(), cancelServing(), cancelFetch(r), destroy().
import { h } from '../dom.js';
import { randomId } from '../env.js';
import { t } from '../i18n.js';
import { CAPS, Control, FileMsg } from '../proto.js';
import { ClipboardUi } from './clipboard-ui.js';
import { whyText } from './progress.js';

// Text and HTML up to this size travel inline (as the host's clipboard::INLINE_MAX).
const INLINE_MAX = 48 * 1024;
// Without `clipboardchange`, how often the clipboard is checked while the page is in front:
// a copy is announced before anyone can switch to the remote screen and paste.
const CHECK_EVERY = 1000;
// The computer's refusals of a request that have words of their own; its others (unsupported,
// io) say only that it failed.
const FETCH_WHY = new Set(['gone', 'big', 'denied']);
const MIME = { text: 'text/plain', html: 'text/html', image: 'image/png' };

const utf8Length = (s) => new TextEncoder().encode(s).length;

function saveBlob(blob, name) {
  const url = URL.createObjectURL(blob);
  h('a', { href: url, download: name || 'file' }).click();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

// Whether this browser refuses this site its clipboard (the person, or a policy, said no).
async function readDenied() {
  try { return (await navigator.permissions.query({ name: 'clipboard-read' })).state === 'denied'; } catch { return false; }
}

// What this device's clipboard holds now: { formats, sig, items }, or null when it cannot be
// read (no permission, page not in front).
async function readLocal() {
  const c = navigator.clipboard;
  if (!c || !c.read) return null;
  let items;
  try { items = await c.read(); } catch { return null; }
  const formats = [];
  const parts = [];
  for (const kind of ['text', 'html', 'image']) {
    const item = items.find((i) => i.types.includes(MIME[kind]));
    if (!item) continue;
    const blob = await item.getType(MIME[kind]);
    const f = { kind, size: blob.size };
    if (kind === 'image') {
      try { const bmp = await createImageBitmap(blob); Object.assign(f, { w: bmp.width, h: bmp.height }); bmp.close(); } catch { /* size only */ }
    }
    formats.push(f);
    // Text is compared by its words, everything else by its size.
    parts.push(kind === 'text' ? `text:${await blob.text()}` : `${kind}:${blob.size}`);
  }
  return { formats, sig: parts.join('\n'), items };
}

export class Clipboard {
  constructor(s) {
    this.s = s;
    // As the computer has it: it says so when the session opens.
    this.share = false;
    // The host's latest generation, whose it is, and its formats.
    this.gen = 0;
    this.owner = null;
    this.formats = [];
    // What the computer copied that waits for “Get it here”: { gen, image } | { gen, files }.
    this.remote = null;
    this.news = 0;
    // The last local clipboard announced (or written here), so it is not announced again.
    this.lastSig = null;
    // The id of the announcement not yet confirmed by the computer.
    this.announcing = null;
    // Text from the computer waiting for the page to come to the front: { gen, text }.
    this.pendingText = null;
    // A paste on the computer being answered: { id, gen, format, started, done, total }.
    this.serving = null;
    // This page's own requests (“Get it here”, large text): id -> request.
    this.fetching = new Map();
    // The incoming bytes on the file channel belong to this request id.
    this.incoming = null;
    this.ui = new ClipboardUi(this, s);
    this.watch();
  }

  // ---- the computer's setting ----

  setShare(on) {
    this.share = !!on;
    if (!this.share) {
      this.forget();
    } else {
      // This page's clipboard is announced as it is now, as RDP does when a session starts.
      this.lastSig = null;
      this.check();
    }
    this.ui.update();
  }

  toggleShare() {
    if (this.s.hasCap(CAPS.CLIPBOARD)) this.s.send({ t: Control.CLIP_SHARE, on: !this.share });
  }

  // Nothing of either side is kept: sharing went off, or the connection went.
  forget() {
    this.remote = null;
    this.news = 0;
    this.pendingText = null;
    this.gen = 0;
    this.owner = null;
    this.formats = [];
    if (this.serving) this.endServing();
    for (const r of [...this.fetching.values()]) this.endFetch(r);
  }

  // ---- this device's clipboard changes ----

  watch() {
    const c = navigator.clipboard;
    this.onFront = () => { if (!document.hidden) { this.check(); this.writePending(); } };
    window.addEventListener('focus', this.onFront);
    document.addEventListener('visibilitychange', this.onFront);
    if (c && 'onclipboardchange' in c) {
      this.onChange = () => this.check();
      c.addEventListener('clipboardchange', this.onChange);
    } else {
      this.timer = setInterval(() => { if (!document.hidden && document.hasFocus()) this.check(); }, CHECK_EVERY);
    }
  }

  // Announces this device's clipboard if it changed. Formats only, never the content.
  async check() {
    if (!this.share || this.checking || !this.s.connected || !this.s.hasCap(CAPS.CLIPBOARD)) return;
    this.checking = true;
    try {
      const local = await readLocal();
      if (!local) {
        // The browser refuses outright (not just a page out of focus): copies here cannot reach
        // the computer, which is said once, with what to do, instead of failing in silence.
        if (!this.deniedSaid && await readDenied()) this.noteReadDenied();
        return;
      }
      this.deniedSaid = false;
      if (!this.share || local.sig === this.lastSig) return;
      // The id comes back in a cberr if the computer cannot take its clipboard.
      const id = randomId();
      if (this.s.send({ t: Control.CLIPBOARD, formats: local.formats, id })) {
        this.lastSig = local.sig;
        this.announcing = id;
      }
    } finally {
      this.checking = false;
    }
  }

  noteReadDenied() {
    if (this.deniedSaid) return;
    this.deniedSaid = true;
    this.s.note(t('clip.readDenied'), 8000);
  }

  // ---- messages from the computer ----

  onHostMessage(m) {
    if (m.t === Control.CLIP_SHARE) this.setShare(m.on);
    else if (m.t === Control.CLIPBOARD) this.onAnnounce(m);
    else if (m.t === Control.CLIPBOARD_REQUEST) this.serve(m);
    else if (m.t === Control.CLIPBOARD_DATA) this.onData(m);
    else if (m.t === Control.CLIPBOARD_ERROR) this.onError(m);
  }

  // The computer's clipboard state: the latest generation wins.
  onAnnounce(m) {
    if (!this.share || !Number.isInteger(m.gen) || m.gen < this.gen) return;
    this.gen = m.gen;
    this.owner = m.owner;
    if (m.owner === 'browser') {
      this.announcing = null;
      this.refusedSaid = false;
    }
    this.formats = m.formats || [];
    this.remote = null;
    this.pendingText = null;
    // A newer announcement makes earlier requests of this page pointless.
    for (const r of [...this.fetching.values()]) if (r.gen < m.gen) this.cancelFetch(r);
    if (m.owner === 'host') {
      const has = (k) => this.formats.find((f) => f.kind === k);
      const image = has('image');
      const files = has('files');
      const text = has('text');
      if (image) this.remote = { gen: m.gen, image };
      else if (files) this.remote = { gen: m.gen, files: (files.items || []).slice(0, 100) };
      // Text too long to travel with the announcement waits like a picture: nothing moves
      // until “Get it here”.
      else if (text && typeof m.text !== 'string') this.remote = { gen: m.gen, text };
      this.news = this.remote ? 1 : 0;
      if (typeof m.text === 'string') this.writeText(m.gen, m.text);
    } else {
      this.news = 0;
    }
    this.ui.update();
  }

  // Text from the computer onto this device's clipboard: now if the page is in front, else
  // when it is again (if nothing newer came meanwhile).
  async writeText(gen, text) {
    this.pendingText = { gen, text };
    await this.writePending();
  }

  async writePending() {
    const p = this.pendingText;
    if (!p || document.hidden || !document.hasFocus() || p.gen !== this.gen) return;
    try {
      await navigator.clipboard.writeText(p.text);
      // Written here, so not announced back as a copy of this device.
      this.lastSig = `text:${p.text}`;
      if (this.pendingText === p) this.pendingText = null;
    } catch { /* not in front after all: tried again when it is */ }
  }

  // ---- a paste on the computer: this page provides the data ----

  async serve(m) {
    const fail = (why) => this.s.send({ t: Control.CLIPBOARD_ERROR, gen: m.gen, id: m.id, why });
    if (!this.share || this.owner !== 'browser' || m.gen !== this.gen) { fail('gone'); return; }
    // A newer paste replaces one still being answered (the computer gave that one up).
    if (this.serving) this.endServing();
    const job = { id: m.id, gen: m.gen, format: m.format, started: performance.now(), done: 0, total: 0 };
    this.serving = job;
    this.ui.noteSoon();
    let blob = null;
    try {
      const items = await navigator.clipboard.read();
      const item = items.find((i) => i.types.includes(MIME[m.format]));
      if (item) blob = await item.getType(MIME[m.format]);
    } catch {
      if (this.serving === job) {
        fail('denied');
        this.endServing();
        this.noteReadDenied();
        // The remote placeholder has already returned empty. Publish the same copy
        // again when clipboard access returns, so the next paste gets a fresh placeholder.
        if (this.gen === job.gen && this.owner === 'browser' && !this.announcing) {
          this.lastSig = null;
          if (!document.hidden && document.hasFocus()) this.check();
        }
      }
      return;
    }
    if (this.serving !== job) return;
    if (!blob) { fail('unsupported'); this.endServing(); return; }
    if (m.format !== 'image' && blob.size <= INLINE_MAX) {
      this.s.send({ t: Control.CLIPBOARD_DATA, gen: m.gen, format: m.format, id: m.id, [m.format]: await blob.text() });
      this.endServing();
      return;
    }
    // Large: the bytes follow on the file channel as a transfer under this request.
    job.total = blob.size;
    this.s.send({ t: Control.CLIPBOARD_DATA, gen: m.gen, format: m.format, id: m.id, size: blob.size });
    this.s.transfer.sendClip({
      id: m.id, gen: m.gen, kind: m.format, blob,
      onProgress: (done) => { job.done = done; this.ui.drawNote(); },
      onEnd: (ok) => {
        if (this.serving !== job) return;
        if (!ok) fail('io');
        this.endServing();
      },
    });
  }

  // ✕ on “Pasting on the remote computer…”: the paste on the computer gets nothing.
  cancelServing() {
    const job = this.serving;
    if (!job) return;
    this.s.send({ t: Control.CLIPBOARD_ERROR, gen: job.gen, id: job.id, why: 'cancelled' });
    this.s.transfer.cancelClip(job.id);
    this.endServing();
  }

  endServing() {
    this.serving = null;
    this.ui.drawNote();
  }

  // ---- this page asks the computer for its data ----

  fetch(gen, format, { i, name, size, onBlob } = {}) {
    const r = { id: randomId(), gen, format, i, name: name || '', total: size || 0, done: 0, parts: null, mime: MIME[format] || '', started: performance.now(), onBlob };
    this.fetching.set(r.id, r);
    this.s.send({ t: Control.CLIPBOARD_REQUEST, gen, format, id: r.id, ...(Number.isInteger(i) ? { i } : {}) });
    this.ui.noteSoon();
    return r;
  }

  onData(m) {
    const r = this.fetching.get(m.id);
    if (!r) return;
    if (typeof m.text === 'string') this.finish(r, new Blob([m.text], { type: 'text/plain' }));
    else if (typeof m.html === 'string') this.finish(r, new Blob([m.html], { type: 'text/html' }));
    else if (Number.isInteger(m.size)) { r.total = m.size; this.ui.drawNote(); }
  }

  onError(m) {
    if (m.id === this.announcing) {
      // Not on the computer's clipboard: announced again at the next check. Why is said once
      // until it works again, so a copy that cannot get there never fails in silence.
      this.announcing = null;
      this.lastSig = null;
      if (!this.refusedSaid) {
        this.refusedSaid = true;
        this.s.note(t(m.why === 'denied' ? 'clip.notTaken' : 'clip.notPlaced'), 8000);
      }
      return;
    }
    const r = this.fetching.get(m.id);
    if (!r) return;
    this.endFetch(r);
    this.s.note(whyText(FETCH_WHY.has(m.why) ? m.why : null), 3000);
  }

  // `meta` and `fin` on the file channel: the bytes in between are the request's data.
  onFileText(m) {
    const r = this.fetching.get(m.id);
    if (m.t === FileMsg.META && r) {
      this.incoming = r;
      Object.assign(r, { parts: [], total: m.size, name: r.name || m.name, mime: m.mime || r.mime });
    } else if (m.t === FileMsg.FIN && r && this.incoming === r) {
      this.incoming = null;
      this.finish(r, new Blob(r.parts, { type: r.mime }));
    }
  }

  onFileBytes(data) {
    const r = this.incoming;
    // Leftovers of a cancelled request are dropped.
    if (!r || !this.fetching.has(r.id)) return;
    r.parts.push(data);
    r.done += data.byteLength;
    this.ui.drawNote();
  }

  async finish(r, blob) {
    this.fetching.delete(r.id);
    this.ui.drawNote();
    if (r.format === 'text') this.writeText(r.gen, await blob.text());
    else if (r.onBlob) r.onBlob(blob);
    else saveBlob(blob, r.name);
  }

  cancelFetch(r) {
    this.s.send({ t: Control.CLIPBOARD_ERROR, gen: r.gen, id: r.id, why: 'cancelled' });
    this.s.transfer.sendMessage({ t: FileMsg.CANCEL, id: r.id });
    this.endFetch(r);
  }

  endFetch(r) {
    this.fetching.delete(r.id);
    if (this.incoming === r) this.incoming = null;
    if (r.onFail) r.onFail();
    this.ui.drawNote();
  }

  // “Get it here”: text and pictures onto this device's clipboard (a picture's write must start
  // in the tap, as browsers require, and fills in when it arrives; a browser that refuses it
  // saves the picture into downloads instead, on every device), files into downloads (a browser
  // cannot put files on the clipboard).
  take() {
    const rc = this.remote;
    if (!rc) return;
    this.news = 0;
    this.ui.syncButton();
    if (rc.image) {
      let resolve;
      let reject;
      const blob = new Promise((res, rej) => { resolve = res; reject = rej; });
      let saveInstead = false;
      const keep = (b) => saveBlob(b, 'image.png');
      try {
        navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]).then(() => this.s.note(t('clip.taken'), 2500), () => { saveInstead = true; });
      } catch {
        saveInstead = true;
      }
      const r = this.fetch(rc.gen, 'image', { size: rc.image.size, name: 'image.png', onBlob: (b) => { resolve(b); if (saveInstead) keep(b); } });
      r.onFail = () => reject(new Error('cancelled'));
    } else if (rc.text) {
      this.fetch(rc.gen, 'text', { size: rc.text.size });
    } else {
      rc.files.forEach((f, i) => this.fetch(rc.gen, 'files', { i, name: f.n, size: f.s }));
    }
  }

  // ---- lifetime ----

  destroy() {
    this.forget();
    clearInterval(this.timer);
    this.ui.destroy();
    window.removeEventListener('focus', this.onFront);
    document.removeEventListener('visibilitychange', this.onFront);
    if (this.onChange) navigator.clipboard.removeEventListener('clipboardchange', this.onChange);
  }
}
