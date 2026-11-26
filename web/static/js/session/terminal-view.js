// A terminal in the shared session stage and toolbar.
import { h } from '../dom.js';
import { CONTROLLER_MAC } from '../env.js';
import { t } from '../i18n.js';
import { CAPS, Control, TerminalMsg as Msg } from '../proto.js';
import { MODIFIERS, charMessage, eventModifiers, isModifier } from './input-messages.js';
import { terminalKey, terminalNativeKey } from './terminal-keys.js';
import { TerminalChannel } from './terminal-channel.js';

const encoder = new TextEncoder();

export class TerminalView {
  constructor(s) {
    this.s = s;
    this.buttons = [s.osk.button];
    this.waiting = this.connectingNotice || 'terminal.starting';
    this.onscreenCapsLock = false;
    this.el = h('div', { class: 'terminal-surface' });
    this.status = h('div', { class: 'notice dark passive terminal-status', role: 'status', hidden: true });
    s.stage.prepend(this.el);
    s.stack.append(this.status);
  }

  get controlsReady() { return !!this.stream?.running; }
  get blocked() { return !this.controlsReady; }
  get keeps() {
    return this.s.caps ? this.s.hasCap(CAPS.TERMINAL_KEEP) : this.s.device.caps?.includes(CAPS.TERMINAL_KEEP);
  }
  get canClose() { return !!this.stream?.attached && !this.stream.closing; }
  get connectingNotice() { if (this.s.device.kept_terminal) return 'terminal.reconnecting'; }
  get recoveringNotice() {
    if (this.stream?.attached && this.keeps) return 'terminal.reconnecting';
  }

  bind() {
    this.el.addEventListener('pointerdown', () => this.s.stage.focus({ preventScroll: true }));
    this.s.root.addEventListener('paste', (e) => {
      if (!this.s.stage.contains(e.target) && e.target !== this.s.osk.typer) return;
      e.preventDefault();
      e.stopPropagation();
      const text = e.clipboardData?.getData('text/plain');
      if (text && !this.s.blocked) this.emulator.paste(text);
    }, true);
  }

  useChannels({ terminal }) {
    this.stream = new TerminalChannel(terminal, (bytes, done) => this.emulator.write(bytes, done), (m) => this.onStatus(m));
    terminal.onopen = () => this.active();
  }

  active() {
    if (this.s.stage.hidden || !this.s.connected || !this.s.hasCap(CAPS.TERMINAL) || this.stream?.channel.readyState !== 'open') return;
    if (!this.loading) this.loading = this.load();
    this.fit();
  }

