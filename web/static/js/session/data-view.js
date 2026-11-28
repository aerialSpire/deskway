// Terminal and Files share one authenticated connection and one toolbar.
import { dialog } from '../dialogs.js';
import { h, ICONS, svg } from '../dom.js';
import { agoText, t } from '../i18n.js';
import { CAPS } from '../proto.js';
import { terminalActions, terminalTakenNotice } from '../terminal-ui.js';
import { FilesView } from './files-view.js';
import { FileTransfers } from './file-transfers.js';
import { TerminalView } from './terminal-view.js';

// A locked host has ended the connection. This page can return with the same resume chain.
const interrupted = new Map();

export class DataView {
  constructor(s, view, target, deviceStatus) {
    this.s = s;
    this.deviceStatus = deviceStatus;
    this.helloCaps = [CAPS.TERMINALS, CAPS.TERMINAL_TAKEOVER];
    this.currentView = view;
    this.terminal = new TerminalView(s, target, (id) => this.terminalTaken(id));
    this.strip = h('div', { class: 'file-transfer-strip' });
    const transferView = [this.strip, () => s.connected, () => this.files.update(),
      (path) => this.files.uploaded(path), () => this.openTransfers()];
    const previous = interrupted.get(s.device.id);
    interrupted.delete(s.device.id);
    this.transfers = previous || new FileTransfers(...transferView);
    if (previous) previous.mount(...transferView);
    this.files = new FilesView(s, this.transfers);
    this.buttons = this.terminal.buttons;
    this.terminalText = h('span', {}, t('terminal.label'));
    this.terminalArrow = h('span', { class: 'route-arrow', hidden: true }, svg(ICONS.chevron));
    this.terminalButton = h('button', {
      class: 'tool', type: 'button', 'data-session-view': 'terminal', 'aria-pressed': 'false',
      onclick: () => {
        if (!this.filesVisible && this.terminal.canSwitch) s.toolbar.togglePanel('terminals');
        else this.show('terminal');
      },
    }, this.terminalText, this.terminalArrow);
    this.filesButton = h('button', {
      class: 'tool', type: 'button', 'data-session-view': 'files', 'aria-pressed': 'false', onclick: () => this.show('files'),
    }, t('files.label'));
    this.navigation = [this.terminalButton, this.filesButton];
  }

  get filesVisible() { return this.currentView === 'files'; }
  get canBrowse() { return this.s.caps ? this.s.hasCap(CAPS.BROWSE) : this.s.device.caps?.includes(CAPS.BROWSE); }
  get hasTerminal() { return this.terminal.opened || !!this.terminal.stream?.attached; }
  get keeps() { return this.terminal.keeps; }
  get canClose() { return this.terminal.canClose; }
  get controlsReady() { return !this.filesVisible && this.terminal.controlsReady; }
  get blocked() { return !this.controlsReady; }
  get connectingNotice() { return this.filesVisible ? null : this.terminal.connectingNotice; }
  get recoveringNotice() { return this.filesVisible ? null : this.terminal.recoveringNotice; }
  get waiting() { return this.filesVisible ? 'common.loading' : this.terminal.waiting; }

  bind() {
    this.terminal.bind();
    this.s.toolbar.addPanel('terminals', this.terminalButton, () => this.terminalPanel());
    this.show(this.currentView);
  }

  show(view, target) {
    if (this.currentView !== view) this.s.releaseAll();
    this.currentView = view;
    const files = this.filesVisible;
    const canBrowse = this.canBrowse;
    this.s.root.classList.toggle('files-session', files);
    this.s.stage.hidden = files;
    this.s.osk.button.hidden = files;
    if (files) this.s.osk.toggle(false);
    this.filesButton.hidden = !canBrowse;
    this.filesButton.setAttribute('aria-pressed', String(files));
    this.terminalButton.setAttribute('aria-pressed', String(!files));
    this.files.show(files);
    this.s.updateRemoteControls();
    if (files) {
      if (this.s.caps && !canBrowse) this.files.showNotice(t('files.unavailable'));
      if (this.s.connected && this.s.caps) this.s.ready();
    } else {
      if (target !== undefined) this.terminal.select(target);
      if (this.s.connected && !this.terminal.stream?.attached && this.terminal.stream?.phase !== 'done') {
        this.s.setWaiting(t(this.terminal.waiting));
      }
      this.s.stage.focus({ preventScroll: true });
    }
    this.terminal.active();
    this.s.toolbar.show();
    this.s.layout();
  }

