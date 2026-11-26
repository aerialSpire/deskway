import { dialog } from '../dialogs.js';
import { h, ICONS, svg } from '../dom.js';
import { dateText, fmtSize, t, timeText } from '../i18n.js';
import { DOWNLOAD_LIMIT } from './download-file.js';
import { Progress, percent } from './progress.js';

const number = new Intl.NumberFormat();
const sizeText = (bytes) => bytes < 1024 ? t('files.transfer.byteSize', { size: number.format(Math.round(bytes)) }) : fmtSize(bytes);
const errors = {
  changed: 'changed', limit: 'limit', not_regular: 'notRegular', integrity: 'integrity',
  expired: 'expired', size: 'size', local_io: 'localIo',
};

export class TransferList {
  constructor(strip, transfers) {
    this.strip = strip;
    this.transfers = transfers;
    this.expanded = false;
    this.rows = new Map();
  }

  render(jobs, current) {
    if (!this.strip) return;
    if (!jobs.length) {
      this.strip.replaceChildren();
      this.toggle = null;
      this.rows.clear();
      this.expanded = false;
      return;
    }
    if (!this.toggle) {
      this.toggle = h('button', { type: 'button', class: 'btn compact', onclick: () => {
        this.expanded = !this.expanded;
        this.items.replaceChildren();
        this.rows.clear();
        this.transfers.render();
      } });
      this.items = h('div', { class: 'file-transfer-items' });
      this.strip.append(this.toggle, this.items);
    }
    this.toggle.setAttribute('aria-expanded', String(this.expanded));
    this.toggle.replaceChildren(t('files.transfer.list', { n: jobs.length }), svg(ICONS.chevron));
    const outcome = jobs.findLast((job) => job.phase === 'ended' && (job.save || (job.problem && job.problem !== 'cancelled')));
    const visible = new Set(this.expanded ? jobs : [current || outcome].filter(Boolean));
    for (const [job, row] of this.rows) {
      if (!visible.has(job)) {
        row.remove();
        this.rows.delete(job);
      }
    }
    for (const job of visible) {
      if (!this.rows.has(job)) this.rows.set(job, new Progress(this.items));
      this.rows.get(job).draw(this.card(job));
    }
  }

  card(job) {
    const { name, direction } = job.request;
    const label = t('files.transfer.' + direction, { name });
    const action = (text, run) => ({ text: t(text), run: () => run.call(this.transfers, job) });
    const dismiss = { label: t('common.close'), run: () => this.transfers.dismiss(job) };
    if (job.phase === 'waiting') {
      return { label: name, detail: t('files.transfer.waiting'),
        actions: [action('common.cancel', this.transfers.stop)] };
    }
    if (job.phase === 'ended') {
      if (job.save) return { label: name, detail: t('files.transfer.readyToSave', { size: sizeText(job.size) }),
        tone: 'ok', actions: [{ text: t('common.save'), run: job.save }], close: dismiss };
      if (!job.problem) return { label: name, detail: t('files.transfer.verified', { size: sizeText(job.size) }), tone: 'ok', close: dismiss };
      const cancelled = job.problem === 'cancelled';
      const key = errors[job.problem] ? 'files.transfer.' + errors[job.problem]
        : job.problem === 'protocol' ? 'files.error.io' : 'files.error.' + job.problem;
      const reason = t(cancelled ? 'files.transfer.cancelled' : key, { limit: DOWNLOAD_LIMIT / 1048576 });
      return { label: t('files.transfer.failure', { name, reason }), tone: cancelled ? null : 'bad',
        actions: [action('files.transfer.tryAgain', this.transfers.retry)], close: dismiss };
    }
    const close = job.saving || job.phase === 'stopping' || job.problem ? null
      : { label: t('common.cancel'), run: () => this.transfers.stop(job) };
    const detail = job.problem === 'cancelled' ? t('files.transfer.cancelled')
      : job.phase === 'interrupted' ? t('files.transfer.interrupted')
      : job.phase === 'conflict' ? t('files.transfer.decision') : this.detail(job);
    const actions = job.phase === 'conflict' ? [action('files.transfer.choose', this.transfers.askConflict)] : [];
    return { label, detail, pct: percent(job.done, job.size), actions, close };
  }

  detail(job) {
    if (job.size === undefined) return null;
    const bytes = t('files.transfer.bytes', { done: number.format(job.done), total: number.format(job.size) });
    if (!job.rateStart || job.done <= job.rateStart.offset) return bytes;
    const elapsed = performance.now() - job.rateStart.at;
    if (elapsed <= 0) return bytes;
    return t('files.transfer.rate', { bytes, rate: sizeText((job.done - job.rateStart.offset) * 1000 / elapsed) });
  }

  moved(job) {
    if (job.phase === 'interrupted') return;
    const row = this.rows.get(job);
    row?.setDetail(this.detail(job));
    row?.setPercent(percent(job.done, job.size));
  }

  conflict(job, existing, many, answer) {
    const { name, file } = job.request;
    const date = (ms) => dateText(ms / 1000) + ' ' + timeText(new Date(ms));
    const remember = h('input', { type: 'checkbox' });
    this.overlay = dialog(t('files.transfer.conflictTitle', { name }), h('div', {},
      h('p', {}, t('files.transfer.existing', { size: sizeText(existing.size), time: date(existing.modified * 1000) })),
      h('p', {}, t('files.transfer.incoming', { size: sizeText(file.size), time: date(file.lastModified) })),
      h('p', {}, t('files.transfer.replaceText')),
      many ? h('label', {}, remember, ' ', t('files.transfer.sameForRest')) : null,
    ), [
      { label: t('files.transfer.skip'), run: () => answer('skip', remember.checked) },
      { label: t('files.transfer.replace'), danger: true, run: () => answer('replace', remember.checked) },
    ]);
  }

  closeConflict() {
    this.overlay?.remove();
    this.overlay = null;
  }
}
