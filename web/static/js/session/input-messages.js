// The input messages and the decisions behind them, pure (no page state), so they are tested
// on their own (tests/unit/input.test.mjs) against proto/messages.json.
import { Control, Move } from '../proto.js';

// Modifier keys by the state they set.
export const MODIFIERS = Object.freeze({
  shift: ['ShiftLeft', 'ShiftRight'],
  ctrl: ['ControlLeft', 'ControlRight'],
  alt: ['AltLeft', 'AltRight'],
  meta: ['MetaLeft', 'MetaRight'],
});
const MODIFIER_CODES = new Set(Object.values(MODIFIERS).flat());
export const isModifier = (code) => MODIFIER_CODES.has(code);

const modifierOn = (e, name) => !!e?.getModifierState?.(name);

export const eventModifiers = (e) => ({
  shift: modifierOn(e, 'Shift'), ctrl: modifierOn(e, 'Control'),
  alt: modifierOn(e, 'Alt'), meta: modifierOn(e, 'Meta'),
});

// Modifiers held through these codes (keys down here, on any keyboard of this page).
function modsOfCodes(codes) {
  const held = new Set(codes);
  return Object.fromEntries(Object.entries(MODIFIERS).map(([k, list]) => [k, list.some((c) => held.has(c))]));
}

// `sync` from an event's modifier state (KeyboardEvent, PointerEvent: getModifierState), with
// the modifiers this page holds down itself (the on-screen keyboard) added: those are down there
// too, whatever the physical keyboard says.
export function syncMessage(e, heldCodes = []) {
  const physical = eventModifiers(e);
  const held = modsOfCodes(heldCodes);
  return {
    t: Control.SYNC,
    locks: { caps: modifierOn(e, 'CapsLock'), num: modifierOn(e, 'NumLock'), scroll: modifierOn(e, 'ScrollLock') },
    mods: { shift: physical.shift || held.shift, ctrl: physical.ctrl || held.ctrl, alt: physical.alt || held.alt, meta: physical.meta || held.meta },
  };
}

// A character with the modifiers held, for the computer to type with its own layout.
export const charMessage = (ch, heldCodes) => ({ t: Control.CHAR, ch, mods: modsOfCodes(heldCodes) });

// Browsers on a Mac deliver no keyup for keys let go while Command is held: when Command goes
// up, those keys are released here (as noVNC and Guacamole do). Modifiers keep their own keyups.
export const releasedWithCommand = (pressed) => [...pressed].filter((c) => !isModifier(c));

// A pointer report is older than this page's own moves when it carries an earlier sequence, or
// when a move is about to go out.
export const reportIsStale = (reportSeq, lastSent, pending) => pending || !(reportSeq >= lastSent);

export const moveTo = (x, y, s) => ({ t: Move.TO, x, y, s });
export const moveBy = (dx, dy, s) => ({ t: Move.BY, dx, dy, s });

// A key's label from the computer's layout: [main, with Shift] or null for the default label.
// Letters show their capital alone, as printed on keyboards.
export function layoutLabel(layout, code) {
  const base = layout && layout.labels && layout.labels[code];
  if (!base) return null;
  const shifted = layout.shift && layout.shift[code];
  if (shifted && shifted !== base && shifted === base.toUpperCase() && base === base.toLowerCase()) return [shifted, null];
  return [base, shifted && shifted !== base ? shifted : null];
}
