// The page behaves like an app, not a document: nothing on it is selected, dragged out, or given
// the browser's long-press menu outside editable fields and copyable code. Held presses
// (a mouse button held through the mouse panel, a key held on the on-screen keyboard, a long
// press on a computer) are the app's own, never the browser's.
//
// One place for the whole page, with style.css (`user-select`, `-webkit-touch-callout`):
// components do not repeat it.
//
// Public surface: installAppFeel().

const SELECTABLE = 'input, textarea, [contenteditable=""], [contenteditable="true"], .copyable-code';

// The element an event is about: a selection starts on a text node.
function elementOf(target) {
  if (!target) return null;
  return target.nodeType === Node.ELEMENT_NODE ? target : target.parentElement;
}

const selectable = (target) => !!elementOf(target)?.closest(SELECTABLE);

export function installAppFeel() {
  // Capturing, on the document: before any component, and whether or not it stops the event.
  for (const kind of ['selectstart', 'dragstart', 'contextmenu']) {
    document.addEventListener(kind, (e) => { if (!selectable(e.target)) e.preventDefault(); }, true);
  }
}
