// [label, code, label with Shift, width]. Laid out like a laptop's. A null label is a word,
// looked up when the keyboard is drawn (`kb.key.<code>`).
const letters = (row) => [...row].map((c) => [c, `Key${c}`]);
export const KEYBOARD_ROWS = [
  [['Esc', 'Escape'], ...Array.from({ length: 12 }, (_, i) => [`F${i + 1}`, `F${i + 1}`]), ['Del', 'Delete']],
  [['`', 'Backquote', '~'], ...[...'1234567890'].map((d, i) => [d, `Digit${d}`, '!@#$%^&*()'[i]]), ['-', 'Minus', '_'], ['=', 'Equal', '+'], ['⌫', 'Backspace', null, 1.8]],
  [['Tab', 'Tab', null, 1.5], ...letters('QWERTYUIOP'), ['[', 'BracketLeft', '{'], [']', 'BracketRight', '}'], ['\\', 'Backslash', '|', 1.3]],
  [['Caps', 'CapsLock', null, 1.8], ...letters('ASDFGHJKL'), [';', 'Semicolon', ':'], ["'", 'Quote', '"'], ['Enter', 'Enter', null, 2]],
  [['Shift', 'ShiftLeft', null, 2.3], ...letters('ZXCVBNM'), [',', 'Comma', '<'], ['.', 'Period', '>'], ['/', 'Slash', '?'], ['Shift', 'ShiftRight', null, 1.5], ['↑', 'ArrowUp']],
  [['Ctrl', 'ControlLeft', null, 1.3], ['Win', 'MetaLeft', null, 1.1], ['Alt', 'AltLeft', null, 1.1], [null, 'Space', null, 4.5], ['Alt', 'AltRight', null, 1.1], ['Ctrl', 'ControlRight', null, 1.1], ['←', 'ArrowLeft'], ['↓', 'ArrowDown'], ['→', 'ArrowRight']],
];