  terminalPanel() {
    const stream = this.terminal.stream;
    const choose = (target, take) => {
      const open = () => {
        if (this.s.closed) return;
        this.takenId = undefined;
        this.s.toolbar.closePanel(this.s.stage);
        this.terminal.select(target, take);
        this.show('terminal');
      };
      if (take) {
        dialog(t('terminal.takeOverTitle', { id: target }), null, [
          { label: t('common.cancel') },
          { label: t('terminal.takeOver'), primary: true, run: open },
        ]);
      } else open();
    };
    const { rows, newTerminal } = terminalActions(this.deviceStatus(), t, (since) => agoText(since, { seen: false }),
      choose, stream?.attached ? stream.id : undefined, this.s.hasCap(CAPS.TERMINAL_TAKEOVER));
    return h('section', { class: 'panel terminal-picker', 'aria-label': t('terminal.label') },
      this.takenId === undefined ? null : h('div', { class: 'notice', role: 'status' }, terminalTakenNotice(t, this.takenId)),
      rows.map(({ text, label, run, disabled }) => h('div', { class: 'notice' }, h('span', { class: 'text' }, text),
        h('button', { class: 'btn compact', type: 'button', 'aria-label': `${label} · ${text}`, disabled, onclick: run }, label))),
      h('button', { class: 'btn compact block', type: 'button', onclick: newTerminal.run }, newTerminal.label));
  }

  terminalTaken(id) {
    this.takenId = id;
    const { toolbar } = this.s;
    if (toolbar.panelKind !== 'terminals') toolbar.togglePanel('terminals');
    toolbar.show();
  }

  refreshTerminalList() { this.updateControls(); }

  updateControls() {
    const { toolbar, stage } = this.s;
    const supported = this.s.hasCap(CAPS.TERMINALS);
    const open = toolbar.panelKind === 'terminals';
    if (open && ((this.filesVisible && this.takenId === undefined) || !this.terminal.canSwitch)) toolbar.closePanel(stage);
    const stream = this.terminal.stream;
    const id = supported && stream?.attached && this.deviceStatus()?.terminals?.length > 1 ? stream.id : undefined;
    const label = id === undefined ? t('terminal.label') : t('terminal.numbered', { id });
    const hidden = !this.canBrowse && !supported;
    // The arrow shows whenever the button opens the list, with one terminal too: the list is
    // where another one is started.
    const listed = !this.filesVisible && this.terminal.canSwitch;
    if (this.terminalButton.hidden !== hidden || this.terminalText.textContent !== label || this.terminalArrow.hidden === listed) {
      this.terminalText.textContent = label;
      this.terminalButton.hidden = hidden;
      this.terminalArrow.hidden = !listed;
      toolbar.fit();
    }
    if (listed) {
      this.terminalButton.setAttribute('aria-expanded', String(toolbar.panelKind === 'terminals'));
    } else {
      this.terminalButton.removeAttribute('aria-expanded');
    }
    toolbar.refreshPanel('terminals');
  }

  keyboardSink() { return this.terminal.keyboardSink(); }
  useChannels(channels) { this.terminal.useChannels(channels); }

  applyCaps() {
    if (!this.s.hasCap(CAPS.TERMINAL)) { this.s.connection.end('no_terminal'); return; }
    if (this.s.hasCap(CAPS.BROWSE) && !this.files.channel) {
      this.files.attach(this.s.connection.pc.createDataChannel('browse'));
    }
    this.openTransfers();
    this.show(this.currentView);
  }

  active() {
    this.openTransfers();
    this.transfers.next();
    this.files.update();
    if (this.filesVisible && this.s.caps) this.s.ready();
    this.terminal.active();
  }

  recover() {
    this.transfers.recover();
  }

  openTransfers() {
    if (this.s.connected && this.s.hasCap(CAPS.FILE_TRANSFER) && !this.transfers.channel) {
      this.transfers.attach(this.s.connection.pc.createDataChannel(CAPS.FILE_TRANSFER));
    }
  }

  reconnect() {
    this.recover();
    this.files.detach();
  }
  placeIme(...args) { this.terminal.placeIme(...args); }
  placeKeypad(...args) { this.terminal.placeKeypad(...args); }
  terminate() { this.terminal.terminate(); }

  destroy(locked) {
    if (locked && this.transfers.park()) {
      interrupted.set(this.s.device.id, this.transfers);
    } else {
      this.transfers.destroy();
    }
    this.files.destroy();
    this.terminal.destroy();
  }
}
