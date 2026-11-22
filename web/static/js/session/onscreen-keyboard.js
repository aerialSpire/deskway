// The on-screen keyboard: a whole computer keyboard, drawn here. Every key is a real key: it
// goes down when the finger lands and up when it lifts, and several can be held at once (Ctrl
// and Shift with one hand, T with the other). Nothing goes through the phone's input method,
// which cannot send such combinations; “Input method” opens it only for typing words, through its
// text box (text-box.js, as every input method's text).
//
// The keys and the system keyboard take turns (`setIme`): while the system keyboard is visible the
// keys step aside, leaving the bar (“Input method” pressed, what is being composed) above it; when it
// goes (“Input method” again, the text box losing the focus, or the phone keyboard folded away) the
// keys come back and “Input method” is released.
//
// Public surface: button, keypad (null while closed), mods (code -> 1 for the next key,
// 2 locked), usedMods(), typer, ime, toggle(open, { auto }), setIme(on), noteHardwareKeyboard(),
// release(), layout(), setLayout(m) (the computer's key labels; until then, the labels of a US
// keyboard).
import { h, ICONS, keepEvents } from '../dom.js';
import { store, buzz, orientation } from '../env.js';
import { t } from '../i18n.js';
import { Control } from '../proto.js';
import { layoutLabel } from './input-messages.js';
import { bindTextBox, resetBox, typeKey } from './text-box.js';
import { draggable, savedPlace, savePlace } from './floating.js';
import { keyboardArea } from './ime-viewport.js';

// [label, code, label with Shift, width]. Laid out like a laptop's. A null label is a word,
// looked up when the keyboard is drawn (`kb.key.<code>`).
const letters = (row) => [...row].map((c) => [c, `Key${c}`]);
const KEYBOARD_ROWS = [
  [['Esc', 'Escape'], ...Array.from({ length: 12 }, (_, i) => [`F${i + 1}`, `F${i + 1}`]), ['Del', 'Delete']],
  [['`', 'Backquote', '~'], ...[...'1234567890'].map((d, i) => [d, `Digit${d}`, '!@#$%^&*()'[i]]), ['-', 'Minus', '_'], ['=', 'Equal', '+'], ['⌫', 'Backspace', null, 1.8]],
  [['Tab', 'Tab', null, 1.5], ...letters('QWERTYUIOP'), ['[', 'BracketLeft', '{'], [']', 'BracketRight', '}'], ['\\', 'Backslash', '|', 1.3]],
  [['Caps', 'CapsLock', null, 1.8], ...letters('ASDFGHJKL'), [';', 'Semicolon', ':'], ["'", 'Quote', '"'], ['Enter', 'Enter', null, 2]],
  [['Shift', 'ShiftLeft', null, 2.3], ...letters('ZXCVBNM'), [',', 'Comma', '<'], ['.', 'Period', '>'], ['/', 'Slash', '?'], ['Shift', 'ShiftRight', null, 1.5], ['↑', 'ArrowUp']],
  [['Ctrl', 'ControlLeft', null, 1.3], ['Win', 'MetaLeft', null, 1.1], ['Alt', 'AltLeft', null, 1.1], [null, 'Space', null, 4.5], ['Alt', 'AltRight', null, 1.1], ['Ctrl', 'ControlRight', null, 1.1], ['←', 'ArrowLeft'], ['↓', 'ArrowDown'], ['→', 'ArrowRight']],
];
const MOD_KEYS = new Set(['ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight', 'MetaLeft']);
// Keys whose spoken name is a word (`kb.key.<code>`); the others are called by their label.
const KEY_NAMES = new Set(['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Backspace', 'Delete', 'PageUp', 'PageDown', 'Space', 'CapsLock', 'ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight']);
// On a Mac: [label, name], the name a word for the Option keys (`kb.key.mac<code>`).
const MAC_KEY_LABELS = { MetaLeft: ['⌘', 'Command'], AltLeft: ['⌥', null], AltRight: ['⌥', null] };

export class OnscreenKeyboard {
  constructor(s) {
    this.s = s;
    this.keypad = null;
    this.mods = new Map();
    this.fingers = null;
    // The computer's layout ({labels, shift}), when it has said.
    this.hostLayout = null;
    // The local input method has been requested; `imeShown` tracks the keyboard on screen.
    this.ime = false;
    this.imeShown = false;
    this.button = h('button', { class: 'tool', 'aria-pressed': 'false', onclick: () => this.toggle() }, t('tb.keyboard'));
  }

