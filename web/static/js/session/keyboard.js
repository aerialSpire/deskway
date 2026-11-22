// Keys from a physical keyboard on the stage. Keys travel as positions (`KeyboardEvent.code`),
// exactly as pressed: nothing is remapped, and no key (paste keys included) means anything here.
// Toggle keys and modifiers are synchronized with the computer (`sync`): after the session
// starts and whenever the page comes back to the front, from the first event that carries
// modifier state, and when an event exposes a missed modifier release.
//
// This device's input method and dictation type there too. Once a physical keyboard has been
// used here, the screen's focus is a text box of its own (`box`, bound as every text box is:
// text-box.js), unseen until something is being composed in it, at the last place pressed. Keys
// the input method takes come back from it as text, typed there as text; every other key still
// goes as its position, and combinations with Ctrl, Alt or Command always do. Without a physical
// keyboard the screen keeps the focus itself: focusing a text box could bring up the system's
// on-screen keyboard (the on-screen keyboard's “Input method” does that, on purpose).
//
// Public surface: pressed (physical keys held here), press(code, down), release(),
// needSync(), noteEvent(e), macCommand (keys let go under Command get no keyup), box, bind(),
// destroy().
import { h } from '../dom.js';
import { CONTROLLER_MAC, store } from '../env.js';
import { t } from '../i18n.js';
import { Control } from '../proto.js';
import { MODIFIERS, eventModifiers, syncMessage, isModifier, releasedWithCommand } from './input-messages.js';
import { bindTextBox } from './text-box.js';

// The input method here takes this key: what it makes arrives in the text box.
const imeTakes = (e) => e.isComposing || (e.keyCode === 229 && !e.ctrlKey && !e.altKey && !e.metaKey);

export class Keyboard {
  constructor(s) {
    this.s = s;
    this.pressed = new Set();
    this.onscreen = new Set();
    this.physicalMods = eventModifiers();
    // The modifiers the last sync told the computer to hold: pressed there without a key here.
    this.syncedMods = eventModifiers();
    this.hinted = false;
    this.syncPending = true;
    // A fact of the browsers on a Mac, not of the device: see releasedWithCommand.
    this.macCommand = CONTROLLER_MAC;
  }

  bind() {
    const s = this.s;
    const stage = s.stage;
    this.bindBox();
    stage.addEventListener('keydown', (e) => {
      if ((e.target === this.box ? imeTakes(e) : e.isComposing) || !e.code) return;
      if (s.blocked) {
        e.preventDefault();
        return;
      }
      s.osk.noteHardwareKeyboard();
      if (document.activeElement === stage) this.focusBox();
      this.hintSystemKeys(e);
      e.preventDefault();
      this.noteEvent(e);
      this.changeKey(e.code, true, this.pressed, this.onscreen);
    });
    stage.addEventListener('keyup', (e) => {
      if (!e.code) return;
      e.preventDefault();
      this.repairModifiers(e);
      // A key that never went down there (pressed before the screen had the focus) is not
      // lifted there; a modifier like that may be down there, so the state is synchronized.
      if (!this.pressed.has(e.code)) {
        if (isModifier(e.code)) this.sendSync(e);
        return;
      }
      this.changeKey(e.code, false, this.pressed, this.onscreen);
      if (this.macCommand && (e.code === 'MetaLeft' || e.code === 'MetaRight')) {
        this.releaseKeysUnderCommand();
      }
    });
    // The keyboard leaves the screen, or comes to it (not between the screen and its text box).
    // Coming to the screen itself, it goes on to the text box.
    stage.addEventListener('focusout', (e) => { if (!stage.contains(e.relatedTarget)) s.releaseAll(); });
    stage.addEventListener('focusin', (e) => {
      if (!stage.contains(e.relatedTarget)) this.needSync();
      if (e.target === stage) this.focusBox();
    });
  }

  bindBox() {
    const stage = this.s.stage;
    const box = h('textarea', { class: 'text-box', rows: '1', tabindex: '-1', inputmode: 'none', autocapitalize: 'off', autocomplete: 'off', autocorrect: 'off', spellcheck: 'false', 'aria-label': t('session.textBox') });
    this.box = box;
    stage.append(box);
    // The input method's window opens where the person last pressed: on the computer, that is
    // usually where the text goes.
    let at = { x: 0, y: 0 };
    const place = () => {
      box.style.left = `${Math.max(0, Math.min(at.x, stage.clientWidth - box.offsetWidth))}px`;
      box.style.top = `${Math.max(0, Math.min(at.y, stage.clientHeight - box.offsetHeight))}px`;
    };
    stage.addEventListener('pointerdown', (e) => {
      const r = stage.getBoundingClientRect();
      at = { x: e.clientX - r.left, y: e.clientY - r.top };
      place();
    });
    box.addEventListener('compositionstart', () => {
      box.classList.add('composing');
      place();
    });
    box.addEventListener('compositionend', () => box.classList.remove('composing'));
    // A word ended early (its first letter was a combination's key): the focus stays on the screen.
    bindTextBox(box, this.s, { endWord: () => { box.blur(); stage.focus({ preventScroll: true }); } });
  }

