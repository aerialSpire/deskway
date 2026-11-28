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

function button(text, fn, attrs = {}) {
  const el = h('button', { class: 'btn compact', type: 'button', ...attrs }, text);
  el.onclick = fn;
  return el;
}

function render() {
  const rows = (terminals ?? []).map(({ id, attached, here, name }) => {
    const selected = active?.stream.id === id && active.stream.attached;
    const label = t('terminal.window_tab', { id });
    const suffix = attached && !here ? ' ' + t('terminal.window_using', { device: name }) : '';
    const select = () => {
      if (selected) active.emulator.focus();
      else if (attached && !here) ask('take', id, name, () => selectTerminal(id, true));
      else selectTerminal(id);
    };
    return h('span', { class: selected ? 'tab selected' : 'tab' },
      button(label + suffix, select, { 'aria-pressed': selected ? 'true' : 'false' }),
      selected ? button('×', () => ask('close', id, null, () => {
        if (active?.stream.id === id) active.stream.terminate();
      }), { 'aria-label': t('terminal.window_close_title', { id }) }) : null);
  });
  if (terminals) {
    rows.push(button('+', () => selectTerminal(null), { 'aria-label': t('terminal.window_new') }));
  }
  $('tabs').replaceChildren(...rows);
  $('tabs').hidden = !terminals;
  const message = started && !terminals ? (locked ? 'terminal.window_locked' : 'terminal.window_unavailable') : notice;
  $('status').textContent = message ? t(message) : '';
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
        if (message.dropped) notice = 'terminal.window_dropped';
        emulator.focus();
      } else if (message.t === TerminalMsg.ERROR && message.why === 'close') {
        notice = 'terminal.close_failed';
      } else {
        clear();
        if (message.t === TerminalMsg.ERROR) notice = 'terminal.open_failed';
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
