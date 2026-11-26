// Terminal input for browser key values and the on-screen US layout.
import { KEYBOARD_ROWS } from './keyboard-layout.js';

const US_KEYS = new Map(KEYBOARD_ROWS.flat().map(([label, code, shifted]) => [code, [label, shifted]]));
function usKey(code, shift, caps) {
  if (code === 'Space') return ' ';
  if (code.startsWith('Key')) return shift !== caps ? code.slice(3) : code.slice(3).toLowerCase();
  const [label, shifted] = US_KEYS.get(code) || [];
  return shifted ? (shift ? shifted : label) : code;
}

// Let the browser paste from the clipboard or commit text from a Mac Option key.
export function terminalNativeKey(e, mac) {
  const { code, key, ctrlKey, altKey, metaKey, shiftKey } = e;
  if (mac && altKey && !ctrlKey && !metaKey) return key === 'Dead' || e.keyCode === 229 || [...key].length === 1;
  if (altKey) return false;
  if (key?.toLowerCase() === 'v') return mac ? metaKey && !ctrlKey : ctrlKey && !metaKey;
  return !mac && code === 'Insert' && shiftKey && !ctrlKey && !metaKey;
}

const ESC = '\x1b';
const CURSOR_KEYS = { ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F' };
const FUNCTION_KEYS = { F1: 'P', F2: 'Q', F3: 'R', F4: 'S' };
const TILDE_KEYS = {
  Delete: 3, PageUp: 5, PageDown: 6,
  F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24,
};
const CONTROL_KEYS = { ' ': '\0', '3': ESC, '4': '\x1c', '5': '\x1d', '6': '\x1e', '7': '\x1f', '8': '\x7f', '?': '\x7f' };

// Hardware and committed input supply a character with its case and layout already applied.
// On-screen positions use the on-screen modifiers and Caps Lock to find their character.
export function terminalKey({ code = '', key, shift = false, ctrl, alt, meta, caps = false, applicationCursor }) {
  key ??= usKey(code, shift, caps);
  const prefix = alt ? ESC : '';
  switch (key) {
    case 'Enter': return prefix + '\r';
    case 'Tab': return shift ? ESC + '[Z' : '\t';
    case 'Backspace': return prefix + (ctrl ? '\b' : '\x7f');
    case 'Escape': return prefix + ESC;
    case 'Insert': return shift || ctrl ? '' : ESC + '[2~';
  }

  // xterm key sequences encode Shift, Alt, Ctrl and Meta as bits in the modifier parameter.
  const modifier = (shift ? 1 : 0) + (alt ? 2 : 0) + (ctrl ? 4 : 0) + (meta ? 8 : 0);
  const param = modifier ? ';' + (modifier + 1) : '';
  if (Object.hasOwn(CURSOR_KEYS, key) || Object.hasOwn(FUNCTION_KEYS, key)) {
    if (meta && key.startsWith('Arrow')) return '';
    const final = CURSOR_KEYS[key] || FUNCTION_KEYS[key];
    if (modifier) return ESC + '[1' + param + final;
    return ESC + (applicationCursor || FUNCTION_KEYS[key] ? 'O' : '[') + final;
  }
  if (Object.hasOwn(TILDE_KEYS, key)) {
    const page = key === 'PageUp' || key === 'PageDown';
    return ESC + '[' + TILDE_KEYS[key] + (page && !ctrl ? '' : param) + '~';
  }

  if (key === 'Dead' && alt && /^Key[A-Z]$/.test(code)) key = usKey(code, shift, caps);
  if (meta || [...key].length !== 1) return '';
  if (ctrl) {
    const ascii = key.toUpperCase().charCodeAt(0);
    if (/^[a-z]$/i.test(key) || '@[\\]^_'.includes(key)) return prefix + String.fromCharCode(ascii & 0x1f);
    return Object.hasOwn(CONTROL_KEYS, key) ? prefix + CONTROL_KEYS[key] : '';
  }
  return prefix + key;
}
