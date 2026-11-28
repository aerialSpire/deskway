// The host window owns tabs, confirmations, and the xterm renderer for its current stream.
import { Terminal } from './vendor/xterm.mjs';
import { FitAddon } from './vendor/addon-fit.mjs';
import { TerminalChannel } from './js/session/terminal-channel.js';
import { TerminalMsg } from './js/proto.js';

const encoder = new TextEncoder();
let active, serial = 0, terminals, started = false, notice, locked;
const confirm = $('confirm');

class LocalChannel {
  constructor() {
    this.id = ++serial;
    this.readyState = 'connecting';
  }

  send(data) {
    const binary = typeof data !== 'string';
    // Wry IPC is text. The pipe and the in-process connection carry raw bytes.
    const encoded = binary ? btoa(String.fromCharCode(...data)) : data;
    send({ cmd: 'send', id: this.id, binary, data: encoded });
  }

  close() {
    // The terminal's leave message drains before the local driver closes the connection.
    if (this.readyState === 'open') this.readyState = 'closing';
  }

  abort() {
    send({ cmd: 'disconnect', id: this.id });
    this.ended();
  }

  ended() {
    this.readyState = 'closed';
    this.onclose?.();
  }
}

function render() {
  const rows = (terminals ?? []).toSorted((a, b) => a.id - b.id).map(({ id, attached, here, name }) => {
    const selected = active?.stream.id === id && active.stream.attached;
    const inUse = attached && !here;
    const tab = h('div', { class: 'tab', role: 'tab', tabindex: '0', 'aria-selected': String(selected) },
      t('terminal.window_tab', { id }), inUse ? h('span', { class: 'who' }, t('terminal.window_using', { device: name })) : null);
    tab.onclick = () => {
      if (selected) active.emulator.focus();
      else if (inUse) ask('take', id, name, () => selectTerminal(id, true));
      else selectTerminal(id);
    };
    tab.onkeydown = (event) => {
      if (event.target !== tab || (event.key !== 'Enter' && event.key !== ' ')) return;
      event.preventDefault();
      tab.click();
    };
    if (selected) {
      const close = h('button', { class: 'x', type: 'button', 'aria-label': t('terminal.window_close', { id }) },
        $('close-icon').content.cloneNode(true));
      close.onclick = (event) => {
        event.stopPropagation();
        ask('close', id, null, () => {
          if (active?.stream.id === id) active.stream.terminate();
        });
      };
      tab.append(close);
    }
    return tab;
  });
  if (terminals) {
    const add = h('button', { class: 'tab add', type: 'button', 'aria-label': t('terminal.window_new') },
      $('add-icon').content.cloneNode(true));
    add.onclick = () => selectTerminal(null);
    rows.push(add);
  }
  $('tabs').replaceChildren(...rows);
  $('tabs').hidden = !terminals;
  renderNotice();
}

function renderNotice() {
  let message = '';
  if (started && !terminals) {
    message = t(locked ? 'terminal.window_locked' : 'terminal.window_unavailable');
  } else if (notice?.key === 'terminal.window_taken') {
    // The status list and the stream can report the takeover in either order.
    const row = terminals.find(({ id }) => id === notice.id);
    notice.device ??= row && !row.here ? row.name : undefined;
    if (notice.device !== undefined) message = t(notice.key, notice);
    // Gone from the list before its taker was seen: the shell ended right after the takeover.
    else if (!row) message = t('terminal.window_ended', notice);
  } else if (notice) {
    message = t(notice.key, notice);
  }
  $('status').textContent = message;
  $('status').hidden = !message;
}

function ask(action, id, device, run) {
  const take = action === 'take';
  $('question').textContent = take ? t('terminal.window_take_title', { id }) : t('terminal.window_close_title', { id });
  $('detail').textContent = take ? t('terminal.window_take_detail', { device }) : t('terminal.window_close_detail');
  $('confirm-action').textContent = take ? t('terminal.window_take') : t('common.close');
  confirm.returnValue = '';
  confirm.onclose = () => { if (confirm.returnValue === 'confirm') run(); };
  confirm.showModal();
}

function clear() {
  if (confirm.open) confirm.close();
  if (!active) return;
  const { stream, emulator, channel } = active;
  active = null;
  stream.dispose();
  channel.abort();
  emulator.dispose();
}

function selectTerminal(target, take = false) {
  if (!terminals) return;
  const previous = active;
  if (previous?.switching) return;
  const start = () => {
    clear();
    notice = undefined;
    const channel = new LocalChannel();
    const el = $('emulator');
    const css = getComputedStyle(el);
    const color = (name) => css.getPropertyValue(name).trim();
    const emulator = new Terminal({
      fontFamily: color('--font-mono'),
      fontSize: parseFloat(css.fontSize),
      theme: { background: color('--screen'), foreground: color('--dark-ink'), cursor: color('--dark-ink'), selectionBackground: color('--dark-key-down') },
    });
    const fitter = new FitAddon();
    emulator.loadAddon(fitter);
    emulator.open(el);
    const stream = new TerminalChannel(channel, (data, done) => emulator.write(data, done), (message) => {
      if (message.t === TerminalMsg.OPENED) {
        if (message.dropped) notice = { key: 'terminal.window_dropped' };
        emulator.focus();
      } else if (message.t === TerminalMsg.ERROR && message.why === 'close') {
        notice = { key: 'terminal.close_failed' };
      } else {
        clear();
        if (message.t === TerminalMsg.DETACHED) notice = { key: 'terminal.window_taken', id: stream.id };
        else if (message.t === TerminalMsg.EXIT) notice = { key: 'terminal.window_ended', id: stream.id };
        else if (message.t === TerminalMsg.ERROR) notice = { key: 'terminal.open_failed' };
      }
      render();
    });
    const fit = () => {
      fitter.fit();
      stream.size(emulator.rows, emulator.cols, target, take);
    };
    channel.onopen = fit;
    active = { channel, stream, emulator, fit };
    emulator.onData((text) => stream.input(encoder.encode(text)));
    send({ cmd: 'connect', id: channel.id });
    render();
  };
  if (previous && previous.channel.readyState === 'open') {
    previous.switching = true;
    previous.stream.leave(() => { if (active === previous && terminals) start(); });
  } else start();
}

window.terminalConnected = (id) => {
  const channel = active?.channel;
  if (channel?.id !== id) return;
  channel.readyState = 'open';
  channel.onopen();
};
window.terminalMessage = (id, binary, data) => {
  const channel = active?.channel;
  if (channel?.id !== id) return;
  channel.onmessage?.({ data: binary ? Uint8Array.from(atob(data), (char) => char.charCodeAt(0)).buffer : data });
};
window.terminalEnded = (id) => {
  if (active?.channel.id === id) active.channel.ended();
};
window.setTerminalStatus = (list, isLocked) => {
  if (started && locked === isLocked && JSON.stringify(terminals) === JSON.stringify(list)) return;
  terminals = list;
  locked = isLocked;
  if (!list) {
    notice = undefined;
    clear();
  }
  if (!started) {
    started = true;
    if (list) selectTerminal();
  }
  render();
};

const resize = new ResizeObserver(() => active?.fit());
resize.observe($('emulator'));
followLanguage(render);
send({ cmd: 'ready' });