  toggle(open, { auto = false } = {}) {
    const show = open ?? !this.keypad;
    if (show && !this.keypad) {
      this.keypad = this.build();
      this.auto = auto;
      this.s.root.append(this.keypad);
    } else if (!show && this.keypad) {
      this.stopIme();
      this.release();
      this.keypad.remove();
      this.keypad = null;
      this.mods.clear();
    }
    this.button.setAttribute('aria-pressed', String(!!this.keypad));
    // The screen and the mouse panel make room for the keyboard.
    this.s.layout();
  }

  // A key pressed on a physical keyboard: this device has one. The on-screen keyboard, if it
  // came up by itself, goes away; next time it does not come up.
  noteHardwareKeyboard() {
    if (store.get('deskway.hwkbd') !== '1') store.set('deskway.hwkbd', '1');
    if (this.keypad && this.auto) this.toggle(false);
  }

  // Request the system keyboard inside the tap that opened it. Save the picture before focus
  // can move or resize the viewport.
  setIme(on) {
    if (!this.keypad || on === this.ime) return;
    if (on) {
      this.ime = true;
      this.imeStart = visibleArea();
      this.s.video.beginIme();
      this.typer.focus({ preventScroll: true });
    } else {
      this.stopIme();
    }
    this.s.layout();
  }

  stopIme() {
    if (!this.ime) return;
    this.ime = false;
    this.imeShown = false;
    this.imeStart = null;
    if (document.activeElement === this.typer) {
      this.typer.blur();
      this.s.stage.focus({ preventScroll: true });
    }
  }

  build() {
    this.typer = h('textarea', { class: 'typer', autocapitalize: 'off', autocomplete: 'off', autocorrect: 'off', spellcheck: 'false', 'aria-label': t('kb.typerLabel') });
    bindTextBox(this.typer, this.s);
    this.bindTyperKeys(this.typer);
    this.preview = h('div', { class: 'kb-preview', hidden: true, 'aria-live': 'polite' });
    this.typer.addEventListener('compositionupdate', (e) => {
      this.preview.textContent = e.data || '';
      this.preview.hidden = !e.data;
    });
    this.typer.addEventListener('compositionend', () => { this.preview.hidden = true; });
    this.typer.addEventListener('blur', () => this.setIme(false));

    this.keys = new Map(); // code -> elements showing it
    const key = ([baseLabel, code, shifted, wide]) => {
      const defaultLabel = baseLabel ?? t(`kb.key.${code}`);
      const mac = this.s.device.os === 'macos' && MAC_KEY_LABELS[code];
      const [label, name] = mac
        ? [mac[0], mac[1] ?? t(`kb.key.mac${code}`)]
        : [defaultLabel, KEY_NAMES.has(code) ? t(`kb.key.${code}`) : defaultLabel];
      const el = h('button', { class: `kb-key${MOD_KEYS.has(code) ? ' mod' : ''}`, type: 'button', 'data-code': code, 'aria-label': name, tabindex: '-1' });
      el.defaults = { label, shifted, named: KEY_NAMES.has(code) || !!mac };
      this.paint(el);
      if (wide) el.style.flexGrow = String(wide);
      if (!this.keys.has(code)) this.keys.set(code, []);
      this.keys.get(code).push(el);
      return el;
    };
    // The bar above the keys: drag it to move the keyboard; “Input method” beside the grip, what the
    // input method is composing in the middle, Hide keyboard at the right.
    const grip = h('div', { class: 'kb-grip', 'aria-hidden': 'true' }, h('span'), h('span'), h('span'));
    const hide = h('button', { class: 'kb-hide', type: 'button', 'aria-label': t('kb.hide'), title: t('kb.hide'), tabindex: '-1', onclick: () => this.toggle(false) });
    hide.innerHTML = ICONS.hideKeyboard;
    const tip = h('div', { class: 'kb-tip', id: 'kb-ime-tip', role: 'tooltip', hidden: true }, t('kb.imeTip'));
    this.imeKey = h('button', { class: 'kb-ime', type: 'button', 'aria-pressed': 'false', 'aria-describedby': 'kb-ime-tip', tabindex: '-1', onclick: () => {
      tip.hidden = true;
      this.setIme(!this.ime);
    } }, t('kb.ime'));
    // Explained under the mouse, at once: a computer has its input method already, and the
    // button only matters where a tap brings the system keyboard up.
    this.imeKey.addEventListener('pointerenter', (e) => { if (e.pointerType === 'mouse') tip.hidden = false; });
    this.imeKey.addEventListener('pointerleave', () => { tip.hidden = true; });
    const bar = h('div', { class: 'kb-bar', 'aria-label': t('kb.barLabel') }, grip, h('div', { class: 'kb-ime-wrap' }, this.imeKey, tip), this.preview, hide);
    const rows = KEYBOARD_ROWS.map((row) => h('div', { class: 'kb-row' }, ...row.map(key)));
    const pad = h('div', { class: 'keypad', role: 'group', 'aria-label': t('tb.keyboard') }, this.typer, bar, ...rows);

    // One entry per finger on a key, so several keys can be down at once.
    this.fingers = new Map(); // pointerId -> { code, el, applied, timer }
    pad.addEventListener('pointerdown', (e) => {
      e.stopPropagation();
      const el = e.target.closest('.kb-key[data-code]');
      // Keys never take the focus: it stays with the screen (or the input method's text box).
      if (e.target.closest('button')) e.preventDefault();
      if (!el || this.s.blocked) return;
      this.s.keyboard.noteEvent(e);
      try { el.setPointerCapture(e.pointerId); } catch { /* fine */ }
      this.keyDown(e.pointerId, el);
    });
    const up = (e) => {
      if (!this.fingers.has(e.pointerId)) return;
      e.stopPropagation();
      this.keyUp(e.pointerId);
    };
    pad.addEventListener('pointerup', up);
    pad.addEventListener('pointercancel', up);
    pad.addEventListener('lostpointercapture', up);
    keepEvents(pad, ['pointermove', 'touchstart', 'contextmenu', 'wheel']);
    pad.addEventListener('mousedown', (e) => { if (e.target.closest('button')) e.preventDefault(); });

    // Moved up or down by its bar (not by the bar's buttons); the place is remembered.
    let top0 = 0;
    draggable(bar, {
      axis: 'y',
      ignore: (e) => this.imeShown || !!e.target.closest('button'),
      start: () => { top0 = pad.getBoundingClientRect().top - visibleArea().y; },
      move: (dx, dy) => {
        const v = visibleArea();
        const r = pad.getBoundingClientRect();
        const y = Math.min(v.h - r.height, Math.max(0, top0 + dy));
        pad.style.top = `${v.y + y}px`;
        pad.place = { x: 0.5, y: y / Math.max(1, v.h - r.height) };
      },
      end: () => {
        if (pad.place) savePlace('kb', pad.place);
        this.s.layout();
      },
    });
    this.syncModifiers(pad);
    return pad;
  }

