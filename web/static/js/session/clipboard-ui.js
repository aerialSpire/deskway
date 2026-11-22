// What the shared clipboard shows (clipboard.js holds its state and speaks to the computer):
// the toolbar's “Clipboard” button and its panel, and the progress note while data moves. What the
// computer copied waits there, marked on the button, until the person takes it.
//
// Public surface: button, progress (the note), update(), buildPanel(), noteSoon(), drawNote(),
// destroy().
import { h } from '../dom.js';
import { t, fmtSize, filesText } from '../i18n.js';
import { CAPS } from '../proto.js';
import { Progress, SHOW_AFTER, percent } from './progress.js';

export class ClipboardUi {
  constructor(clip, s) {
    this.clip = clip;
    this.s = s;
    this.progress = new Progress(s.stack, 'clip-note');
    this.dot = h('span', { class: 'dot', 'aria-hidden': 'true' });
    this.button = h('button', { class: 'tool tool-clip', 'aria-pressed': 'false', onclick: () => s.toolbar.togglePanel('clip') });
  }

  // ---- the toolbar button and the panel ----

  // The button and an open panel say what is true now.
  update() {
    this.syncButton();
    this.s.toolbar.refreshPanel('clip');
  }

  syncButton() {
    const { news, share } = this.clip;
    const badge = news ? h('span', { class: 'tool-badge' }, String(news)) : null;
    // One short word and a dot for the state, so it never wraps on a narrow toolbar.
    this.dot.classList.toggle('ok', share);
    this.button.replaceChildren(h('span', { class: 'tool-clip-label' }, t('clip.name'), this.dot, badge));
    this.button.setAttribute('aria-label', t(share ? (news ? 'tb.clipLabelOnNew' : 'tb.clipLabelOn') : 'tb.clipLabelOff'));
  }

  // What the computer copied, in a few words.
  describe(rc) {
    if (rc.text) return t('clip.text', { size: fmtSize(rc.text.size || 0) });
    if (rc.image) return `${t('clip.image', { size: rc.image.w ? `${rc.image.w}×${rc.image.h}` : '' }).trim()}${rc.image.size ? ` · ${fmtSize(rc.image.size)}` : ''}`;
    if (!rc.files.length) return t('clip.folder');
    const total = rc.files.reduce((n, f) => n + (f.s || 0), 0);
    return `${filesText(rc.files.map((f) => f.n))} · ${fmtSize(total)}`;
  }

  // Files and pictures from this device: they go to the computer's Downloads and onto its
  // clipboard (browsers cannot read files from this device's clipboard).
  filePicker() {
    const picker = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => { this.s.transfer.send(picker.files); picker.value = ''; } });
    return picker;
  }

  buildPanel() {
    const clip = this.clip;
    const rc = clip.remote;
    const sw = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(!!clip.share), 'aria-label': t('clip.share'), onclick: () => clip.toggleShare() }, h('span'));
    let from;
    if (!clip.share) {
      from = h('p', { class: 'clip-quiet' }, t('clip.offHint'));
    } else if (!rc) {
      from = h('p', { class: 'clip-quiet' }, t('clip.onHint'));
    } else {
      from = h('div', { class: 'clip-item' },
        h('span', { class: 'clip-desc' }, this.describe(rc)),
        rc.files && !rc.files.length ? null : h('button', { class: 'btn compact', onclick: () => clip.take() }, t('clip.take')));
    }
    const picker = this.filePicker();
    // Only what the computer says it can do: the shared clipboard, sending files.
    const shared = this.s.hasCap(CAPS.CLIPBOARD);
    const files = this.s.hasCap(CAPS.FILES);
    return h('div', { class: 'panel clip' },
      shared ? h('div', { class: 'clip-share' },
        h('div', {}, h('div', { class: 'clip-title' }, t('clip.share')),
          h('div', { class: 'clip-quiet' }, t('clip.shareText'))),
        sw) : null,
      shared ? h('div', { class: 'clip-section' }, h('div', { class: 'clip-label' }, t('clip.fromRemote')), from) : null,
      files ? h('div', { class: 'clip-section' }, h('div', { class: 'clip-label' }, t('clip.toRemote')),
        h('p', { class: 'clip-quiet' }, t('clip.filesHint')),
        h('div', { class: 'clip-actions' },
          h('button', { class: 'btn', onclick: () => picker.click() }, t('clip.sendFiles')),
          picker)) : null);
  }

  // ---- the progress note: a paste on the computer being answered, or data being taken ----

  noteSoon() {
    clearTimeout(this.noteTimer);
    this.noteTimer = setTimeout(() => this.drawNote(), SHOW_AFTER);
  }

  drawNote() {
    const clip = this.clip;
    const job = clip.serving || clip.fetching.values().next().value;
    if (!job || performance.now() - job.started < SHOW_AFTER) {
      this.progress.remove();
      return;
    }
    const serving = job === clip.serving;
    this.progress.draw({
      label: serving ? t('clip.pasting') : t('clip.taking', { name: job.format === 'files' ? job.name : t(`clip.kind.${job.format}`) }),
      pct: job.total ? percent(job.done, job.total) : null,
      more: serving ? 0 : clip.fetching.size - 1,
      close: { label: t('clip.cancel'), run: () => (serving ? clip.cancelServing() : clip.cancelFetch(job)) },
    });
  }

  destroy() {
    clearTimeout(this.noteTimer);
    this.progress.remove();
  }
}
