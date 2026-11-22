// Text from this device's input methods, typed on the computer: the one way text gets there.
// Every text box that takes it is bound here: the on-screen keyboard's (“Input method”, the phone
// keyboard) and the screen's own (keyboard.js: this device's input method and dictation). What
// an input method composes goes when the word is chosen; text put in at once goes as it is;
// line breaks and deleting go as Enter, Backspace and Delete. Modifiers tapped on the on-screen
// keyboard apply: a single character with them on is a combination (Ctrl+C), and so is the
// first letter of a word started with them on.
//
// Public surface: bindTextBox(box, s, { endWord }), resetBox(box), typeKey(s, code, extra),
// typeText(s, text).
import { buzz } from '../env.js';
import { CAPS, Control } from '../proto.js';
import { charMessage } from './input-messages.js';

// A little invisible text lets Backspace register even when nothing was typed.
const SENTINEL = '​​';

export function resetBox(box) {
  box.value = SENTINEL;
  box.setSelectionRange(SENTINEL.length, SENTINEL.length);
}

// One key with the on-screen keyboard's modifiers (and `extra`) held around it. Modifiers tapped
// once are then released.
export function typeKey(s, code, extra = []) {
  const send = (c, d) => s.send({ t: Control.KEY, c, d });
  const mods = [...new Set([...s.osk.mods.keys(), ...extra])];
  for (const m of mods) send(m, true);
  send(code, true);
  send(code, false);
  for (const m of [...mods].reverse()) send(m, false);
  s.osk.usedMods();
  buzz(5);
}

// Text, typed there as text. Line breaks become Enter. A single character with modifiers on is
// a combination (Ctrl+C): the computer finds its key with its own layout.
export function typeText(s, text) {
  const mods = s.osk.mods;
  if (mods.size && [...text].length === 1 && s.hasCap(CAPS.CHARS)) {
    s.send(charMessage(text, [...mods.keys()]));
    s.osk.usedMods();
    buzz(5);
    return;
  }
  const parts = text.replace(/\r\n?/g, '\n').split('\n');
  parts.forEach((p, i) => {
    if (i > 0) typeKey(s, 'Enter');
    if (p) s.send({ t: Control.TEXT, v: p });
  });
}

// `endWord()` ends a word whose first letter turned out to be a combination's key.
export function bindTextBox(box, s, { endWord = () => box.blur() } = {}) {
  let composing = false;
  let comboTaken = false;
  box.addEventListener('focus', () => resetBox(box));
  box.addEventListener('compositionstart', () => { composing = true; });
  // A modifier is down and the input method starts a word: its first letter is the key of the
  // combination (Ctrl+Shift+C), not the start of a word.
  box.addEventListener('compositionupdate', (e) => {
    if (!s.osk.mods.size || !e.data || comboTaken) return;
    const ch = e.data[0];
    if (!/[a-z0-9]/i.test(ch)) return;
    comboTaken = true;
    typeText(s, ch.toLowerCase());
    endWord();
  });
  box.addEventListener('compositionend', (e) => {
    composing = false;
    if (e.data && !comboTaken) typeText(s, e.data);
    comboTaken = false;
    resetBox(box);
  });
  box.addEventListener('input', (e) => {
    if (composing || e.isComposing) return;
    const typed = box.value.replaceAll('​', '');
    switch (e.inputType) {
      case 'insertLineBreak':
      case 'insertParagraph':
        typeKey(s, 'Enter');
        break;
      case 'deleteContentBackward':
        typeKey(s, 'Backspace');
        break;
      case 'deleteContentForward':
        typeKey(s, 'Delete');
        break;
      default:
        if (typed) typeText(s, typed);
    }
    resetBox(box);
  });
  box.addEventListener('blur', () => { box.value = ''; });
}