  // A key's face: the computer's label for it when known, else the default one.
  paint(el) {
    const d = el.defaults;
    const [label, shifted] = layoutLabel(this.hostLayout, el.dataset.code) || [d.label, d.shifted];
    const main = h('span', { class: 'kb-main' }, label);
    if (shifted) el.replaceChildren(h('span', { class: 'kb-alt' }, shifted), main);
    else el.replaceChildren(main);
    if (!d.named) el.setAttribute('aria-label', label);
  }

  // The computer's keyboard layout: its keys are drawn as they type there.
  setLayout(m) {
    this.hostLayout = { labels: m.labels || {}, shift: m.shift || {} };
    for (const els of this.keys?.values() || []) for (const el of els) this.paint(el);
  }

  keyDown(finger, el) {
    const code = el.dataset.code;
    if (MOD_KEYS.has(code)) {
      // Held with another key: down on the remote computer, like a real one. Tapped alone: it
      // applies to the next key (tapped twice quickly: it stays on), and the remote computer
      // sees nothing, so a lone Win does not open Start. Held alone a moment, it goes down
      // too (Shift while clicking).
      const was = this.mods.get(code) || 0;
      this.mods.delete(code);
      const f = { code, el, mod: true, was, used: false, sent: false };
      f.timer = setTimeout(() => {
        if (this.fingers.get(finger) !== f || f.sent) return;
        f.sent = true;
        this.press(code, true);
      }, 400);
      this.fingers.set(finger, f);
    } else {
      for (const f of this.fingers.values()) {
        if (!f.mod) continue;
        f.used = true;
        if (!f.sent) { f.sent = true; clearTimeout(f.timer); this.press(f.code, true); }
      }
      const held = new Set([...this.fingers.values()].map((f) => f.code));
      const applied = [...this.mods.keys()].filter((m) => !held.has(m));
      for (const m of applied) this.press(m, true);
      this.press(code, true);
      // Held long, it repeats, as a real keyboard does (Backspace, arrows, letters).
      const timer = code === 'CapsLock' ? null : setTimeout(function repeat() {
        if (!this.fingers.has(finger)) return;
        this.s.send({ t: Control.KEY, c: code, d: true });
        this.fingers.get(finger).timer = setTimeout(repeat.bind(this), 60);
      }.bind(this), 500);
      this.fingers.set(finger, { code, el, mod: false, applied, timer });
    }
    el.classList.add('down');
    buzz(5);
    this.syncModifiers();
  }

