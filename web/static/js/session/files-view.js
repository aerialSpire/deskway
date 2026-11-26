// A paged directory in the data-only session.
import { h, ICONS, svg } from '../dom.js';
import { dialog, formDialog, shownError } from '../dialogs.js';
import { t } from '../i18n.js';
import { CAPS, BrowseMsg as Msg, BROWSE_MAX_BYTES, validate } from '../proto.js';
import { dropdown } from '../views/menu.js';
import { crumbs, decodeName, displayName, entryActions, permissions, safeText } from './file-listing.js';

const bytes = new Intl.NumberFormat(undefined, { style: 'unit', unit: 'byte', unitDisplay: 'short' });
const modified = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
const button = (label, action) => h('button', { class: 'btn compact', type: 'button', onclick: action }, label);
const moreButton = (label) => h('button', { class: 'btn icon more', type: 'button', 'aria-label': label }, svg(ICONS.more));

export class FilesView {
  constructor(s, transfers) {
    this.s = s;
    this.transfers = transfers;
    this.path = undefined;
    this.notice = h('p', { class: 'notice', role: 'status', hidden: true });
    this.breadcrumbs = h('nav', { class: 'files-path', 'aria-label': t('files.path') });
    this.picker = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => {
      if (this.uploadPath && this.transfers.ready) this.transfers.upload(this.uploadPath, this.picker.files);
      this.uploadPath = undefined;
      this.picker.value = '';
    } });
    this.upload = button(t('files.upload'), () => {
      this.uploadPath = this.path;
      this.picker.click();
    });
    this.refresh = button(t('files.refresh'), () => this.open(this.path));
    this.menu = dropdown('files:bar', moreButton(t('files.actions')), (item) => [
      item(t('files.home'), () => this.open()),
      this.path && item(t('files.mkdir'), () => this.enqueue(() => this.nameDialog(this.path))),
    ]);
    this.rows = h('div', { role: 'list' });
    this.tail = h('div', { 'aria-hidden': 'true' });
    this.list = h('div', { class: 'files-list' }, this.rows, this.tail);
    this.el = h('section', { class: 'files-surface', tabindex: '-1', 'aria-label': t('files.label'), hidden: true },
      h('div', { class: 'files-bar' }, this.breadcrumbs, this.upload, this.refresh, this.menu.el, this.picker),
      this.notice, this.list);
    this.observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) this.next();
    }, { root: this.list, rootMargin: '120px' });
    this.list.addEventListener('scroll', () => this.entryMenu?.close(), { passive: true });
    this.bindDrop();
    this.drawPath();
    this.showNotice(t('common.loading'));
    this.update();
  }

  attach(channel) {
    this.channel = channel;
    channel.addEventListener('open', () => this.update());
    channel.addEventListener('message', ({ data }) => this.receive(data));
    channel.addEventListener('close', () => this.fail());
  }

  get ready() { return this.channel?.readyState === 'open' && this.s.connected; }

  get canUpload() { return this.ready && this.listed && this.transfers.ready; }

  bindDrop() {
    const files = (e) => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
    this.el.addEventListener('dragover', (e) => {
      if (!files(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = this.canUpload ? 'copy' : 'none';
    });
    this.el.addEventListener('drop', (e) => {
      if (!files(e)) return;
      e.preventDefault();
      if (!this.canUpload) return;
      const items = [...e.dataTransfer.items].filter((item) => item.kind === 'file');
      const dropped = items.filter((item) => !item.webkitGetAsEntry?.()?.isDirectory)
        .map((item) => item.getAsFile()).filter(Boolean);
      this.transfers.upload(this.path, dropped);
    });
  }

  uploaded(path) {
    this.enqueue(() => {
      if (this.listed && this.path.length === path.length && path.every((part, i) => this.path[i] === part)) {
        return this.loadFolder(path);
      }
    });
  }

  update() {
    const ready = this.ready;
    this.upload.hidden = !this.s.hasCap(CAPS.FILE_TRANSFER);
    this.upload.disabled = !this.canUpload;
    this.refresh.disabled = !ready;
    this.menu.el.querySelector('button').disabled = !ready;
    if (!ready) this.observer.disconnect();
    if (this.el.hidden || !ready || this.queue) return;
    if (!this.started) this.open();
    else if (this.cursor) this.observer.observe(this.tail);
  }

  show(visible) {
    this.el.hidden = !visible;
    if (!visible) {
      this.observer.disconnect();
      this.entryMenu?.close();
      this.menu.close();
    }
    this.update();
  }

  drawPath() {
    const items = crumbs(this.path || []);
    this.breadcrumbs.replaceChildren(...items.flatMap(({ label, path }, i) => [
      i ? h('span', { 'aria-hidden': 'true' }, '›') : null,
      h('button', {
        class: 'btn text compact', type: 'button', title: label,
        'aria-current': i === items.length - 1 && this.path ? 'location' : null,
        onclick: () => this.open(path),
      }, h('bdi', {}, label)),
    ].filter(Boolean)));
    this.breadcrumbs.scrollLeft = this.breadcrumbs.scrollWidth;
  }

  open(path) {
    return this.enqueue(() => this.loadFolder(path));
  }

  async loadFolder(path) {
    this.started = true;
    this.path = path;
    this.listed = false;
    this.upload.disabled = true;
    this.cursor = undefined;
    this.entryMenu?.close();
    this.list.scrollTop = 0;
    this.rows.replaceChildren();
    this.drawPath();
    this.showNotice(t('common.loading'));
    await this.load();
  }

  next() {
    if (this.ready && !this.queue && this.cursor && !this.el.hidden) return this.enqueue(() => this.load(this.cursor), true);
  }

  async load(cursor) {
    try {
      const result = await this.request({ t: Msg.LIST, path: this.path, cursor }, Msg.LISTING);
      if (!cursor) {
        this.path = result.path;
        this.listed = true;
        this.drawPath();
      }
      this.cursor = result.cursor;
      this.rows.append(...result.entries.map((entry) => this.row(entry)));
      this.showNotice(!this.cursor && !this.rows.childElementCount ? t('files.empty') : '');
    } catch (error) {
      this.cursor = undefined;
      this.showNotice(error.message);
    }
  }

  row(entry) {
    const name = displayName(entry.name);
    const actions = entryActions(entry);
    const path = this.path;
    const open = () => this.open([...path, entry.name]);
    const menu = dropdown(Symbol(), moreButton(t('files.entryActions', { name })), (item) => {
      this.entryMenu = menu;
      return actions.map((action) => item(t(action === 'rename' ? 'common.rename' : `files.${action}`),
        { open, rename: () => this.nameDialog(path, entry), delete: () => this.confirmDelete(path, entry, name) }[action],
        { danger: action === 'delete' })).concat(
        this.transfers.ready && entry.kind === 'file'
          ? [item(t('files.download'), () => this.transfers.download(path, entry.name, name, decodeName(entry.name).value ?? name))]
          : []);
    }, { floating: true });
    const nameText = h('bdi', {}, name);
    const label = actions.includes('open')
      ? h('button', { class: 'btn text files-name', type: 'button', title: name, onclick: open }, nameText)
      : h('span', { class: 'files-name', title: name }, nameText);
    const target = entry.target === undefined ? '' : displayName(entry.target);
    return h('div', { class: 'files-row', role: 'listitem' },
      h('div', { class: 'files-label' }, h('span', { class: `files-mark ${entry.kind}`, 'aria-hidden': 'true' }), label,
        target ? h('span', { class: 'files-target', title: target }, '→ ', h('bdi', {}, target)) : null),
      h('div', { class: 'files-meta' }, t(`files.kind.${entry.kind}`), ' · ', bytes.format(entry.size),
        entry.modified === undefined ? null : [' · ', modified.format(entry.modified * 1000)]),
      h('div', { class: 'files-mode' }, permissions(entry), '  ', h('bdi', {}, safeText(entry.owner))),
      menu.el);
  }

  nameDialog(path, entry) {
    if (!this.ready || !path) return;
    const seed = entry ? decodeName(entry.name).value : '';
    const input = h('input', { class: 'input', value: seed && seed === safeText(seed) ? seed : '', required: true });
    formDialog(t(entry ? 'common.rename' : 'files.mkdir'), null,
      [h('label', { class: 'field-label' }, t('common.name'), input)], t('common.save'), async () => {
        if (entry && input.value === seed) return;
        const message = entry
          ? { t: Msg.RENAME, path, entry: entry.name, name: input.value }
          : { t: Msg.MKDIR, path, name: input.value };
        await this.change(message);
      });
    input.select();
  }

  confirmDelete(path, entry, name) {
    if (!this.ready) return;
    dialog(t('files.delete'), h('p', { class: 'files-confirm' }, t('files.confirmDelete', { name })), [
      { label: t('common.cancel') },
      { label: t('files.delete'), danger: true, run: async () => {
        try {
          await this.change({ t: Msg.DELETE, path, entry: entry.name });
        } catch (e) { this.showNotice(e.message); }
      } },
    ]);
  }

  change(message) {
    return this.enqueue(async () => {
      await this.request(message, Msg.CHANGED);
      await this.loadFolder(message.path);
    });
  }

  enqueue(action, automatic = false) {
    this.observer.disconnect();
    const result = (this.queue || Promise.resolve()).then(() => {
      if (!automatic || this.queue === tail) return action();
    });
    const done = () => {
      if (this.queue === tail) {
        this.queue = null;
        this.update();
      }
    };
    const tail = result.then(done, done);
    this.queue = tail;
    return result;
  }

  request(message, expects) {
    if (!this.ready) return Promise.reject(shownError('files.disconnected'));
    return new Promise((resolve, reject) => {
      try {
        this.channel.send(JSON.stringify(message));
        this.pending = { expects, resolve, reject };
      } catch {
        reject(shownError('files.disconnected'));
      }
    });
  }

  receive(data) {
    const pending = this.pending;
    this.pending = null;
    if (!pending) return;
    let reply;
    if (typeof data === 'string' && data.length <= BROWSE_MAX_BYTES) {
      try { reply = JSON.parse(data); } catch { /* invalid reply */ }
    }
    if (validate('browse', reply)) pending.reject(shownError('files.error.io'));
    else if (reply.t === Msg.ERROR) pending.reject(shownError(`files.error.${reply.why}`));
    else if (reply.t === pending.expects) pending.resolve(reply);
    else pending.reject(shownError('files.error.io'));
  }

  showNotice(text) {
    this.notice.textContent = text;
    this.notice.hidden = !text;
  }

  fail() {
    this.pending?.reject(shownError('files.disconnected'));
    this.pending = null;
    this.showNotice(t('files.disconnected'));
    this.update();
  }

  destroy() {
    this.observer.disconnect();
    this.entryMenu?.close();
    this.menu.close();
    this.fail();
  }
}