  async load() {
    const style = h('link', { rel: 'stylesheet', href: new URL('../../vendor/xterm.css', import.meta.url) });
    this.style = style;
    const styled = new Promise((resolve, reject) => { style.onload = resolve; style.onerror = reject; });
    document.head.append(style);
    try {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import('../../vendor/xterm.mjs'), import('../../vendor/addon-fit.mjs'), styled,
      ]);
      if (this.s.closed || this.stream.phase === 'done') return;
      const css = getComputedStyle(this.el);
      const value = (token) => css.getPropertyValue(token).trim();
      const emulator = new Terminal({
        fontFamily: value('--font-mono'),
        fontSize: parseFloat(css.fontSize),
        theme: { background: value('--screen'), foreground: value('--dark-ink'), cursor: value('--dark-ink'), selectionBackground: value('--dark-key-down') },
      });
      this.emulator = emulator;
      this.fitter = new FitAddon();
      emulator.loadAddon(this.fitter);
      emulator.open(this.el);
      // The session's text boxes own keyboard focus and IME.
      emulator.textarea.inert = true;
      emulator.onData((text) => {
        if (!this.s.blocked) this.stream.input(encoder.encode(text));
      });
      this.resize = new ResizeObserver(() => this.fit());
      this.resize.observe(this.el);
      this.fit();
    } catch {
      this.stream.finish({ t: Msg.ERROR, why: 'load' });
    }
  }

  fit() {
    if (!this.fitter || !this.el.clientWidth || !this.el.clientHeight) return;
    this.fitter.fit();
    if (this.s.connected) this.stream.size(this.emulator.rows, this.emulator.cols);
  }

  keyboardSink() {
    return {
      send: (...args) => this.key(...args),
      acceptsModifiedText: true,
      nativeKey: (e) => terminalNativeKey(e, CONTROLLER_MAC),
      // Shift alone can start a composed word; shortcuts retain the typed case.
      compositionKey: (ch, mods) => [...mods.keys()].some((m) => !MODIFIERS.shift.includes(m)) ? ch : null,
    };
  }

  key(m, event, latched = []) {
    if (this.s.blocked) return false;
    if (m.t === Control.TEXT) this.emulator.input(m.v);
    else if (m.t === Control.CHAR) this.typeKey({ key: m.ch, ...m.mods });
    else if (m.t === Control.KEY && m.d && !isModifier(m.c)) {
      if (m.c === 'CapsLock') {
        if (!event) this.onscreenCapsLock = !this.onscreenCapsLock;
      } else {
        const held = charMessage('', [...this.s.keyboard.onscreen, ...latched]).mods;
        const physical = event ? eventModifiers(event) : this.s.keyboard.physicalMods;
        const altGraph = event?.getModifierState('AltGraph');
        this.typeKey({
          code: m.c, key: event?.key || undefined, caps: this.onscreenCapsLock,
          shift: held.shift || physical.shift, meta: held.meta || physical.meta,
          ctrl: held.ctrl || (!altGraph && physical.ctrl), alt: held.alt || (!altGraph && physical.alt),
        });
      }
    }
    return true;
  }

  typeKey(key) {
    const { code, shift, ctrl, alt, meta } = key;
    if (shift && (code === 'PageUp' || code === 'PageDown')) {
      this.emulator.scrollLines((this.emulator.rows - 1) * (code === 'PageUp' ? -1 : 1));
    } else if (CONTROLLER_MAC && meta && !shift && !ctrl && !alt && /^[a]$/i.test(key.key)) {
      this.emulator.selectAll();
    } else {
      const input = terminalKey({ ...key, applicationCursor: this.emulator.modes.applicationCursorKeysMode });
      if (input) this.emulator.input(input);
    }
  }

  onStatus(m) {
    if (m.t === Msg.CLOSED) { this.s.leave(); return; }
    if (!this.s.stage.hidden) this.s.ready();
    if (m.t === Msg.OPENED) {
      this.fit();
      if (m.dropped) this.showStatus(t('terminal.dropped'));
    } else {
      if (m.why !== 'close') this.s.osk.toggle(false);
      this.showStatus(m.t === Msg.EXIT
        ? t(m.code < 0 ? 'terminal.signal' : 'terminal.exit', { code: Math.abs(m.code) })
        : t(`terminal.error.${m.why}`));
    }
    this.s.updateRemoteControls();
  }

  showStatus(text) {
    this.status.textContent = text;
    this.status.hidden = false;
  }

  placeIme(area, stripTop) {
    this.place(area.x, area.y, area.x + area.w, stripTop);
  }

  // Keep the terminal in the larger of the spaces left by the floating keyboard.
  placeKeypad(pad) {
    if (!pad) { this.el.style.inset = ''; return; }
    const stage = this.s.stage.getBoundingClientRect();
    const key = pad.getBoundingClientRect();
    if (key.top - stage.top >= stage.bottom - key.bottom) this.place(stage.left, stage.top, stage.right, key.top);
    else this.place(stage.left, key.bottom, stage.right, stage.bottom);
  }

  place(left, top, right, bottom) {
    const stage = this.s.stage.getBoundingClientRect();
    this.el.style.inset = [top - stage.top, stage.right - right, stage.bottom - bottom, left - stage.left]
      .map((gap) => `${Math.max(0, gap)}px`).join(' ');
  }

  terminate() {
    if (this.stream?.terminate()) {
      this.s.setWaiting(t('terminal.closing'));
      this.s.updateRemoteControls();
    }
  }

  destroy() {
    this.stream?.dispose();
    this.resize?.disconnect();
    this.emulator?.dispose();
    this.style?.remove();
  }
}