  keyUp(finger) {
    const f = this.fingers.get(finger);
    if (!f) return;
    this.fingers.delete(finger);
    clearTimeout(f.timer);
    f.el.classList.remove('down');
    if (f.mod) {
      if (f.sent) this.press(f.code, false);
      if (!f.used && !f.sent) {
        const now = performance.now();
        const twice = this.lastModTap && this.lastModTap.code === f.code && now - this.lastModTap.t < 400;
        this.lastModTap = { code: f.code, t: now };
        const next = f.was === 0 ? 1 : (f.was === 1 && twice ? 2 : 0);
        if (next) this.mods.set(f.code, next);
      }
    } else {
      this.press(f.code, false);
      for (const m of [...f.applied].reverse()) {
        this.press(m, false);
        if (this.mods.get(m) === 1) this.mods.delete(m);
      }
    }
    this.syncModifiers();
  }

  press(code, down) {
    this.s.keyboard.press(code, down);
  }

  // Lets go of every key held on the screen (the keyboard closes, the page is left).
  release() {
    if (!this.fingers) return;
    for (const finger of [...this.fingers.keys()]) this.keyUp(finger);
  }

  // Shows which modifiers are on, and what the keys type with Shift.
  syncModifiers(pad = this.keypad) {
    if (!this.keys) return;
    const held = new Set(this.fingers ? [...this.fingers.values()].map((f) => f.code) : []);
    for (const code of MOD_KEYS) {
      const st = held.has(code) ? 3 : this.mods.get(code) || 0;
      for (const el of this.keys.get(code) || []) {
        el.setAttribute('aria-pressed', st ? 'true' : 'false');
        el.classList.toggle('locked', st === 2);
      }
    }
    const shift = held.has('ShiftLeft') || held.has('ShiftRight') || this.mods.has('ShiftLeft') || this.mods.has('ShiftRight');
    pad?.classList.toggle('shift', shift);
  }

  layout() {
    const pad = this.keypad;
    const view = visibleArea();
    const area = this.ime && keyboardArea(view, document.documentElement.clientHeight,
      this.imeStart, navigator.virtualKeyboard?.boundingRect);
    if (!area && this.imeShown) this.stopIme();
    this.imeShown = !!area;

    if (pad) {
      pad.classList.toggle('ime', this.imeShown);
      this.imeKey.setAttribute('aria-pressed', String(this.ime));
      const v = area || view;
      pad.style.left = `${v.x}px`;
      pad.style.width = `${v.w}px`;
      const height = pad.getBoundingClientRect().height;
      const room = Math.max(0, v.h - height);
      // The compact bar belongs to the viewport. Only the full keypad remembers a drag.
      const saved = area ? null : savedPlace('kb');
      const fraction = Number.isFinite(saved?.y) ? saved.y : 1;
      pad.style.top = `${v.y + fraction * room}px`;
      if (area) {
        this.s.video.placeIme(area, v.y + room);
        return;
      }
    }

    const upright = orientation() === 'portrait';
    this.s.root.style.paddingBottom = pad && upright ? `${pad.offsetHeight + 8}px` : '';
    if (!this.ime) this.s.video.endIme();
  }

  // The modifiers tapped once have been used by a key or a character: they let go.
  usedMods() {
    for (const [m, st] of this.mods) if (st === 1) this.mods.delete(m);
    this.syncModifiers();
  }

  // Keys a physical keyboard presses while the text box has the focus (it is not the screen's):
  // those that type no text, and combinations.
  bindTyperKeys(ta) {
    ta.addEventListener('keydown', (e) => {
      if (e.isComposing || e.keyCode === 229) return;
      const special = ['Enter', 'Backspace', 'Tab', 'Escape', 'Delete', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown'];
      const extra = [e.ctrlKey && 'ControlLeft', e.altKey && 'AltLeft', e.metaKey && 'MetaLeft'].filter(Boolean);
      if (special.includes(e.key) || (extra.length && e.code)) {
        e.preventDefault();
        if (e.shiftKey && extra.length) extra.push('ShiftLeft');
        typeKey(this.s, special.includes(e.key) ? (e.key === 'Enter' ? 'Enter' : e.code || e.key) : e.code, extra);
        resetBox(ta);
      }
    });
  }
}

// The part of the layout viewport the browser currently shows.
function visibleArea() {
  const vv = window.visualViewport;
  return vv
    ? { x: vv.offsetLeft, y: vv.offsetTop, w: vv.width, h: vv.height, scale: vv.scale }
    : { x: 0, y: 0, w: innerWidth, h: innerHeight, scale: 1 };
}