  // The screen's focus goes to its text box, once a physical keyboard has been used here.
  focusBox() {
    if (store.get('deskway.hwkbd') === '1') this.box.focus({ preventScroll: true });
  }

  // The page came to the front (or the session started): the state is read from the next event.
  needSync() {
    this.syncPending = true;
  }

  // Synchronize on focus, and use new presses to recover missed physical releases.
  noteEvent(e) {
    if (e.type !== 'pointermove') this.repairModifiers(e);
    if (this.syncPending) this.sendSync(e);
  }

  // Not open yet: the next event with modifier state synchronizes instead.
  sendSync(e) {
    const m = syncMessage(e, this.onscreen);
    if (!this.s.send(m)) {
      this.syncPending = true;
      return;
    }
    this.syncPending = false;
    this.physicalMods = eventModifiers(e);
    this.syncedMods = m.mods;
  }

  // System shortcuts can swallow keyups without moving focus. Release those keys before the
  // next input, including modifiers synchronized when a keydown never reached the stage.
  repairModifiers(e) {
    const actual = eventModifiers(e);
    const missing = Object.keys(MODIFIERS).filter((name) => this.physicalMods[name] && !actual[name]
      && !(e.type === 'keyup' && MODIFIERS[name].includes(e.code)));
    this.physicalMods = actual;
    if (!missing.length) return;
    for (const name of missing) {
      for (const code of MODIFIERS[name]) {
        if (this.pressed.has(code)) this.changeKey(code, false, this.pressed, this.onscreen);
      }
    }
    if (this.macCommand && missing.includes('meta')) this.releaseKeysUnderCommand();
    this.sendSync(e);
  }

  releaseKeysUnderCommand() {
    for (const code of releasedWithCommand(this.pressed)) this.changeKey(code, false, this.pressed, this.onscreen);
  }

  // A key down or up from the on-screen keyboard.
  press(code, down) {
    this.changeKey(code, down, this.onscreen, this.pressed);
  }

  changeKey(code, down, own, other) {
    if (down) own.add(code);
    else own.delete(code);
    if (!other.has(code)) this.s.send({ t: Control.KEY, c: code, d: down });
  }

  // The page lost the keyboard: the physical keys go up there, and so do modifiers a sync had
  // pressed there with no key here (a missed release), unless the on-screen keyboard holds them.
  // The on-screen keyboard lets go independently when its pointers are cancelled.
  release() {
    for (const code of this.pressed) this.changeKey(code, false, this.pressed, this.onscreen);
    for (const [name, codes] of Object.entries(MODIFIERS)) {
      if (!this.syncedMods[name] || codes.some((c) => this.onscreen.has(c))) continue;
      for (const code of codes) this.s.send({ t: Control.KEY, c: code, d: false });
    }
    this.syncedMods = eventModifiers();
    this.physicalMods = eventModifiers();
  }

  // Win, Alt+Tab and the like are taken by this computer unless the keyboard is locked, which
  // browsers allow only in full screen. Said once per session, when it happens.
  hintSystemKeys(e) {
    if (this.hinted || this.s.toolbar.isFull) return;
    const system = e.code === 'MetaLeft' || e.code === 'MetaRight' || (e.altKey && (e.code === 'Tab' || e.code === 'F4')) || (e.ctrlKey && ['KeyW', 'KeyT', 'KeyN', 'Tab'].includes(e.code));
    if (!system) return;
    this.hinted = true;
    this.showHint(t('session.systemKeys', { key: CONTROLLER_MAC ? '⌘' : 'Win' }));
  }

  showHint(text) {
    const drop = () => { this.hintEl?.remove(); this.hintEl = null; };
    drop();
    clearTimeout(this.hintTimer);
    this.hintEl = h('div', { class: 'notice dark keys-hint', role: 'status' }, h('span', { class: 'text' }, text), h('button', { class: 'x-btn', type: 'button', 'aria-label': t('common.close'), onclick: drop }, '✕'));
    this.s.stack.append(this.hintEl);
    this.hintTimer = setTimeout(drop, 8000);
  }

  destroy() {
    clearTimeout(this.hintTimer);
  }
}
