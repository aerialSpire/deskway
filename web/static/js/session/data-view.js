// Terminal and Files share one authenticated connection and one toolbar.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { CAPS } from '../proto.js';
import { FilesView } from './files-view.js';
import { FileTransfers } from './file-transfers.js';
import { TerminalView } from './terminal-view.js';

export class DataView {
  constructor(s, view) {
    this.s = s;
    this.currentView = view;
    this.terminal = new TerminalView(s);
    this.strip = h('div', { class: 'file-transfer-strip' });
    this.transfers = new FileTransfers(this.strip, () => s.connected, () => this.files.update(),
      (path) => this.files.uploaded(path), () => this.openTransfers());
    this.files = new FilesView(s, this.transfers);
    this.buttons = this.terminal.buttons;
    this.navigation = ['terminal', 'files'].map((view) => h('button', {
      class: 'tool', type: 'button', 'data-session-view': view, 'aria-pressed': 'false', onclick: () => this.show(view),
    }, t(view === 'terminal' ? 'terminal.label' : 'files.label')));
  }

  get filesVisible() { return this.currentView === 'files'; }
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
    this.show(this.currentView);
  }

  show(view) {
    if (this.currentView !== view) this.s.releaseAll();
    this.currentView = view;
    const files = this.filesVisible;
    const canBrowse = this.s.caps ? this.s.hasCap(CAPS.BROWSE) : this.s.device.caps?.includes(CAPS.BROWSE);
    this.s.root.classList.toggle('files-session', files);
    this.s.stage.hidden = files;
    this.s.osk.button.hidden = files;
    if (files) this.s.osk.toggle(false);
    for (const button of this.navigation) {
      button.hidden = !canBrowse;
      button.setAttribute('aria-pressed', String(button.dataset.sessionView === view));
    }
    this.files.show(files);
    this.s.updateRemoteControls();
    if (files) {
      if (this.s.caps && !canBrowse) this.files.showNotice(t('files.unavailable'));
      if (this.s.connected && this.s.caps) this.s.ready();
    } else {
      if (this.s.connected && !this.terminal.stream?.attached && this.terminal.stream?.phase !== 'done') {
        this.s.setWaiting(t(this.terminal.waiting));
      }
      this.s.stage.focus({ preventScroll: true });
    }
    this.terminal.active();
    this.s.toolbar.show();
    this.s.layout();
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

  destroy() {
    this.transfers.destroy();
    this.files.destroy();
    this.terminal.destroy();
  }
}
